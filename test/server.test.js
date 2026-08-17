import assert from "node:assert/strict";
import { mkdtemp, mkdir, rm, writeFile } from "node:fs/promises";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { Client } from "@modelcontextprotocol/sdk/client/index.js";
import { StdioClientTransport } from "@modelcontextprotocol/sdk/client/stdio.js";

test("stdio server advertises and executes read-only tools", async (t) => {
  const base = await mkdtemp(path.join(os.tmpdir(), "local-files-mcp-server-"));
  const work = path.join(base, "work");
  const cowork = path.join(base, "cowork");
  await mkdir(work);
  await mkdir(cowork);
  await writeFile(path.join(work, "hello.md"), "Привет из MCP\n", "utf8");

  const transport = new StdioClientTransport({
    command: process.execPath,
    args: [path.resolve("src/server.js")],
    env: {
      ...process.env,
      MCP_WORK_ROOT: work,
      MCP_COWORK_ROOT: cowork
    },
    stderr: "pipe"
  });
  const client = new Client({ name: "integration-test", version: "1.0.0" });

  t.after(async () => {
    await client.close();
    await rm(base, { recursive: true, force: true });
  });

  await client.connect(transport);
  const listed = await client.listTools();
  const names = listed.tools.map((tool) => tool.name);
  assert.deepEqual(names.sort(), [
    "fetch",
    "get_file_info",
    "list_directory",
    "list_roots",
    "read_file",
    "read_text_file",
    "search",
    "search_files",
    "search_text"
  ]);
  assert.ok(listed.tools.every((tool) => tool.annotations?.readOnlyHint === true));

  const read = await client.callTool({
    name: "read_text_file",
    arguments: { root: "work", path: "hello.md" }
  });
  assert.equal(read.isError, undefined);
  assert.equal(read.structuredContent.text, "Привет из MCP\n");

  const blocked = await client.callTool({
    name: "read_text_file",
    arguments: { root: "work", path: "../outside.md" }
  });
  assert.equal(blocked.isError, true);
  assert.equal(blocked.structuredContent.error.code, "PATH_ESCAPE");
});
