#!/usr/bin/env python3
"""Verification and recording script for CP1251 TXT support."""

from __future__ import annotations

import base64
import hashlib
import json
import os
import re
import shutil
import subprocess
import sys
from pathlib import Path


def get_repo_root() -> Path:
    return Path(__file__).resolve().parent.parent


def sha256_file(path: Path) -> str:
    h = hashlib.sha256()
    with open(path, "rb") as f:
        while chunk := f.read(65536):
            h.update(chunk)
    return h.hexdigest()


def compute_fingerprint(repo_root: Path) -> dict:
    files = {}

    top_files = [
        "README.md",
        "Cargo.toml",
        "Cargo.lock",
        "package.json",
        "package-lock.json",
    ]
    for rel in top_files:
        p = repo_root / rel
        if p.is_file():
            files[rel] = sha256_file(p)

    for d in ["native", "test", "scripts"]:
        dir_path = repo_root / d
        if not dir_path.is_dir():
            continue
        for root, _, filenames in os.walk(dir_path):
            for name in filenames:
                full_path = Path(root) / name
                rel_path = full_path.relative_to(repo_root).as_posix()
                if rel_path == "scripts/verify-cp1251.py":
                    continue
                if name == ".DS_Store":
                    continue
                files[rel_path] = sha256_file(full_path)

    files = dict(sorted(files.items()))

    def test_block_hash(path: Path) -> str:
        lines = path.read_text(encoding="utf-8").splitlines(keepends=True)
        idx = None
        for i, line in enumerate(lines):
            if line.startswith("mod tests {"):
                idx = i
                break
        if idx is None:
            raise ValueError(f"No 'mod tests {{' found in {path}")
        content = "".join(lines[idx:]).encode("utf-8")
        return hashlib.sha256(content).hexdigest()

    tests = {
        "native/filesystem.rs": test_block_hash(repo_root / "native/filesystem.rs"),
        "native/documents.rs": test_block_hash(repo_root / "native/documents.rs"),
        "test/native.test.mjs": sha256_file(repo_root / "test/native.test.mjs"),
    }

    return {
        "files": files,
        "tests": tests,
    }


def run_command_with_log(
    cmd: list[str],
    log_path: Path,
    exit_path: Path,
    cwd: Path,
    env: dict[str, str] | None = None,
) -> int:
    process_env = os.environ.copy()
    if env:
        process_env.update(env)

    result = subprocess.run(
        cmd,
        cwd=cwd,
        env=process_env,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        text=True,
    )
    log_path.write_text(result.stdout, encoding="utf-8")
    exit_path.write_text(f"{result.returncode}\n", encoding="utf-8")
    return result.returncode


def record_red(repo_root: Path):
    red_dir = repo_root / "artifacts/cp1251-fix/red"
    red_dir.mkdir(parents=True, exist_ok=True)

    commands = [
        ("cargo-test", ["cargo", "test", "--locked", "cp1251"], None),
        (
            "ac18-cargo",
            [
                "cargo",
                "test",
                "--locked",
                "validates_utf8_beyond_the_requested_output_and_guards_panics",
            ],
            None,
        ),
        (
            "build",
            ["cargo", "build", "--release", "--locked", "--bin", "local-files"],
            None,
        ),
        (
            "native-test",
            [
                "node",
                "--test",
                "--test-name-pattern",
                "cp1251",
                "test/native.test.mjs",
            ],
            {"NATIVE_BINARY": "target/release/local-files"},
        ),
        (
            "ac18-native",
            [
                "node",
                "--test",
                "--test-name-pattern",
                "document continuation progresses at maxChars=1 and rejects stale files",
                "test/native.test.mjs",
            ],
            {"NATIVE_BINARY": "target/release/local-files"},
        ),
    ]

    for name, cmd, env in commands:
        log_file = red_dir / f"{name}.log"
        exit_file = red_dir / f"{name}.exit"
        run_command_with_log(cmd, log_file, exit_file, repo_root, env)

    diff_proc = subprocess.run(
        ["git", "diff", "13dbae0", "--", "native", "test"],
        cwd=repo_root,
        stdout=subprocess.PIPE,
        text=True,
        check=True,
    )
    (red_dir / "diff.patch").write_text(diff_proc.stdout, encoding="utf-8")

    src_bin = repo_root / "target/release/local-files"
    dst_bin = red_dir / "local-files"
    shutil.copy2(src_bin, dst_bin)
    os.chmod(dst_bin, 0o755)

    bin_sha256 = sha256_file(dst_bin)
    (red_dir / "local-files.sha256").write_text(f"{bin_sha256}\n", encoding="utf-8")

    fp = compute_fingerprint(repo_root)
    (red_dir / "fingerprint.json").write_text(
        json.dumps(fp, indent=2) + "\n", encoding="utf-8"
    )
    print("record-red completed successfully.")


CP1251_TEST_NAME = (
    "cp1251: Windows-1251 text is read and searched by every tool"
)
CP1251_LOCK_NAME = (
    "cp1251 lock: UTF-8, BOM, binary, limits and access rules are unchanged"
)


def parse_node_test_outcome(log: str, test_name: str) -> str:
    escaped = re.escape(test_name)
    spec_pass = bool(
        re.search(r"^[✔√]\s+" + escaped + r"(?:\s+\([\d.]+m?s\))?\s*$", log, re.M)
    )
    tap_pass = bool(
        re.search(r"^ok(?:\s+\d+)?\s+-\s+" + escaped + r"(?:\s+#.*)?$", log, re.M)
    )
    is_pass = spec_pass or tap_pass

    spec_fail = bool(
        re.search(r"^[✖×]\s+" + escaped + r"(?:\s+\([\d.]+m?s\))?\s*$", log, re.M)
    )
    tap_fail = bool(
        re.search(
            r"^not ok(?:\s+\d+)?\s+-\s+" + escaped + r"(?:\s+#.*)?$", log, re.M
        )
    )
    is_fail = spec_fail or tap_fail

    if is_pass and not is_fail:
        return "PASS"
    if is_fail and not is_pass:
        return "FAIL"
    if is_pass and is_fail:
        return "AMBIGUOUS"
    return "NOT_FOUND"


def check_native_test_log(log: str) -> tuple[bool, str]:
    # Reject timeout, transport, and setup errors regardless of overall counts
    disallowed = [
        (
            r"\b(?:timed?\s*out|timeouts?|etimedout|timeouterror)\b",
            "timeout error",
        ),
        (
            r"\b(?:econnreset|econnrefused|epipe|socket\s+hang\s+up|premature\s+close|transport\s+(?:error|closed)|connection\s+closed)\b",
            "transport error",
        ),
        (
            r"\b(?:(?:before|beforeEach|after|afterEach)\s+hook\s+failed|hook\s+failure|setup\s+failed|spawn\s+enoent)\b",
            "setup error",
        ),
    ]
    for pattern, kind in disallowed:
        if re.search(pattern, log, re.I):
            return False, f"native-test.log contains {kind} ({pattern})"

    # Summary counts: pass 1, fail 1
    if not re.search(r"^(?:ℹ|#|\u2139)?\s*pass 1\b", log, re.M):
        return False, "native-test.log does not contain pass 1"
    if not re.search(r"^(?:ℹ|#|\u2139)?\s*fail 1\b", log, re.M):
        return False, "native-test.log does not contain fail 1"

    # Lock test must PASS
    lock_status = parse_node_test_outcome(log, CP1251_LOCK_NAME)
    if lock_status != "PASS":
        return (
            False,
            f"Lock test '{CP1251_LOCK_NAME}' outcome is {lock_status}, expected PASS",
        )

    # CP1251 test must FAIL
    cp1251_status = parse_node_test_outcome(log, CP1251_TEST_NAME)
    if cp1251_status != "FAIL":
        return (
            False,
            f"CP1251 test '{CP1251_TEST_NAME}' outcome is {cp1251_status}, expected FAIL",
        )

    # CP1251 test must fail due to expected assertion failure
    assertion_pattern = (
        r"(?:[✖×]\s+|not ok(?:\s+\d+)?\s+-\s+)"
        + re.escape(CP1251_TEST_NAME)
        + r"[\s\S]*?(?:AssertionError|ERR_ASSERTION)"
    )
    if not re.search(assertion_pattern, log):
        return (
            False,
            f"CP1251 test '{CP1251_TEST_NAME}' failure does not contain expected AssertionError / ERR_ASSERTION",
        )

    return True, "OK"


def verify_red(repo_root: Path):
    red_dir = repo_root / "artifacts/cp1251-fix/red"
    guards_dir = repo_root / "artifacts/cp1251-fix/guards"

    # Check 1: fingerprint matches
    fp_path = red_dir / "fingerprint.json"
    if not fp_path.is_file():
        print("FAIL: red/fingerprint.json missing")
        sys.exit(1)
    expected_fp = json.loads(fp_path.read_text(encoding="utf-8"))
    actual_fp = compute_fingerprint(repo_root)
    if actual_fp != expected_fp:
        print("FAIL: Current draft fingerprint does not match red/fingerprint.json")
        sys.exit(1)

    # Check 2: changed and new files outside docs/
    # (by git status --porcelain and git diff --name-only 13dbae0)
    status_proc = subprocess.run(
        ["git", "status", "--porcelain"],
        cwd=repo_root,
        stdout=subprocess.PIPE,
        text=True,
        check=True,
    )
    status_files = set()
    for line in status_proc.stdout.splitlines():
        if line.strip():
            # porcelain format: XY filename or XY orig -> dest
            path_str = line[3:].strip()
            if " -> " in path_str:
                path_str = path_str.split(" -> ")[1].strip()
            status_files.add(path_str)

    diff_proc = subprocess.run(
        ["git", "diff", "--name-only", "13dbae0"],
        cwd=repo_root,
        stdout=subprocess.PIPE,
        text=True,
        check=True,
    )
    diff_files = set(filter(None, diff_proc.stdout.splitlines()))

    non_docs_files = {
        f for f in (status_files | diff_files) if not f.startswith("docs/")
    }
    expected_non_docs = {
        "native/documents.rs",
        "native/filesystem.rs",
        "test/native.test.mjs",
        "scripts/verify-cp1251.py",
    }
    if non_docs_files != expected_non_docs:
        print(
            f"FAIL: Changed/new files outside docs/ {non_docs_files} != {expected_non_docs}"
        )
        sys.exit(1)

    # Check 3: no deleted lines in filesystem.rs, documents.rs changed exactly -1/+1
    fs_diff_proc = subprocess.run(
        ["git", "diff", "-U0", "13dbae0", "--", "native/filesystem.rs"],
        cwd=repo_root,
        stdout=subprocess.PIPE,
        text=True,
        check=True,
    )
    for line in fs_diff_proc.stdout.splitlines():
        if line.startswith("-") and not line.startswith("---"):
            print("FAIL: Deleted line found in native/filesystem.rs diff")
            sys.exit(1)

    doc_diff_proc = subprocess.run(
        ["git", "diff", "-U0", "13dbae0", "--", "native/documents.rs"],
        cwd=repo_root,
        stdout=subprocess.PIPE,
        text=True,
        check=True,
    )
    doc_minus = [
        line
        for line in doc_diff_proc.stdout.splitlines()
        if line.startswith("-") and not line.startswith("---")
    ]
    doc_plus = [
        line
        for line in doc_diff_proc.stdout.splitlines()
        if line.startswith("+") and not line.startswith("+++")
    ]
    if len(doc_minus) != 1 or len(doc_plus) != 1:
        print(
            f"FAIL: native/documents.rs diff not exactly -1/+1 (-{len(doc_minus)}/+{len(doc_plus)})"
        )
        sys.exit(1)

    # Check 4: diff.patch touches exactly 3 test files and contains specific strings
    patch_path = red_dir / "diff.patch"
    if not patch_path.is_file():
        print("FAIL: red/diff.patch missing")
        sys.exit(1)
    patch_text = patch_path.read_text(encoding="utf-8")
    patch_files = re.findall(r"^diff --git a/(.*?) b/", patch_text, re.M)
    expected_patch_files = [
        "native/documents.rs",
        "native/filesystem.rs",
        "test/native.test.mjs",
    ]
    if sorted(patch_files) != sorted(expected_patch_files):
        print(f"FAIL: diff.patch touched files {patch_files} != {expected_patch_files}")
        sys.exit(1)
    if 'b"valid\\n\\xff\\x00"' not in patch_text:
        print('FAIL: diff.patch does not contain b"valid\\n\\xff\\x00"')
        sys.exit(1)
    if "[65,10,0xff,0x00]" not in patch_text:
        print("FAIL: diff.patch does not contain [65,10,0xff,0x00]")
        sys.exit(1)

    # Check 5: .exit files
    def read_exit(name: str) -> int:
        p = red_dir / f"{name}.exit"
        if not p.is_file():
            print(f"FAIL: {name}.exit missing")
            sys.exit(1)
        return int(p.read_text(encoding="utf-8").strip())

    cargo_exit = read_exit("cargo-test")
    build_exit = read_exit("build")
    native_exit = read_exit("native-test")
    ac18_cargo_exit = read_exit("ac18-cargo")
    ac18_native_exit = read_exit("ac18-native")

    if cargo_exit == 0:
        print("FAIL: cargo-test.exit is 0, expected non-zero")
        sys.exit(1)
    if build_exit != 0:
        print(f"FAIL: build.exit is {build_exit}, expected 0")
        sys.exit(1)
    if native_exit == 0:
        print("FAIL: native-test.exit is 0, expected non-zero")
        sys.exit(1)
    if ac18_cargo_exit != 0:
        print(f"FAIL: ac18-cargo.exit is {ac18_cargo_exit}, expected 0")
        sys.exit(1)
    if ac18_native_exit != 0:
        print(f"FAIL: ac18-native.exit is {ac18_native_exit}, expected 0")
        sys.exit(1)

    # Check 6: local-files sha256
    bin_file = red_dir / "local-files"
    sha_file = red_dir / "local-files.sha256"
    if not bin_file.is_file() or not sha_file.is_file():
        print("FAIL: red/local-files or red/local-files.sha256 missing")
        sys.exit(1)
    expected_bin_sha = sha_file.read_text(encoding="utf-8").strip().split()[0]
    actual_bin_sha = sha256_file(bin_file)
    if actual_bin_sha != expected_bin_sha:
        print(
            f"FAIL: red/local-files sha256 {actual_bin_sha} != {expected_bin_sha}"
        )
        sys.exit(1)

    # Check 7: cargo-test.log
    cargo_log = (red_dir / "cargo-test.log").read_text(encoding="utf-8")
    matches = re.findall(
        r"^test filesystem::tests::(cp1251_\w+) \.\.\. (ok|FAILED)$",
        cargo_log,
        re.M,
    )
    if len(matches) != 9:
        print(f"FAIL: cargo-test.log has {len(matches)} cp1251 tests, expected 9")
        sys.exit(1)
    for test_name, status in matches:
        if test_name.startswith("cp1251_lock_"):
            if status != "ok":
                print(f"FAIL: Lock test {test_name} failed in cargo-test.log")
                sys.exit(1)
        else:
            if status != "FAILED":
                print(
                    f"FAIL: Non-lock test {test_name} passed in cargo-test.log on RED"
                )
                sys.exit(1)
    if "error[E" in cargo_log:
        print("FAIL: cargo-test.log contains compilation error (error[E)")
        sys.exit(1)
    if "NOT_TEXT" not in cargo_log:
        print("FAIL: cargo-test.log does not contain NOT_TEXT")
        sys.exit(1)

    # Check 8: native-test.log
    # Verify in-memory negative probes: flipped outcomes and timeout must be rejected
    flipped_probe = (
        "✔ cp1251: Windows-1251 text is read and searched by every tool (654ms)\n"
        "✖ cp1251 lock: UTF-8, BOM, binary, limits and access rules are unchanged (44ms)\n"
        "ℹ pass 1\nℹ fail 1\n"
        "✖ failing tests:\n"
        "✖ cp1251 lock: UTF-8, BOM, binary, limits and access rules are unchanged (44ms)\n"
        "  AssertionError [ERR_ASSERTION]\n"
    )
    if check_native_test_log(flipped_probe)[0]:
        print("FAIL: Parser admitted flipped outcomes in-memory probe")
        sys.exit(1)

    timeout_probe = (
        "✖ cp1251: Windows-1251 text is read and searched by every tool (5000ms)\n"
        "✔ cp1251 lock: UTF-8, BOM, binary, limits and access rules are unchanged (44ms)\n"
        "ℹ pass 1\nℹ fail 1\n"
        "✖ failing tests:\n"
        "✖ cp1251: Windows-1251 text is read and searched by every tool (5000ms)\n"
        "  Error: test timed out after 5000ms\n"
    )
    if check_native_test_log(timeout_probe)[0]:
        print("FAIL: Parser admitted timeout in-memory probe")
        sys.exit(1)

    tap_flipped_probe = (
        "TAP version 13\n"
        "ok 1 - cp1251: Windows-1251 text is read and searched by every tool\n"
        "not ok 2 - cp1251 lock: UTF-8, BOM, binary, limits and access rules are unchanged\n"
        "1..2\n# tests 2\n# pass 1\n# fail 1\n"
    )
    if check_native_test_log(tap_flipped_probe)[0]:
        print("FAIL: Parser admitted TAP flipped outcomes in-memory probe")
        sys.exit(1)

    tap_timeout_probe = (
        "TAP version 13\n"
        "not ok 1 - cp1251: Windows-1251 text is read and searched by every tool\n"
        "  ---\n  failureType: 'testTimeoutFailure'\n  error: 'test timed out'\n  ...\n"
        "ok 2 - cp1251 lock: UTF-8, BOM, binary, limits and access rules are unchanged\n"
        "1..2\n# tests 2\n# pass 1\n# fail 1\n"
    )
    if check_native_test_log(tap_timeout_probe)[0]:
        print("FAIL: Parser admitted TAP timeout in-memory probe")
        sys.exit(1)

    native_log = (red_dir / "native-test.log").read_text(encoding="utf-8")
    ok, err_msg = check_native_test_log(native_log)
    if not ok:
        print(f"FAIL: {err_msg}")
        sys.exit(1)

    # Check 9: ac18 logs
    ac18_cargo_log = (red_dir / "ac18-cargo.log").read_text(encoding="utf-8")
    if " 1 passed;" not in ac18_cargo_log:
        print("FAIL: ac18-cargo.log does not contain ' 1 passed;'")
        sys.exit(1)
    ac18_native_log = (red_dir / "ac18-native.log").read_text(encoding="utf-8")
    if not re.search(r"^(?:ℹ|#|\u2139)?\s*pass 1\b", ac18_native_log, re.M):
        print("FAIL: ac18-native.log does not contain pass 1")
        sys.exit(1)
    if not re.search(r"^(?:ℹ|#|\u2139)?\s*fail 0\b", ac18_native_log, re.M):
        print("FAIL: ac18-native.log does not contain fail 0")
        sys.exit(1)

    # Check 10: guards
    dist_before = guards_dir / "dist-before.txt"
    pgrep_before = guards_dir / "pgrep-before.txt"
    if not dist_before.is_file() or dist_before.stat().st_size == 0:
        print("FAIL: guards/dist-before.txt is missing or empty")
        sys.exit(1)
    if not pgrep_before.is_file():
        print("FAIL: guards/pgrep-before.txt is missing")
        sys.exit(1)

    # Check 11: live run of cp1251 lock with red/local-files
    red_bin = str(red_dir / "local-files")
    live_env = os.environ.copy()
    live_env["NATIVE_BINARY"] = red_bin
    live_proc = subprocess.run(
        [
            "node",
            "--test",
            "--test-name-pattern",
            "cp1251 lock",
            "test/native.test.mjs",
        ],
        cwd=repo_root,
        env=live_env,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
    )
    if live_proc.returncode != 0:
        print(
            f"FAIL: Live test of 'cp1251 lock' with red/local-files returned {live_proc.returncode}:\n{live_proc.stdout}\n{live_proc.stderr}"
        )
        sys.exit(1)

    print("red verification passed.")


def record_local(repo_root: Path):
    green_dir = repo_root / "artifacts/cp1251-fix/green-local"
    green_dir.mkdir(parents=True, exist_ok=True)

    commands = [
        ("fmt", ["cargo", "fmt", "--check"], None),
        (
            "clippy",
            ["cargo", "clippy", "--locked", "--all-targets", "--", "-D", "warnings"],
            None,
        ),
        ("cargo-test", ["cargo", "test", "--locked"], None),
        (
            "build",
            ["cargo", "build", "--release", "--locked", "--bin", "local-files"],
            None,
        ),
        (
            "native-serve",
            ["node", "--test", "test/native.test.mjs"],
            {"NATIVE_BINARY": "target/release/local-files"},
        ),
        (
            "native-worker",
            ["node", "--test", "test/native.test.mjs"],
            {"NATIVE_BINARY": "target/release/local-files", "NATIVE_WORKER": "1"},
        ),
        ("npm-check", ["npm", "run", "check"], None),
        ("cargo-cp1251", ["cargo", "test", "--locked", "cp1251"], None),
        (
            "native-cp1251",
            [
                "node",
                "--test",
                "--test-name-pattern",
                "cp1251",
                "test/native.test.mjs",
            ],
            {"NATIVE_BINARY": "target/release/local-files"},
        ),
    ]

    for name, cmd, env in commands:
        log_file = green_dir / f"{name}.log"
        exit_file = green_dir / f"{name}.exit"
        print(f"Running {name}...")
        run_command_with_log(cmd, log_file, exit_file, repo_root, env)

    fp = compute_fingerprint(repo_root)
    (green_dir / "fingerprint.json").write_text(
        json.dumps(fp, indent=2) + "\n", encoding="utf-8"
    )
    print("record-local completed successfully.")


def check_frozen_tests(repo_root: Path, red_ac3_fp: dict):
    """Allow only the two accepted AC3 literals and six Windows fixture names."""
    accepted = "43ccffb573e75642776fd065afe7d02eccc8819e"

    def source(revision, path):
        return subprocess.check_output(
            ["git", "show", f"{revision}:{path}"], cwd=repo_root
        ).decode("utf-8")

    def tests(path, content):
        if path.endswith(".rs"):
            marker = "mod tests {"
            if marker not in content:
                raise ValueError(f"Missing test block: {path}")
            return content[content.index(marker):]
        return content

    corrections = {
        "native/filesystem.rs": ('assert_eq!(r2["text"], "Посто");', 'assert_eq!(r2["text"], "Поста");'),
        "test/native.test.mjs": ("assert.equal(mc.structuredContent.text, 'Посто');", "assert.equal(mc.structuredContent.text, 'Поста');"),
    }
    for path in ["native/filesystem.rs", "native/documents.rs", "test/native.test.mjs"]:
        f1 = tests(path, source("ef522b1", path))
        ac3 = tests(path, source(accepted, path))
        expected_ac3 = f1
        if path in corrections:
            old, new = corrections[path]
            if f1.count(old) != 1:
                raise ValueError(f"Unexpected F1 AC3 literal: {path}")
            expected_ac3 = f1.replace(old, new)
        if ac3 != expected_ac3:
            raise ValueError(f"Accepted AC3 delta differs from frozen F1: {path}")
        if hashlib.sha256(ac3.encode("utf-8")).hexdigest() != red_ac3_fp["tests"][path]:
            raise ValueError(f"Immutable red-ac3 test fingerprint mismatch: {path}")
        expected = ac3
        if path == "native/filesystem.rs":
            if ac3.count('"nul.txt"') != 3:
                raise ValueError("Expected exactly three frozen Rust NUL fixture literals")
            expected = ac3.replace('"nul.txt"', '"nul-control.txt"')
        elif path == "test/native.test.mjs":
            if ac3.count("'nul.txt'") != 1 or ac3.count("cp1251-controls/nul.txt") != 2:
                raise ValueError("Expected exactly three frozen Node NUL fixture literals")
            expected = ac3.replace("'nul.txt'", "'nul-control.txt'").replace(
                "cp1251-controls/nul.txt", "cp1251-controls/nul-control.txt"
            )
        current = tests(path, (repo_root / path).read_text(encoding="utf-8"))
        if current != expected:
            raise ValueError(f"Unapproved frozen test change: {path}")

    # The portability prerequisite must not alter any production source byte.
    native_paths = subprocess.check_output(
        ["git", "ls-tree", "-r", "--name-only", accepted, "--", "native"],
        cwd=repo_root, text=True,
    ).splitlines()
    for path in native_paths:
        original = subprocess.check_output(["git", "show", f"{accepted}:{path}"], cwd=repo_root)
        current = (repo_root / path).read_bytes()
        if path in {"native/filesystem.rs", "native/documents.rs"}:
            original = original.split(b"mod tests {", 1)[0]
            current = current.split(b"mod tests {", 1)[0]
        if original != current:
            raise ValueError(f"Production source differs from accepted F2: {path}")


def verify_local(repo_root: Path):
    green_dir = repo_root / "artifacts/cp1251-fix/green-local"
    if not green_dir.is_dir():
        print("FAIL: artifacts/cp1251-fix/green-local missing")
        sys.exit(1)

    # 1. Fingerprint matches green-local/fingerprint.json
    fp_path = green_dir / "fingerprint.json"
    if not fp_path.is_file():
        print("FAIL: green-local/fingerprint.json missing")
        sys.exit(1)
    expected_fp = json.loads(fp_path.read_text(encoding="utf-8"))
    actual_fp = compute_fingerprint(repo_root)
    if actual_fp != expected_fp:
        print(
            "FAIL: Current draft fingerprint does not match green-local/fingerprint.json"
        )
        sys.exit(1)

    # 2. Preserve immutable F1/AC3 evidence and allow only the reviewed NUL-name correction.
    red_ac3_fp_path = repo_root / "artifacts/cp1251-fix/red-ac3/fingerprint.json"
    if not red_ac3_fp_path.is_file():
        print("FAIL: artifacts/cp1251-fix/red-ac3/fingerprint.json missing")
        sys.exit(1)
    red_ac3_fp = json.loads(red_ac3_fp_path.read_text(encoding="utf-8"))
    try:
        check_frozen_tests(repo_root, red_ac3_fp)
    except ValueError as exc:
        print(f"FAIL: {exc}")
        sys.exit(1)

    # 3. All nine .exit contain 0
    exit_names = [
        "fmt",
        "clippy",
        "cargo-test",
        "build",
        "native-serve",
        "native-worker",
        "npm-check",
        "cargo-cp1251",
        "native-cp1251",
    ]
    for name in exit_names:
        exit_file = green_dir / f"{name}.exit"
        if not exit_file.is_file():
            print(f"FAIL: {name}.exit missing in green-local")
            sys.exit(1)
        val = exit_file.read_text(encoding="utf-8").strip()
        if val != "0":
            print(f"FAIL: {name}.exit is {val}, expected 0")
            sys.exit(1)

    # 4. Changed and new files outside docs/ relative to 13dbae0
    status_proc = subprocess.run(
        ["git", "status", "--porcelain"],
        cwd=repo_root,
        stdout=subprocess.PIPE,
        text=True,
        check=True,
    )
    status_files = set()
    for line in status_proc.stdout.splitlines():
        if line.strip():
            path_str = line[3:].strip()
            if " -> " in path_str:
                path_str = path_str.split(" -> ")[1].strip()
            status_files.add(path_str)

    diff_proc = subprocess.run(
        ["git", "diff", "--name-only", "13dbae0"],
        cwd=repo_root,
        stdout=subprocess.PIPE,
        text=True,
        check=True,
    )
    diff_files = set(filter(None, diff_proc.stdout.splitlines()))

    non_docs = {f for f in (status_files | diff_files) if not f.startswith("docs/")}
    expected_allowed = {
        "native/text.rs",
        "native/main.rs",
        "native/filesystem.rs",
        "native/documents.rs",
        "native/mcp.rs",
        "README.md",
        "test/native.test.mjs",
        "scripts/verify-cp1251.py",
    }
    if not (non_docs <= expected_allowed):
        print(
            f"FAIL: Changed/new files outside docs/ {non_docs} not a subset of {expected_allowed}"
        )
        sys.exit(1)

    # 5. git diff 13dbae0 is empty for specified files
    diff_untouched = subprocess.run(
        [
            "git",
            "diff",
            "13dbae0",
            "--",
            "Cargo.toml",
            "Cargo.lock",
            "native/doc_search.rs",
            "native/cursor.rs",
            "native/walk.rs",
        ],
        cwd=repo_root,
        stdout=subprocess.PIPE,
        text=True,
        check=True,
    ).stdout.strip()
    if diff_untouched:
        print(
            f"FAIL: git diff 13dbae0 is not empty for protected files:\n{diff_untouched}"
        )
        sys.exit(1)

    # 6. No const modifications in native/ (AC15)
    diff_native = subprocess.run(
        ["git", "diff", "13dbae0", "--", "native/"],
        cwd=repo_root,
        stdout=subprocess.PIPE,
        text=True,
        check=True,
    ).stdout
    const_pattern = re.compile(
        r"^[-+].*const (TEXT_LIMIT|BINARY_LIMIT|SCAN_LIMIT|OUTPUT_LIMIT|WALK_LIMIT|TEXT_READ_LIMIT|SEARCH_DEADLINE|DOC_SCAN_LIMIT|SEARCH_DOC_FILE_LIMIT|DOC_DEFAULT_CHARS|ZIP_ENTRY_LIMIT|ZIP_RATIO_LIMIT|CONTAINER_DEPTH)",
        re.M,
    )
    if const_pattern.search(diff_native):
        print("FAIL: AC15 consts modified in native/")
        sys.exit(1)

    # 7. native/mcp.rs contains four phrases
    mcp_text = (repo_root / "native/mcp.rs").read_text(encoding="utf-8")
    mcp_phrases = [
        "search in UTF-8 or Windows-1251 text;",
        "Read UTF-8 or Windows-1251 text or selected lines.",
        "Read UTF-8/Windows-1251 text, PDF,",
        "plain text (UTF-8 or Windows-1251).",
    ]
    for phrase in mcp_phrases:
        if phrase not in mcp_text:
            print(f"FAIL: native/mcp.rs missing phrase: {phrase}")
            sys.exit(1)

    # 8. README.md contains three table lines and four substrings
    readme_text = (repo_root / "README.md").read_text(encoding="utf-8")
    table_lines = [
        "| `search_text` | Поиск в UTF-8 или Windows-1251, PDF, ",
        "| UTF-8 или Windows-1251 текст | Строки с номерами |",
        "| UTF-8 или Windows-1251 текст — до 64 МиБ, потоковая проверка всего файла |",
    ]
    for tl in table_lines:
        if tl not in readme_text:
            print(f"FAIL: README.md missing table line matching: {tl}")
            sys.exit(1)

    readme_substrings = [
        "0x00–0x08, 0x0B, 0x0E–0x1F, 0x7F, 0x98",
        "EF BB BF",
        "skippedInaccessible",
        "`encoding`",
    ]
    for sub in readme_substrings:
        if sub not in readme_text:
            print(f"FAIL: README.md missing substring: {sub}")
            sys.exit(1)

    # 9. Live run of node --test --test-name-pattern cp1251 test/native.test.mjs with NATIVE_WORKER=1 gives 0
    live_env = os.environ.copy()
    live_env["NATIVE_BINARY"] = str(repo_root / "target/release/local-files")
    live_env["NATIVE_WORKER"] = "1"
    live_proc = subprocess.run(
        [
            "node",
            "--test",
            "--test-name-pattern",
            "cp1251",
            "test/native.test.mjs",
        ],
        cwd=repo_root,
        env=live_env,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
    )
    if live_proc.returncode != 0:
        print(
            f"FAIL: Live test of cp1251 with NATIVE_WORKER=1 failed with code {live_proc.returncode}:\n{live_proc.stdout}\n{live_proc.stderr}"
        )
        sys.exit(1)

    print("local verification passed.")



# F3: independent recomputation of the evidence contract.
SOURCE = '86a9e5157f42db70d553dbb70d90ff7388e6ed0b'
BASELINE_SHA = '8b0b32a9d8239a361cccb237ad4599b2a0bf2ebeeba1b01d28bfe07c32520f0f'
TEXT = 'Постановление Верховного Суда РФ\r\nЗамещающие сделки: взыскание убытков.\r\nОпределение от 03.03.2023 N 305-ЭС22-2377\r\n'
TYPO = '«Определение Судебной коллегии» — дело № 305-ЭС22-2377…\r\nУчёт: ЁМКОСТЬ, ёмкость; „цитата“ – § 3,\xa0стр. 2\r\nЁлкин А.Ё. и ёлкин а.ё.\r\n'
METHODS = ['read_text_file', 'read_file', 'read_document', 'inspect_document', 'fetch', 'search_text']
NAMES = ['cp1251.txt', 'utf8.txt', 'utf8-bom.txt', 'cp1251-typography.txt', 'utf8-also-cp1251.txt', 'bom-cp1251.txt', 'damaged-utf8.txt', 'cp1251-nbsp.txt', 'binary.txt']
EXTRA = ['range', 'maxchars', 'invalid-range', 'cursor', 'yo', 'folder', 'folder-search-line3', 'folder-nbsp', 'connector', 'connector-nbsp', 'dat', 'bin', 'controls']

def synthetic():
    cp = TEXT.encode('cp1251')
    u = TEXT.encode('utf-8')
    return {'cp1251.txt': cp, 'utf8.txt': u, 'utf8-bom.txt': b'\xef\xbb\xbf' + u, 'cp1251-typography.txt': TYPO.encode('cp1251'), 'utf8-also-cp1251.txt': bytes([194, 187]), 'bom-cp1251.txt': b'\xef\xbb\xbf' + cp, 'damaged-utf8.txt': u.replace(b'\r\n', b'\r\n\xff', 1), 'cp1251-nbsp.txt': 'В\xa02024, В\xa02025 — 100'.encode('cp1251'), 'binary.txt': bytes(range(256)), 'binary.bin': bytes(range(256)), 'cp1251.dat': cp}

def query(name):
    return {'cp1251-typography.txt': 'ёмкость', 'utf8-also-cp1251.txt': '»', 'damaged-utf8.txt': '03.03.2023', 'cp1251-nbsp.txt': '2025 — 100'}.get(name, 'Замещающие')

def args_for(tool, name):
    a = {'id': 'probe:' + name} if tool == 'fetch' else {'root': 'probe', 'path': name}
    if tool == 'search_text':
        a['query'] = query(name)
    return a

def catalog():
    c = {t + ':' + n: [(t, args_for(t, n))] for n in NAMES for t in METHODS}

    def read(**kw):
        return dict(root='probe', path='cp1251.txt', **kw)

    def search(q):
        return {'root': 'probe', 'query': q}
    c.update({'range': [('read_text_file', read(startLine=2, endLine=2))], 'maxchars': [('read_text_file', read(maxChars=5))], 'invalid-range': [('read_text_file', read(startLine=3, endLine=2))], 'cursor': [('read_document', read(maxChars=1))], 'yo': [('search_text', {'root': 'probe', 'path': 'cp1251-typography.txt', 'query': 'ЁЛКИН'})], 'folder': [('search_text', search('Замещающие'))], 'folder-search-line3': [('search_text', search('03.03.2023'))], 'folder-nbsp': [('search_text', search('2025 — 100'))], 'connector': [('search', {'query': 'Замещающие'}), ('fetch', {'id': 'probe:cp1251.txt'})], 'connector-nbsp': [('search', {'query': '2025 — 100'})], 'dat': [('read_text_file', {'root': 'probe', 'path': 'cp1251.dat'}), ('read_file', {'root': 'probe', 'path': 'cp1251.dat'})], 'bin': [('read_file', {'root': 'probe', 'path': 'binary.bin'})], 'controls': [('read_document', {'root': 'probe', 'path': n}) for n in ['control.docx', 'control.eml']]})
    return c

def body(d):
    (h, sep, b) = d['text'].partition('\n')
    if not sep or not h.startswith('[source: '):
        raise AssertionError('missing source header')
    return b

def evaluate(results):
    checks = []

    def check(label, fn):
        try:
            fn()
            checks.append({'check': label, 'pass': True})
        except Exception as e:
            checks.append({'check': label, 'pass': False, 'detail': str(e) or type(e).__name__})

    def require(value, message='expectation mismatch'):
        if not value:
            raise AssertionError(message)
    expected_catalog = catalog()
    runs = {}
    for r in results['runs']:
        key = (r['exe'], r['mode'])
        require(key not in runs, 'duplicate run')
        runs[key] = r
    wanted = {(e, m) for e in (['snapshot', 'baseline'] if results['baseline']['status'] == 'run' else ['snapshot']) for m in ['serve', 'worker']}
    require(set(runs) == wanted, 'required runs')
    for ((exe, mode), r) in runs.items():
        prefix = exe + '/' + mode + '/'
        require(r['exit_code'] == 0 and type(r['exit_code']) is int, 'process exit')
        require(isinstance(r['sha256'], str) and len(r['sha256']) == 64, 'binary hash')
        if exe == 'baseline':
            require(r['sha256'] == BASELINE_SHA, 'baseline hash')
        require(set(r['cases']) == set(expected_catalog), 'case catalog')
        for (case, calls) in r['cases'].items():
            require(isinstance(calls, list) and calls, 'empty calls')
            for (i, call) in enumerate(calls):
                require(call['response'].get('jsonrpc') == '2.0' and 'result' in call['response'], 'raw JSON-RPC result')
                if case != 'cursor':
                    require(i < len(expected_catalog[case]), 'extra call')
                    (t, a) = expected_catalog[case][i]
                    require(call['tool'] == t and call['args'] == a, 'call args')
            if case != 'cursor':
                require(len(calls) == len(expected_catalog[case]), 'call count')

        def numeric_fields(value):
            if isinstance(value, dict):
                for key, item in value.items():
                    if key in {'line', 'lines', 'totalLines', 'startLine', 'endLine', 'size', 'chars', 'skippedInaccessible', 'skippedLargeFiles'}:
                        require(type(item) is int, 'integer required: ' + key)
                    numeric_fields(item)
            elif isinstance(value, list):
                for item in value:
                    numeric_fields(item)

        def data(case, i=0):
            res = r['cases'][case][i]['response']['result']
            require('isError' not in res, 'unexpected isError on success')
            numeric_fields(res['structuredContent'])
            return res['structuredContent']

        def error(case, code, message=None, i=0):
            res = r['cases'][case][i]['response']['result']
            require(res.get('isError') is True)
            e = res['structuredContent']['error']
            require(e['code'] == code)
            require(message is None or e['message'] == message)

        def blob(case, want, i=0):
            d = data(case, i)
            require(d['kind'] == 'binary')
            content = r['cases'][case][i]['response']['result']['content']
            blobs = [x['resource']['blob'] for x in content if x.get('type') == 'resource' and 'blob' in x.get('resource', {})]
            require(len(blobs) == 1 and base64.b64decode(blobs[0], validate=True) == want)
        if exe == 'baseline':
            for n in ['cp1251.txt', 'cp1251-typography.txt', 'cp1251-nbsp.txt', 'damaged-utf8.txt']:
                for t in METHODS:
                    check(prefix + t + ':' + n, lambda t=t, n=n: error(t + ':' + n, 'NOT_TEXT'))

            def baseline_folder():
                d = data('folder')
                require('cp1251.txt' not in {x['relativePath'] for x in d['results']})
                require(d['skippedInaccessible'] == 0)
            check(prefix + 'folder', baseline_folder)
            for n in ['utf8.txt', 'utf8-bom.txt']:
                for t in ['read_text_file', 'read_file', 'read_document', 'fetch']:

                    def eq(t=t, n=n):
                        a = data(t + ':' + n)
                        s = runs['snapshot', mode]['cases'][t + ':' + n][0]['response']['result']['structuredContent']
                        require((body(a) if t in ['read_document', 'fetch'] else a['text']) == (body(s) if t in ['read_document', 'fetch'] else s['text']))
                    check(prefix + t + ':' + n, eq)
            continue
        for n in NAMES:
            for t in METHODS:

                def matrix(n=n, t=t):
                    case = t + ':' + n
                    if n in ['bom-cp1251.txt', 'binary.txt']:
                        error(case, 'NOT_TEXT', 'File looks like damaged UTF-8' if n == 'bom-cp1251.txt' else 'File is neither UTF-8 nor Windows-1251 text')
                        return
                    if t == 'search_text' and n == 'utf8-also-cp1251.txt':
                        error(case, 'INVALID_ARGUMENT')
                        return
                    d = data(case)
                    enc = 'utf-8' if n in ['utf8.txt', 'utf8-bom.txt', 'utf8-also-cp1251.txt'] else 'windows-1251'
                    raw = synthetic()[n]
                    e = raw.decode('utf-8' if enc == 'utf-8' else 'cp1251').replace('\r\n', '\n')
                    lines = e.count('\n') + 1
                    if t == 'search_text':
                        found = d['results']
                        count = 2 if n == 'cp1251-typography.txt' else 1
                        line = 3 if n == 'damaged-utf8.txt' else 1 if n == 'cp1251-nbsp.txt' else 2
                        require(len(found) == count and all((x['locator']['line'] == line for x in found)))
                        if n in ['cp1251.txt', 'utf8.txt', 'utf8-bom.txt']:
                            require(found[0]['snippet'] == 'Замещающие сделки: взыскание убытков.')
                    elif t == 'fetch':
                        require(d['metadata']['format'] == 'text' and d['metadata']['encoding'] == enc and (body(d) == e))
                    else:
                        require(d['encoding'] == enc)
                        if t == 'read_text_file':
                            require(d['text'] == e and d['totalLines'] == lines)
                        if t == 'read_file':
                            require(d['kind'] == 'text' and d['text'] == e)
                            require(not any(('blob' in x.get('resource', {}) for x in r['cases'][case][0]['response']['result'].get('content', []))))
                        if t in ['read_document', 'inspect_document']:
                            require(d['format'] == 'text' and d['size'] == len(raw) and (d['lines'] == lines))
                        if t == 'read_document':
                            require(body(d) == e)
                check(prefix + t + ':' + n, matrix)

        def additional():
            d = data('range')
            require(d['text'] == TEXT.replace('\r\n', '\n').split('\n')[1] and d['startLine'] == d['endLine'] == 2)
            d = data('maxchars')
            require(d['text'] == 'Поста' and d['truncated'] is True)
            error('invalid-range', 'INVALID_RANGE')
            calls = r['cases']['cursor']
            recovered = {}
            previous = None
            for (i, c) in enumerate(calls):
                want = {'root': 'probe', 'path': 'cp1251.txt', 'maxChars': 1}
                if previous:
                    want['cursor'] = previous
                require(c['tool'] == 'read_document' and c['args'] == want, 'cursor request')
                d = data('cursor', i)
                b = body(d)
                offset = 0
                for unit in d['units']:
                    require(type(unit['line']) is int and type(unit['chars']) is int and (unit['chars'] >= 0))
                    k = unit['line']
                    recovered[k] = recovered.get(k, '') + b[offset:offset + unit['chars']]
                    offset += unit['chars']
                previous = d.get('nextCursor')
                require(bool(previous) == (i < len(calls) - 1), 'cursor termination')
            require(set(recovered).issubset(set(range(1, 5))))
            require([recovered.get(i, '') for i in range(1, 5)] == TEXT.replace('\r\n', '\n').split('\n'), 'cursor text')
            d = data('yo')
            require(len(d['results']) == 2 and all((x['locator']['line'] == 3 for x in d['results'])))
            d = data('folder')
            require({x['relativePath'] for x in d['results']} == {'cp1251.txt', 'utf8.txt', 'utf8-bom.txt', 'control.docx', 'control.eml'})
            require(d['skippedInaccessible'] == 2 and d['skippedLargeFiles'] == 0)
            require(all((x['line'] == 2 for x in d['results'] if x['relativePath'] == 'cp1251.txt')))
            require(any((x['relativePath'] == 'cp1251.txt' and x['line'] == 3 for x in data('folder-search-line3')['results'])))
            d = data('folder-nbsp')
            require(len(d['results']) == 1 and d['results'][0]['relativePath'] == 'cp1251-nbsp.txt' and (d['results'][0]['line'] == 1))
            require('probe:cp1251.txt' in {x['id'] for x in data('connector')['results']})
            require(body(data('connector', 1)) == TEXT.replace('\r\n', '\n'))
            require('probe:cp1251-nbsp.txt' in {x['id'] for x in data('connector-nbsp')['results']})
            error('dat', 'NOT_TEXT', 'File is not valid UTF-8. Use read_document for document formats.')
            blob('dat', synthetic()['cp1251.txt'], 1)
            blob('bin', bytes(range(256)))
            for i in range(2):
                require('Замещающие сделки' in body(data('controls', i)))
        check(prefix + 'additional', additional)
    snapshot_ok = all((c['pass'] for c in checks if c['check'].startswith('snapshot/')))
    baseline_ok = all((c['pass'] for c in checks if c['check'].startswith('baseline/')))
    return {'verdicts': {'snapshot': 'GREEN_NATIVE_WINDOWS' if snapshot_ok else 'FAILED', 'baseline': ('BASELINE_REPRODUCED_2.2.0' if baseline_ok else 'FAILED') if results['baseline']['status'] == 'run' else 'BASELINE_NOT_RUN'}, 'checks': checks}

def verify_hpc(repo_root):
    import base64
    from datetime import datetime, timezone
    evidence = repo_root / 'artifacts/cp1251-fix/hpc-green'

    def require(ok, why):
        if not ok:
            raise ValueError(why)

    def unique(pairs):
        obj = {}
        for (k, v) in pairs:
            require(k not in obj, 'Duplicate JSON key: ' + k)
            obj[k] = v
        return obj

    def read(name):
        return json.loads((evidence / name).read_text(encoding='utf-8'), object_pairs_hook=unique)

    def git(*args):
        return subprocess.check_output(['git', *args], cwd=repo_root, text=True).rstrip('\r\n')

    def time(value):
        require(isinstance(value, str), 'ISO UTC string required')
        d = datetime.fromisoformat(value.replace('Z', '+00:00'))
        require(d.tzinfo is not None and d.utcoffset().total_seconds() == 0, 'UTC required')
        return d
    snap = read('snapshot.json')
    fixtures = read('fixtures.json')
    results = read('results.json')
    stored = read('verification.json')
    preserve = read('preservation.json')
    keys = {'fmt', 'clippy', 'test', 'build', 'npm_ci', 'native_serve', 'native_worker'}
    require(set(snap['exit_codes']) == keys and all((type(v) is int and v == 0 for v in snap['exit_codes'].values())), 'seven successful native gates required')
    require(set(snap['versions']) == {'os', 'rustc', 'cargo', 'node', 'python'} and all((isinstance(v, str) and v.strip() for v in snap['versions'].values())), 'versions required')
    require('windows' in snap['versions']['os'].lower() and 'x86_64-pc-windows-msvc' in snap['versions']['rustc'], 'native Windows MSVC required')
    require(isinstance(snap['stage'], str) and 'local-files-cp1251-green-' in snap['stage'], 'isolated stage required')
    require(time(snap['clone_started_utc']) <= time(snap['gates_finished_utc']), 'gate times')
    bundle = evidence / 'cp1251.bundle'
    bundle_sha = sha256_file(bundle)
    require((evidence / 'cp1251.bundle.sha256').read_text().split() == [bundle_sha, 'cp1251.bundle'], 'bundle checksum file')
    require(bundle_sha == snap['bundle_sha256'], 'remote bundle checksum')
    heads = dict(((ref, h) for (h, ref) in (line.split() for line in git('bundle', 'list-heads', str(bundle)).splitlines())))
    require(heads.get('refs/heads/fix/cp1251-txt') == snap['head'] == SOURCE, 'immutable accepted F2 source')
    require(snap['status_porcelain'] == '', 'snapshot must be clean')
    subprocess.run(['git', 'merge-base', '--is-ancestor', SOURCE, 'HEAD'], cwd=repo_root, check=True)
    subprocess.run(['git', 'merge-base', '--is-ancestor', '13dbae0fa651aeb8acca53b28efc0f58cd259c63', SOURCE], cwd=repo_root, check=True)
    relevant = ['native', 'test', 'README.md', 'Cargo.toml', 'Cargo.lock', 'package.json', 'package-lock.json']
    require(git('diff', SOURCE, '--', *relevant) == '', 'product source changed after accepted F2')
    check_frozen_tests(repo_root, json.loads((repo_root / 'artifacts/cp1251-fix/red-ac3/fingerprint.json').read_text(encoding='utf-8')))
    expected = synthetic()
    require(set(fixtures) == set(expected) | {'control.docx', 'control.eml'}, 'all thirteen fixtures')
    for (n, v) in fixtures.items():
        require(set(v) == {'bytes', 'sha256'} and type(v['bytes']) is int and (v['bytes'] > 0) and isinstance(v['sha256'], str) and re.fullmatch('[0-9a-f]{64}', v['sha256']), 'fixture schema: ' + n)
    for (n, b) in expected.items():
        require(fixtures[n] == {'bytes': len(b), 'sha256': hashlib.sha256(b).hexdigest()}, 'fixture bytes: ' + n)
    red = json.loads((repo_root / 'artifacts/cp1251-hpc-20261030/results.json').read_text())
    for n in ['cp1251.txt', 'utf8.txt', 'utf8-bom.txt']:
        require(fixtures[n] == red['fixtures'][n], 'RED fixture equality: ' + n)
    baseline = results['baseline']
    require(baseline['status'] in {'run', 'missing', 'sha_mismatch'} and isinstance(baseline['path'], str) and baseline['path'], 'baseline status')
    require(baseline['status'] == 'run' and baseline['sha256'] == BASELINE_SHA or (baseline['status'] == 'missing' and baseline['sha256'] is None) or (baseline['status'] == 'sha_mismatch' and isinstance(baseline['sha256'], str) and re.fullmatch('[0-9a-f]{64}', baseline['sha256']) and (baseline['sha256'] != BASELINE_SHA)), 'baseline identity')
    require(time(results['started_utc']) >= time(snap['gates_finished_utc']) and time(results['finished_utc']) >= time(results['started_utc']), 'run times')
    for r in results['runs']:
        require(re.fullmatch('[0-9a-f]{64}', r['sha256']) is not None, 'EXE SHA256')
    computed = evaluate(results)
    require(all((x['pass'] for x in computed['checks'])), 'raw response expectations failed: ' + str([x for x in computed['checks'] if not x['pass']]))
    require(stored == computed, 'verification.json must match recomputed checks and verdicts')
    require(computed['verdicts']['snapshot'] == 'GREEN_NATIVE_WINDOWS', 'snapshot verdict')
    report = (evidence / 'REPORT.txt').read_text(encoding='utf-8')
    require(all((v in report for v in computed['verdicts'].values())), 'report verdicts')
    required = {'time_utc', 'folders_json_sha256', 'installed_exe_sha256', 'repo_head', 'repo_status', 'pids'}
    (before, after) = (preserve['before'], preserve['after'])
    require(set(before) == required and set(after) == required | {'fixtures_sha256'}, 'preservation schema')
    for point in [before, after]:
        require(isinstance(point['folders_json_sha256'], str) and re.fullmatch('[0-9a-f]{64}', point['folders_json_sha256']), 'folders hash')
        require(point['installed_exe_sha256'] is None or re.fullmatch('[0-9a-f]{64}', point['installed_exe_sha256']), 'installed EXE hash')
        require(re.fullmatch('[0-9a-f]{40}', point['repo_head']) and isinstance(point['repo_status'], str), 'repository preservation')
        require(isinstance(point['pids'], list) and all((set(p) == {'pid', 'image'} and type(p['pid']) is int and (p['pid'] > 0) and (p['image'].lower() == 'local-files.exe') for p in point['pids'])), 'PID schema')
    preserved = all((before[k] == after[k] for k in ['folders_json_sha256', 'installed_exe_sha256', 'repo_head', 'repo_status'])) and all((p in after['pids'] for p in before['pids'])) and (after['fixtures_sha256'] == {n: v['sha256'] for (n, v) in fixtures.items()}) and (time(before['time_utc']) <= time(snap['clone_started_utc'])) and (time(after['time_utc']) >= max(time(results['finished_utc']), time(snap['gates_finished_utc'])))
    require(preserved and preserve['preservation_ok'] is True, 'preservation recomputation')
    require(git('diff', '13dbae0', '--', 'Cargo.toml', 'Cargo.lock') == '', 'AC24 Cargo unchanged')
    require(git('rev-parse', 'origin/main') == '13dbae0fa651aeb8acca53b28efc0f58cd259c63', 'AC24 origin/main')
    dist = subprocess.check_output(['find', 'dist'], cwd=repo_root, text=True).splitlines()
    require(sorted(dist) == (repo_root / 'artifacts/cp1251-fix/guards/dist-before.txt').read_text().splitlines(), 'AC24 dist listing')
    for (name, digest) in {'REPORT.txt': 'd55ff262a21881cab6923d665753769fa5068e382b568a2c3807da8f00bcdab1', 'results.json': '97e29980b3a10e9113d67fa10729b766a73ee636cca153ad2fd61be381377d67'}.items():
        require(sha256_file(repo_root / 'artifacts/cp1251-hpc-20261030' / name) == digest, 'AC24 immutable RED: ' + name)
    p = subprocess.run(['pgrep', '-fl', 'local-files'], capture_output=True, text=True)
    require(p.returncode in [0, 1], 'pgrep failed')
    require(set((repo_root / 'artifacts/cp1251-fix/guards/pgrep-before.txt').read_text().splitlines()).issubset(set(p.stdout.splitlines())), 'AC24 original processes')
    print('hpc verification passed: ' + json.dumps(computed['verdicts']) + '; ' + str(len(computed['checks'])) + ' raw-response checks')


def main():
    if len(sys.argv) < 2:
        print(
            "Usage: python3 scripts/verify-cp1251.py <record-red|red|record-local|local|hpc>"
        )
        sys.exit(1)

    mode = sys.argv[1]
    repo_root = get_repo_root()

    if mode == "record-red":
        record_red(repo_root)
    elif mode == "red":
        verify_red(repo_root)
    elif mode == "record-local":
        record_local(repo_root)
    elif mode == "local":
        verify_local(repo_root)
    elif mode == "hpc":
        verify_hpc(repo_root)
    else:
        print(f"Unknown or unimplemented mode: {mode}")
        sys.exit(1)


if __name__ == "__main__":
    main()
