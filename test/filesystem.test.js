import assert from "node:assert/strict";
import { mkdtemp, mkdir, rm, symlink, writeFile } from "node:fs/promises";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { createFilesystemService, FilesystemError } from "../src/filesystem.js";

async function fixture(t) {
  const base = await mkdtemp(path.join(os.tmpdir(), "local-files-mcp-"));
  const work = path.join(base, "work");
  const cowork = path.join(base, "cowork");
  const outside = path.join(base, "outside.txt");
  await mkdir(path.join(work, "case"), { recursive: true });
  await mkdir(cowork, { recursive: true });
  await writeFile(path.join(work, "case", "memo.md"), "Первая строка\nИскомая фраза\nТретья строка\n", "utf8");
  await writeFile(path.join(cowork, "notes.txt"), "cowork note", "utf8");
  await writeFile(path.join(work, ".env"), "SECRET=value", "utf8");
  await writeFile(outside, "outside", "utf8");
  await symlink(outside, path.join(work, "escape-link"));
  t.after(() => rm(base, { recursive: true, force: true }));
  return { service: createFilesystemService({ work, cowork }), work, cowork };
}

test("lists only logical root aliases", async (t) => {
  const { service } = await fixture(t);
  const roots = await service.listRoots();
  assert.deepEqual(roots.map((root) => root.name), ["work", "cowork"]);
  assert.equal(roots[0].path, "work");
  assert.equal(roots[0].readOnly, true);
});

test("reads text by line range", async (t) => {
  const { service } = await fixture(t);
  const result = await service.readTextFile({ root: "work", path: "case/memo.md", startLine: 2, endLine: 2 });
  assert.equal(result.text, "Искомая фраза");
  assert.equal(result.path, "work/case/memo.md");
});

test("blocks path traversal", async (t) => {
  const { service } = await fixture(t);
  await assert.rejects(
    service.readTextFile({ root: "work", path: "../outside.txt" }),
    (error) => error instanceof FilesystemError && error.code === "PATH_ESCAPE"
  );
});

test("blocks symlinks escaping a root", async (t) => {
  const { service } = await fixture(t);
  await assert.rejects(
    service.readTextFile({ root: "work", path: "escape-link" }),
    (error) => error instanceof FilesystemError && error.code === "SYMLINK_ESCAPE"
  );
});

test("blocks common secret files", async (t) => {
  const { service } = await fixture(t);
  await assert.rejects(
    service.readTextFile({ root: "work", path: ".env" }),
    (error) => error instanceof FilesystemError && error.code === "BLOCKED_PATH"
  );
});

test("searches names and text", async (t) => {
  const { service } = await fixture(t);
  const names = await service.searchFiles({ root: "all", query: "memo" });
  assert.equal(names.results.length, 1);
  assert.equal(names.results[0].path, "work/case/memo.md");

  const text = await service.searchText({ root: "work", query: "искомая" });
  assert.equal(text.results.length, 1);
  assert.equal(text.results[0].line, 2);
});

test("connector search and fetch use stable ids", async (t) => {
  const { service } = await fixture(t);
  const search = await service.connectorSearch({ query: "memo" });
  assert.equal(search.results[0].id, "work:case/memo.md");
  const fetched = await service.connectorFetch({ id: search.results[0].id });
  assert.match(fetched.text, /Искомая фраза/);
});
