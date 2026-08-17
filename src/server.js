#!/usr/bin/env node

import { McpServer } from "@modelcontextprotocol/sdk/server/mcp.js";
import { StdioServerTransport } from "@modelcontextprotocol/sdk/server/stdio.js";
import { z } from "zod";
import { createFilesystemService, FilesystemError } from "./filesystem.js";

const DEFAULT_WORK_ROOT = "/Users/USER/Documents/Claude/Projects/Work (local)";
const DEFAULT_COWORK_ROOT = "/Users/USER/Documents/Claude/Projects/Cowork (local)";

const service = createFilesystemService({
  work: process.env.MCP_WORK_ROOT || DEFAULT_WORK_ROOT,
  cowork: process.env.MCP_COWORK_ROOT || DEFAULT_COWORK_ROOT
});

const server = new McpServer(
  { name: "strigov-local-files", version: "1.0.0" },
  {
    instructions:
      "Read-only access to two local document roots: work and cowork. Paths are always relative to a selected root. Start with list_roots or search. Prefer Markdown/text sidecars for PDF, DOCX, XLSX and scans. Never claim a write succeeded: this server exposes no write tools."
  }
);

const annotations = {
  readOnlyHint: true,
  destructiveHint: false,
  idempotentHint: true,
  openWorldHint: false
};

const rootSchema = z.enum(["work", "cowork"]);
const searchableRootSchema = z.enum(["work", "cowork", "all"]);

function textResult(data, summary) {
  return {
    structuredContent: data,
    content: [{ type: "text", text: summary }]
  };
}

function errorResult(error) {
  const code = error instanceof FilesystemError ? error.code : "INTERNAL_ERROR";
  const message = error instanceof Error ? error.message : String(error);
  return {
    isError: true,
    structuredContent: { error: { code, message } },
    content: [{ type: "text", text: `${code}: ${message}` }]
  };
}

function register(name, config, handler) {
  server.registerTool(name, { ...config, annotations }, async (input) => {
    try {
      return await handler(input);
    } catch (error) {
      return errorResult(error);
    }
  });
}

register(
  "list_roots",
  {
    title: "List allowed folders",
    description: "List the two read-only local folder aliases available to ChatGPT.",
    inputSchema: {},
    outputSchema: { roots: z.array(z.object({ name: z.string(), path: z.string(), uri: z.string(), readOnly: z.boolean() })) }
  },
  async () => {
    const roots = await service.listRoots();
    return textResult({ roots }, `Available read-only roots: ${roots.map((root) => root.name).join(", ")}.`);
  }
);

register(
  "list_directory",
  {
    title: "List directory",
    description: "List files and subdirectories below a relative path in work or cowork.",
    inputSchema: {
      root: rootSchema,
      path: z.string().default(""),
      depth: z.number().int().min(1).max(5).default(1),
      limit: z.number().int().min(1).max(2000).default(500)
    }
  },
  async (input) => {
    const data = await service.listDirectory(input);
    return textResult(data, `Found ${data.entries.length} entries under ${data.path}${data.truncated ? " (truncated)" : ""}.`);
  }
);

register(
  "get_file_info",
  {
    title: "Get file information",
    description: "Return size, dates, MIME type, and URI for a file or directory.",
    inputSchema: { root: rootSchema, path: z.string().min(1) }
  },
  async (input) => {
    const data = await service.getFileInfo(input);
    return textResult(data, `${data.path}: ${data.type}, ${data.size} bytes, modified ${data.modifiedAt}.`);
  }
);

register(
  "search_files",
  {
    title: "Search file names",
    description: "Search relative file paths by case-insensitive name fragment. Use root=all to search both folders.",
    inputSchema: {
      root: searchableRootSchema.default("all"),
      path: z.string().default(""),
      query: z.string().min(1),
      limit: z.number().int().min(1).max(200).default(50)
    }
  },
  async (input) => {
    const data = await service.searchFiles(input);
    return textResult(data, `Found ${data.results.length} file paths matching “${data.query}”${data.truncated ? " (truncated)" : ""}.`);
  }
);

register(
  "search_text",
  {
    title: "Search inside text files",
    description: "Search UTF-8 text, Markdown, CSV, JSON, HTML, RTF, and source files. Narrow path for large searches.",
    inputSchema: {
      root: searchableRootSchema.default("all"),
      path: z.string().default(""),
      query: z.string().min(2),
      limit: z.number().int().min(1).max(100).default(30)
    }
  },
  async (input) => {
    const data = await service.searchText(input);
    return textResult(data, `Found ${data.results.length} text matches for “${data.query}”${data.truncated ? " (search limit reached)" : ""}.`);
  }
);

register(
  "read_text_file",
  {
    title: "Read text file",
    description: "Read a UTF-8 text file or selected line range. Prefer this for Markdown and extracted OCR text.",
    inputSchema: {
      root: rootSchema,
      path: z.string().min(1),
      startLine: z.number().int().min(1).default(1),
      endLine: z.number().int().min(1).optional(),
      maxChars: z.number().int().min(1).max(200000).default(200000)
    }
  },
  async (input) => {
    const data = await service.readTextFile(input);
    return textResult(data, data.text);
  }
);

register(
  "read_file",
  {
    title: "Read file",
    description: "Read text directly or return a bounded binary file (PDF, Office document, image, audio) as an MCP embedded resource.",
    inputSchema: {
      root: rootSchema,
      path: z.string().min(1),
      maxChars: z.number().int().min(1).max(200000).default(200000)
    }
  },
  async (input) => {
    const data = await service.readFile(input);
    if (data.kind === "text") {
      return textResult(data, data.text);
    }

    const { blob, ...metadata } = data;
    return {
      structuredContent: metadata,
      content: [
        {
          type: "resource",
          resource: { uri: data.uri, blob, mimeType: data.mimeType }
        }
      ]
    };
  }
);

register(
  "search",
  {
    title: "Search local documents",
    description: "ChatGPT-compatible search across file names and indexed-on-demand text in both allowed roots.",
    inputSchema: { query: z.string().min(1) },
    outputSchema: {
      results: z.array(z.object({ id: z.string(), title: z.string(), text: z.string(), url: z.string() }))
    }
  },
  async (input) => {
    const data = await service.connectorSearch(input);
    return textResult(data, `Found ${data.results.length} local documents.`);
  }
);

register(
  "fetch",
  {
    title: "Fetch local document",
    description: "Fetch a UTF-8 local document returned by search using its stable root:path id.",
    inputSchema: { id: z.string().min(3) }
  },
  async (input) => {
    const data = await service.connectorFetch(input);
    return textResult(data, data.text);
  }
);

async function main() {
  await service.listRoots();
  const transport = new StdioServerTransport();
  await server.connect(transport);
  console.error("strigov-local-files MCP server is ready (read-only)");
}

main().catch((error) => {
  console.error(error instanceof Error ? error.stack : error);
  process.exit(1);
});
