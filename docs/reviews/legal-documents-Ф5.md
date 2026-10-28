# Implementation report: legal-documents — Ф5

- **Date**: 2026-10-28
- **Phase scope**: ZIP and mail attachments through `::` paths, bounded document search, release 2.2.0
- **Base commit**: `3938ca741ed356e7b319fb511f5fa3cd21867213` (shared tree remains uncommitted)
- **Windows validation snapshot**: `c6f5ab3d2522c5520bcb6fca935e6b3d175eed83`, clean isolated checkout
- **Outcome**: IMPLEMENTED; independent phase review pending

## Findings history

Source: existing `docs/plans/legal-documents.ledger.json` entries with ids starting `Ф5-`. The ledger was present in the dirty tree; this dispatch did not add or edit ledger entries. The three accepted implementation findings were verified against code, documentation and tests and already had `fixed` statuses.

| id | reviewer | finding (one line) | decision | final status |
|----|----------|--------------------|----------|--------------|
| Ф5-PRE-1 | orchestrator | Revisit R17 before folder-wide PDF search; keep the accepted crafted-file DoS residual under the approved threat model | ACCEPTED | fixed (plan) |
| Ф5-PRE-2 | orchestrator | Container listing must check exact cursor fit before yielding a member and count omitted members once in `skippedTooDeep` | ACCEPTED | closed |
| Ф5-F1-B1 | opus-review | Depth 3 listing and inspection could expose depth-4 child paths | ACCEPTED | fixed |
| Ф5-F1-B2 | opus-review | README needed to state that making scan text copies is the user's choice and originals remain unchanged | ACCEPTED | fixed |
| Ф5-F1-B3 | orchestrator | Windows validation instructions named the 2.1.2 ZIP | ACCEPTED | fixed |
| Ф5-F1-D1 | opus-review | Crafted long ZIP names can amplify listing memory before cursor-fit filtering | DEFER | deferred |
| Ф5-F1-D2 | opus-review | One corrupt ZIP local header can fail the full listing with `ZIP_INVALID` | DEFER | deferred |
| Ф5-F1-D3 | opus-review | Existing raw ZIP `read_file` may return up to 15 MiB, including bytes of blocked members | DEFER | deferred |
| Ф5-F1-D4 | opus-review | ZIP-style name validation makes some colon or legacy path attachment names unreadable | DEFER | deferred |
| Ф5-F1-N1–N6 | opus-review | Attachment index ordering, unnamed attachment format, transient in-memory copy, macOS readme version bump, missing-member error wording, and MSG OLE attachment classification | NIT | deferred |

## Implementation and tests

`native/archive.rs` adds bounded ZIP listing and extraction with CP866 name decoding, path normalization, encrypted-member detection and size, ratio and entry-count guards. `native/doc_search.rs` searches document units with locators, bounded snippets, byte and deadline budgets, and resumable cursors. The document/read/view dispatch now resolves nested ZIP members and indexed EML/MSG attachments; folder `search_text` searches supported document formats, while `search_files` does not descend into archives. MCP descriptions and server instructions document `::` syntax and cursor continuation.

New Rust coverage includes archive size/count and path-confinement checks; document search continuation, stale-file handling, locators and budgets; EML attachment extraction; and bounded MSG attachment extraction with explicit rejection of embedded Outlook messages. `test/native.test.mjs` adds MCP cases for safe/nested ZIP paths and cursor fit, CP866 and corrupt-member bounds, attachments across readers/views/search, document-search continuation and locators, PDF page numbers and connector search, and the B1 depth cap.

All required local commands were run on the current shared tree after the Windows lint and CRLF test fixes; each exited successfully:

| Command | Result |
|---|---|
| `cargo fmt --check` | PASS |
| `cargo clippy --locked --all-targets -- -D warnings` | PASS |
| `cargo test --locked` | PASS — 143 passed, 0 failed, 3 ignored |
| `cargo build --release --locked` | PASS |
| `npm ci --ignore-scripts` | PASS — 93 packages installed; npm reported 8 audit advisories (3 moderate, 4 high, 1 critical); no Node dependencies were upgraded |
| `node --test test/native.test.mjs` | PASS — 34 passed, 0 failed |
| `NATIVE_WORKER=1 node --test test/native.test.mjs` | PASS — 34 passed, 0 failed |
| `python3 -B -m unittest test/test_mcp_experiment.py test/test_windows_packaging.py` | PASS — 9 tests |
| `git diff --check` | PASS |

Additional release-only budget evidence from `/tmp/local-files-readiness-budgets.log`: `large_pdf_budget` passed on a 321,620,336-byte synthetic PDF; inspection took 63 ms, page 900 read took 43 ms, and maximum RSS was 342,409,216 bytes (20,788,880 bytes above file size, below the allowed 128 MiB overhead). `xls_formula_memory_budget` passed; whole-process maximum RSS was 460,144,640 bytes, below 512 MiB.

## Release evidence

### macOS arm64

The previous implementer executed the isolated release build and verification in `/tmp/local-files-f5-release.6BKWB2/project`; this dispatch inspected the packaging and verification scripts and record, then rechecked the DMG SHA in both the isolated workspace and shared `dist`. The package script replaces same-named app bundles under its `dist`, so it ran against the isolated project and only the new 2.2.0 DMG and sidecars were copied into the shared `dist`. The 2.1.0, 2.1.1 and 2.1.2 distributions remain present.

- Artifact: `dist/Strigov-Local-MCP-2.2.0-macos-arm64.dmg`
- Size: 13,009,601 bytes
- SHA-256: `babba456452763610e6f29057ef79ea9b00d365d2f35005bbc8309df31711ffb` (matches the isolated build and repository copy)
- Verification: `dist/Strigov-Local-MCP-2.2.0-macos-arm64.verification.json` reports clean isolated install, empty initial roots, settings and connection preservation on update, backup creation, codesign verification, packaged worker MCP tests and the generated-profile runtime probe; `live_user_settings_changed` is `false`.
- Limits: GUI rename interaction was `NOT_RUN_FOR_RENAME`; notarization is false and other-account authentication was not tested.

### Windows x64 MSVC on HPC

The preserved HPC checkout was `D:\Projects\GPT-MCP-filesystem` in workspace `d8a47734-0007-40b4-9893-3bdfab5441fb`; it remained clean at `af901a2129faf12360e7b51e667de3f16c8505b8`. Validation ran in a separate isolated staging checkout at `C:\Users\strig\AppData\Local\Temp\local-files-f5-2.2.0-20261028-7c14e8\project`. Its `validation-results.json` records source SHA `c6f5ab3d2522c5520bcb6fca935e6b3d175eed83`, `dirty: false`, Windows 10 build 26200, AMD64, Rust/Cargo 1.92.0, and `rustc -vV` reports host `x86_64-pc-windows-msvc`. The required format, Clippy, Rust tests, release build, npm, Node, worker, Python, installer and distribution-verifier checks all completed successfully in that snapshot.

- Validation summary: `C:\Users\strig\AppData\Local\Temp\local-files-f5-2.2.0-20261028-7c14e8\validation-results.json`; step logs are in the adjacent `logs` directory (`01-source-diff-check.log` through `11-windows-distribution-verifier.log`).

- Rust tests: 145 passed, 0 failed, 3 ignored; installer test binary: 2 passed.
- Native MCP and worker MCP: 34 total each, 29 passed, 0 failed, 5 skipped on Windows.
- Python discovery: 9 passed. The first harness invocation used package-style module names and failed to import; the corrected discovery command passed and is the recorded successful check.
- Windows installer ZIP: `C:\Users\strig\AppData\Local\Temp\local-files-f5-2.2.0-20261028-7c14e8\project\dist\Strigov-Local-MCP-2.2.0-windows-x64.zip`, 12,077,495 bytes, SHA-256 `def8ad4b73760b48483af0fb3717e423379a145f010fbbd173b247db83dd26d0`.
- Verifier report: `C:\Users\strig\AppData\Local\Temp\local-files-f5-2.2.0-20261028-7c14e8\project\artifacts\windows-checks\20261028T121630Z-0e44c6d6\verification.json`; all 19 checks passed, including installed worker MCP and packaged-runtime health. `user_settings_changed` is false and `signed` is false.
- Verifier check IDs, all `PASS`: `sidecar-sha256`, `zip-members-safe`, `zip-layout`, `installer-check`, `install-clean`, `installed-matches-payload`, `app-version`, `app-init-shares-nothing`, `app-check`, `app-status-not-connected`, `reinstall-keeps-settings-and-backup`, `reject-busy-unchanged`, `reject-foreign-unchanged`, `reject-reparse-ancestor`, `reject-corrupt-check`, `reject-corrupt-install`, `reject-extra-file`, `installed-worker-mcp`, `packaged-runtime-local-health`.
- Windows GUI interaction, external-account authentication and system autostart were `NOT_RUN`. No Windows runtime result is inferred beyond the executed installer, worker MCP and local-health checks.

## Deferred residuals and unrun checks

- R17 remains accepted: lopdf has no whole-load memory/time guard for crafted PDFs. Folder search uses the same loader one file at a time under the existing semaphore and deadline; ordinary-file limits and confidentiality/path/read-only guarantees remain required.
- R18 remains accepted: a crafted JBIG2/JPX decode can exceed header-derived memory expectations or run beyond the per-call deadline because codecs lack cumulative allocation limits or cooperative cancellation.
- Ф5-F1-D1–D4 remain deferred as recorded in the findings table. They do not change the accepted confidentiality, path-confinement or read-only requirements.
- Optional manual archive/TIFF/cursor-driven search against the named case-folder corpus was `NOT_RUN`; those real user files were not opened. Synthetic fixtures and MCP tests provide the executed container/search evidence.
- The README line stating that remote CI on 2.1.2 was not run remains unchanged; this report claims no CI result for that line.

## Working-tree notes

The implementation files and README, packaging instructions and verifier were already modified in the shared tree when this dispatch resumed. Additional current-tree changes outside the original Ф5 file list are `docs/distribution-readme.txt` (2.2.0 macOS heading), `docs/windows-validation.md` (B3 path correction), `native/nickname_dialog.rs` (Windows Clippy/MSVC compatibility), and `native/pdf.rs` (CRLF normalization in a source-inspection test). The plan metadata additions (`orca_run` and `implementer`) and the existing untracked ledger were present on entry and left untouched; the plan's phase status remains coordinator-owned. No dependency upgrades or commits were made in the shared working tree by this dispatch. The user-authorized validation branch `validation/legal-documents-f5-20261028` at `https://github.com/strigov/GPT-MCP-filesystem.git` contains the clean snapshot at SHA `c6f5ab3d2522c5520bcb6fca935e6b3d175eed83`.
