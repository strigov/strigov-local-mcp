import { promises as fs } from "node:fs";
import path from "node:path";
import { TextDecoder } from "node:util";

const UTF8 = new TextDecoder("utf-8", { fatal: true });

const TEXT_EXTENSIONS = new Set([
  ".c", ".cc", ".conf", ".cpp", ".css", ".csv", ".eml", ".h", ".hpp",
  ".htm", ".html", ".ini", ".java", ".js", ".json", ".jsonl", ".jsx",
  ".log", ".md", ".mjs", ".py", ".rb", ".rst", ".rtf", ".sh", ".sql",
  ".svg", ".toml", ".ts", ".tsx", ".txt", ".xml", ".yaml", ".yml"
]);

const MIME_TYPES = new Map([
  [".bmp", "image/bmp"],
  [".csv", "text/csv"],
  [".doc", "application/msword"],
  [".docx", "application/vnd.openxmlformats-officedocument.wordprocessingml.document"],
  [".gif", "image/gif"],
  [".htm", "text/html"],
  [".html", "text/html"],
  [".jpeg", "image/jpeg"],
  [".jpg", "image/jpeg"],
  [".json", "application/json"],
  [".md", "text/markdown"],
  [".mp3", "audio/mpeg"],
  [".mp4", "video/mp4"],
  [".ods", "application/vnd.oasis.opendocument.spreadsheet"],
  [".odt", "application/vnd.oasis.opendocument.text"],
  [".pdf", "application/pdf"],
  [".png", "image/png"],
  [".ppt", "application/vnd.ms-powerpoint"],
  [".pptx", "application/vnd.openxmlformats-officedocument.presentationml.presentation"],
  [".rtf", "application/rtf"],
  [".svg", "image/svg+xml"],
  [".tif", "image/tiff"],
  [".tiff", "image/tiff"],
  [".txt", "text/plain"],
  [".wav", "audio/wav"],
  [".webp", "image/webp"],
  [".xls", "application/vnd.ms-excel"],
  [".xlsx", "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet"],
  [".xml", "application/xml"],
  [".zip", "application/zip"]
]);

const DEFAULT_MAX_TEXT_BYTES = 5 * 1024 * 1024;
const DEFAULT_MAX_BINARY_BYTES = 15 * 1024 * 1024;
const DEFAULT_MAX_OUTPUT_CHARS = 200_000;

export class FilesystemError extends Error {
  constructor(code, message) {
    super(message);
    this.name = "FilesystemError";
    this.code = code;
  }
}

function isInside(root, candidate) {
  const relative = path.relative(root, candidate);
  return relative === "" || (!relative.startsWith(`..${path.sep}`) && relative !== ".." && !path.isAbsolute(relative));
}

function denyReason(relativePath) {
  const components = relativePath.split(path.sep).filter(Boolean);

  for (const component of components) {
    const lower = component.toLowerCase();
    if (lower === ".git" || lower === ".ssh" || lower === ".aws" || lower === "node_modules") {
      return `access to ${component} is blocked`;
    }
    if (lower === ".ds_store") {
      return "macOS metadata files are blocked";
    }
    if (lower === ".env" || lower.startsWith(".env.")) {
      return "environment files are blocked";
    }
    if (lower === "id_rsa" || lower === "id_ed25519") {
      return "private key files are blocked";
    }
  }

  const extension = path.extname(relativePath).toLowerCase();
  if ([".key", ".p12", ".pfx", ".pem"].includes(extension)) {
    return "credential and private key files are blocked";
  }

  return null;
}

function publicPath(rootName, relativePath = "") {
  const normalized = relativePath.split(path.sep).join("/");
  return normalized ? `${rootName}/${normalized}` : rootName;
}

function fileUri(rootName, relativePath) {
  return `mcp://local-files/${encodeURIComponent(rootName)}/${relativePath
    .split(path.sep)
    .filter(Boolean)
    .map(encodeURIComponent)
    .join("/")}`;
}

function mimeTypeFor(filePath) {
  return MIME_TYPES.get(path.extname(filePath).toLowerCase()) ?? "application/octet-stream";
}

function looksLikeText(filePath) {
  return TEXT_EXTENSIONS.has(path.extname(filePath).toLowerCase());
}

function toIso(value) {
  return value instanceof Date ? value.toISOString() : new Date(value).toISOString();
}

export function createFilesystemService(rootDefinitions, options = {}) {
  const maxTextBytes = options.maxTextBytes ?? DEFAULT_MAX_TEXT_BYTES;
  const maxBinaryBytes = options.maxBinaryBytes ?? DEFAULT_MAX_BINARY_BYTES;
  const maxOutputChars = options.maxOutputChars ?? DEFAULT_MAX_OUTPUT_CHARS;
  let rootsPromise;

  async function initializeRoots() {
    const entries = await Promise.all(
      Object.entries(rootDefinitions).map(async ([name, configuredPath]) => {
        if (!/^[a-z][a-z0-9_-]{0,31}$/.test(name)) {
          throw new FilesystemError("INVALID_ROOT", `Invalid root alias: ${name}`);
        }

        const absolutePath = path.resolve(configuredPath);
        let realPath;
        let stat;
        try {
          [realPath, stat] = await Promise.all([fs.realpath(absolutePath), fs.stat(absolutePath)]);
        } catch (error) {
          throw new FilesystemError("ROOT_UNAVAILABLE", `Root ${name} is unavailable: ${error.message}`);
        }
        if (!stat.isDirectory()) {
          throw new FilesystemError("ROOT_NOT_DIRECTORY", `Root ${name} is not a directory`);
        }
        return [name, { name, configuredPath: absolutePath, realPath }];
      })
    );
    return new Map(entries);
  }

  function roots() {
    rootsPromise ??= initializeRoots();
    return rootsPromise;
  }

  async function resolvePath(rootName, requestedPath = "", expectedType = "any") {
    if (typeof requestedPath !== "string" || requestedPath.includes("\0")) {
      throw new FilesystemError("INVALID_PATH", "Path must be a relative string without NUL bytes");
    }
    if (path.isAbsolute(requestedPath)) {
      throw new FilesystemError("ABSOLUTE_PATH", "Absolute paths are not accepted; use a root alias and relative path");
    }

    const root = (await roots()).get(rootName);
    if (!root) {
      throw new FilesystemError("UNKNOWN_ROOT", `Unknown root: ${rootName}`);
    }

    const lexicalPath = path.resolve(root.realPath, requestedPath || ".");
    if (!isInside(root.realPath, lexicalPath)) {
      throw new FilesystemError("PATH_ESCAPE", "The requested path leaves the selected root");
    }

    const relativePath = path.relative(root.realPath, lexicalPath);
    const denied = denyReason(relativePath);
    if (denied) {
      throw new FilesystemError("BLOCKED_PATH", denied);
    }

    let realPath;
    let stat;
    try {
      realPath = await fs.realpath(lexicalPath);
      if (!isInside(root.realPath, realPath)) {
        throw new FilesystemError("SYMLINK_ESCAPE", "The requested path resolves outside the selected root");
      }
      stat = await fs.stat(realPath);
    } catch (error) {
      if (error instanceof FilesystemError) throw error;
      if (error.code === "ENOENT") {
        throw new FilesystemError("NOT_FOUND", `Path not found: ${publicPath(rootName, relativePath)}`);
      }
      throw new FilesystemError("ACCESS_ERROR", `Cannot access ${publicPath(rootName, relativePath)}: ${error.message}`);
    }

    if (expectedType === "file" && !stat.isFile()) {
      throw new FilesystemError("NOT_FILE", `Not a file: ${publicPath(rootName, relativePath)}`);
    }
    if (expectedType === "directory" && !stat.isDirectory()) {
      throw new FilesystemError("NOT_DIRECTORY", `Not a directory: ${publicPath(rootName, relativePath)}`);
    }

    return { root, rootName, relativePath, realPath, stat };
  }

  async function listRoots() {
    return [...(await roots()).values()].map((root) => ({
      name: root.name,
      path: root.name,
      uri: `mcp://local-files/${encodeURIComponent(root.name)}/`,
      readOnly: true
    }));
  }

  async function listDirectory({ root: rootName, path: requestedPath = "", depth = 1, limit = 500 }) {
    const resolved = await resolvePath(rootName, requestedPath, "directory");
    const safeDepth = Math.max(1, Math.min(depth, 5));
    const safeLimit = Math.max(1, Math.min(limit, 2_000));
    const entries = [];
    let truncated = false;

    async function visit(directoryPath, directoryRelative, remainingDepth) {
      if (truncated) return;
      const children = await fs.readdir(directoryPath, { withFileTypes: true });
      children.sort((a, b) => a.name.localeCompare(b.name, "ru"));

      for (const child of children) {
        if (entries.length >= safeLimit) {
          truncated = true;
          return;
        }

        const childRelative = path.join(directoryRelative, child.name);
        const blocked = denyReason(childRelative);
        if (blocked) continue;

        const childPath = path.join(directoryPath, child.name);
        let type = child.isDirectory() ? "directory" : child.isFile() ? "file" : child.isSymbolicLink() ? "symlink" : "other";
        let size = null;
        let modifiedAt = null;
        try {
          const childStat = await fs.lstat(childPath);
          size = childStat.isFile() ? childStat.size : null;
          modifiedAt = toIso(childStat.mtime);
        } catch {
          type = "unavailable";
        }

        entries.push({
          name: child.name,
          path: publicPath(rootName, childRelative),
          relativePath: childRelative.split(path.sep).join("/"),
          type,
          size,
          modifiedAt
        });

        if (child.isDirectory() && !child.isSymbolicLink() && remainingDepth > 1) {
          await visit(childPath, childRelative, remainingDepth - 1);
        }
      }
    }

    await visit(resolved.realPath, resolved.relativePath, safeDepth);
    return {
      root: rootName,
      path: publicPath(rootName, resolved.relativePath),
      entries,
      truncated
    };
  }

  async function getFileInfo({ root: rootName, path: requestedPath }) {
    const resolved = await resolvePath(rootName, requestedPath);
    return {
      root: rootName,
      path: publicPath(rootName, resolved.relativePath),
      relativePath: resolved.relativePath.split(path.sep).join("/"),
      uri: fileUri(rootName, resolved.relativePath),
      type: resolved.stat.isFile() ? "file" : resolved.stat.isDirectory() ? "directory" : "other",
      size: resolved.stat.size,
      mimeType: resolved.stat.isFile() ? mimeTypeFor(resolved.realPath) : null,
      createdAt: toIso(resolved.stat.birthtime),
      modifiedAt: toIso(resolved.stat.mtime),
      readOnly: true
    };
  }

  async function readTextFile({ root: rootName, path: requestedPath, startLine = 1, endLine, maxChars = maxOutputChars }) {
    const resolved = await resolvePath(rootName, requestedPath, "file");
    if (resolved.stat.size > maxTextBytes) {
      throw new FilesystemError("FILE_TOO_LARGE", `Text file is ${resolved.stat.size} bytes; limit is ${maxTextBytes}`);
    }

    const buffer = await fs.readFile(resolved.realPath);
    let text;
    try {
      text = UTF8.decode(buffer);
    } catch {
      throw new FilesystemError(
        "NOT_TEXT",
        "The file is not valid UTF-8 text. For PDF/DOCX/XLSX use read_file, or create a Markdown sidecar for reliable analysis."
      );
    }

    const lines = text.split(/\r?\n/);
    const first = Math.max(1, startLine);
    const last = Math.min(endLine ?? lines.length, lines.length);
    if (last < first) {
      throw new FilesystemError("INVALID_RANGE", "endLine must be greater than or equal to startLine");
    }

    const safeMaxChars = Math.max(1, Math.min(maxChars, maxOutputChars));
    const selected = lines.slice(first - 1, last).join("\n");
    const truncated = selected.length > safeMaxChars;
    const output = truncated ? selected.slice(0, safeMaxChars) : selected;

    return {
      root: rootName,
      path: publicPath(rootName, resolved.relativePath),
      uri: fileUri(rootName, resolved.relativePath),
      mimeType: mimeTypeFor(resolved.realPath),
      text: output,
      startLine: first,
      endLine: first + output.split(/\r?\n/).length - 1,
      totalLines: lines.length,
      truncated
    };
  }

  async function readFile({ root: rootName, path: requestedPath, maxChars = maxOutputChars }) {
    const resolved = await resolvePath(rootName, requestedPath, "file");
    if (looksLikeText(resolved.realPath)) {
      return { kind: "text", ...(await readTextFile({ root: rootName, path: requestedPath, maxChars })) };
    }
    if (resolved.stat.size > maxBinaryBytes) {
      throw new FilesystemError("FILE_TOO_LARGE", `Binary file is ${resolved.stat.size} bytes; limit is ${maxBinaryBytes}`);
    }

    const buffer = await fs.readFile(resolved.realPath);
    try {
      const text = UTF8.decode(buffer);
      const safeMaxChars = Math.max(1, Math.min(maxChars, maxOutputChars));
      return {
        kind: "text",
        root: rootName,
        path: publicPath(rootName, resolved.relativePath),
        uri: fileUri(rootName, resolved.relativePath),
        mimeType: "text/plain",
        text: text.slice(0, safeMaxChars),
        startLine: 1,
        endLine: text.slice(0, safeMaxChars).split(/\r?\n/).length,
        totalLines: text.split(/\r?\n/).length,
        truncated: text.length > safeMaxChars
      };
    } catch {
      return {
        kind: "binary",
        root: rootName,
        path: publicPath(rootName, resolved.relativePath),
        uri: fileUri(rootName, resolved.relativePath),
        mimeType: mimeTypeFor(resolved.realPath),
        size: resolved.stat.size,
        blob: buffer.toString("base64")
      };
    }
  }

  async function walkFiles(rootName, requestedPath, options, onFile) {
    const resolved = await resolvePath(rootName, requestedPath || "", "directory");
    const maxFiles = options.maxFiles ?? 10_000;
    const stack = [{ absolute: resolved.realPath, relative: resolved.relativePath }];
    let visitedFiles = 0;
    let truncated = false;

    while (stack.length > 0) {
      const current = stack.pop();
      const children = await fs.readdir(current.absolute, { withFileTypes: true });
      children.sort((a, b) => b.name.localeCompare(a.name, "ru"));

      for (const child of children) {
        const childRelative = path.join(current.relative, child.name);
        if (denyReason(childRelative)) continue;
        const childAbsolute = path.join(current.absolute, child.name);

        if (child.isDirectory() && !child.isSymbolicLink()) {
          stack.push({ absolute: childAbsolute, relative: childRelative });
          continue;
        }
        if (!child.isFile()) continue;

        visitedFiles += 1;
        if (visitedFiles > maxFiles) {
          truncated = true;
          return { visitedFiles: visitedFiles - 1, truncated };
        }
        const shouldStop = await onFile({ absolute: childAbsolute, relative: childRelative });
        if (shouldStop) return { visitedFiles, truncated: true };
      }
    }
    return { visitedFiles, truncated };
  }

  async function searchFiles({ root: requestedRoot, path: requestedPath = "", query, limit = 50 }) {
    const aliases = requestedRoot === "all" ? [...(await roots()).keys()] : [requestedRoot];
    const safeLimit = Math.max(1, Math.min(limit, 200));
    const needle = query.toLocaleLowerCase("ru");
    const results = [];
    let truncated = false;

    for (const rootName of aliases) {
      const walk = await walkFiles(rootName, requestedPath, { maxFiles: 20_000 }, async (file) => {
        const normalized = file.relative.split(path.sep).join("/");
        if (normalized.toLocaleLowerCase("ru").includes(needle)) {
          const stat = await fs.stat(file.absolute);
          results.push({
            root: rootName,
            path: publicPath(rootName, file.relative),
            relativePath: normalized,
            uri: fileUri(rootName, file.relative),
            size: stat.size,
            modifiedAt: toIso(stat.mtime)
          });
        }
        return results.length >= safeLimit;
      });
      truncated ||= walk.truncated;
      if (results.length >= safeLimit) break;
    }

    return { query, results, truncated };
  }

  async function searchText({ root: requestedRoot, path: requestedPath = "", query, limit = 30 }) {
    const aliases = requestedRoot === "all" ? [...(await roots()).keys()] : [requestedRoot];
    const safeLimit = Math.max(1, Math.min(limit, 100));
    const needle = query.toLocaleLowerCase("ru");
    const results = [];
    let scannedBytes = 0;
    let skippedLargeFiles = 0;
    let truncated = false;
    const maxScannedBytes = 128 * 1024 * 1024;

    for (const rootName of aliases) {
      const walk = await walkFiles(rootName, requestedPath, { maxFiles: 20_000 }, async (file) => {
        if (!looksLikeText(file.absolute)) return false;
        const stat = await fs.stat(file.absolute);
        if (stat.size > maxTextBytes) {
          skippedLargeFiles += 1;
          return false;
        }
        if (scannedBytes + stat.size > maxScannedBytes) return true;
        scannedBytes += stat.size;

        let text;
        try {
          text = UTF8.decode(await fs.readFile(file.absolute));
        } catch {
          return false;
        }
        const haystack = text.toLocaleLowerCase("ru");
        const index = haystack.indexOf(needle);
        if (index === -1) return false;

        const line = text.slice(0, index).split(/\r?\n/).length;
        const from = Math.max(0, index - 180);
        const to = Math.min(text.length, index + query.length + 320);
        results.push({
          root: rootName,
          path: publicPath(rootName, file.relative),
          relativePath: file.relative.split(path.sep).join("/"),
          uri: fileUri(rootName, file.relative),
          line,
          snippet: text.slice(from, to).replace(/\s+/g, " ").trim()
        });
        return results.length >= safeLimit;
      });
      truncated ||= walk.truncated;
      if (results.length >= safeLimit || scannedBytes >= maxScannedBytes) {
        truncated = true;
        break;
      }
    }

    return { query, results, scannedBytes, skippedLargeFiles, truncated };
  }

  function parseId(id) {
    if (typeof id !== "string") throw new FilesystemError("INVALID_ID", "Document id must be a string");
    const slash = id.indexOf(":");
    if (slash <= 0) throw new FilesystemError("INVALID_ID", "Document id must use root:path format");
    return { root: id.slice(0, slash), path: id.slice(slash + 1) };
  }

  async function connectorSearch({ query }) {
    const names = await searchFiles({ root: "all", query, limit: 20 });
    let matches = names.results.map((item) => ({
      id: `${item.root}:${item.relativePath}`,
      title: path.basename(item.relativePath),
      text: item.path,
      url: item.uri
    }));

    if (matches.length < 20) {
      const textMatches = await searchText({ root: "all", query, limit: 20 - matches.length });
      const seen = new Set(matches.map((item) => item.id));
      for (const item of textMatches.results) {
        const id = `${item.root}:${item.relativePath}`;
        if (!seen.has(id)) {
          matches.push({ id, title: path.basename(item.relativePath), text: item.snippet, url: item.uri });
          seen.add(id);
        }
      }
    }

    return { results: matches.slice(0, 20) };
  }

  async function connectorFetch({ id }) {
    const parsed = parseId(id);
    const result = await readTextFile({ root: parsed.root, path: parsed.path });
    return {
      id,
      title: path.basename(parsed.path),
      text: result.text,
      url: result.uri,
      metadata: {
        root: parsed.root,
        path: result.path,
        mimeType: result.mimeType,
        totalLines: result.totalLines,
        truncated: result.truncated
      }
    };
  }

  return {
    listRoots,
    listDirectory,
    getFileInfo,
    readTextFile,
    readFile,
    searchFiles,
    searchText,
    connectorSearch,
    connectorFetch,
    resolvePath
  };
}
