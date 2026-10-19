# Review: legal-documents — Ф2

- **Date**: 2026-10-19
- **Phase scope**: Continuation cursors, deterministic resumable traversal, streaming text, new read_document/inspect_document tools with the PDF text layer (pages, OCR-layer and suspicious-text detection)
- **Base commit**: 9e60ace
- **Rounds**: 7 Opus 4.1 + 2 Codex Astra 4.2 control (rounds 5–7 authorized by the user beyond the cap of 4)
- **Outcome**: CLEAN

## Findings history

Source: `docs/plans/legal-documents.ledger.json`. Ids without a phase prefix (STRUCT-1, STRUCT-1-cal) are included because they close Ф2 findings in `native/pdf.rs`; CAL-1 is a Ф1 calibration record and is left out.

| id | reviewer | finding (one line) | decision | final status |
|----|----------|--------------------|----------|--------------|
| Ф2-F1-PM1 | opus-review | lopdf Content::decode under the 64 MiB per-stream limit amplifies memory ~100x (8 MiB decoded content -> 799 MiB RSS); fixed by user-chosen PDF_CONTENT_LIMIT = 2 MiB cumulative per page with a page-level flag | ACCEPTED | fixed |
| Ф2-F1-B1 | opus-review | One unresolvable font/XObject reference made the whole PDF DOCUMENT_INVALID, contradicting the C7 undecodable rule | ACCEPTED | fixed |
| Ф2-F1-D1 | opus-review | fetch regression: UTF-8 files with unlisted extensions (README, .tex, .srt) now return UNSUPPORTED_FORMAT | DEFER | deferred |
| Ф2-F1-N1 | opus-review | debug_assert! used to touch unused contract fields; page_images inside it repeats extraction | NIT | deferred |
| Ф2-F1-N2 | opus-review | Fallback unsupported hint "save as DOCX or PDF" is misleading for .docx/.eml/images/.zip pending later phases | NIT | deferred |
| Ф2-F1-N3 | opus-review | Node test comment claims a forged path is never opened, but the cursor fails on root-id mismatch | NIT | deferred |
| Ф2-F1-N4 | opus-review | get_file_info description lacks the new format field; directories get a format value | NIT | deferred |
| Ф2-F1-N5 | opus-review | read_file on a UTF-8 file with a non-text extension reads the file twice | NIT | deferred |
| Ф2-F1-N6 | opus-review | show() counts C1 controls as undecodable; C7 says C0 only | NIT | deferred |
| Ф2-F2-PM1 | opus-review | lopdf expands every ToUnicode bfrange code (~1 KB PDF -> 276–534 MiB per font); fixed by user-chosen TOUNICODE_CODE_LIMIT = 65536 pre-scan | ACCEPTED | fixed |
| Ф2-F2-B1 | opus-review | Font encodings re-resolved on every Do (4000 Do -> 17.6 s, deadline overrun); fixed by a per-page font cache keyed by ObjectId | ACCEPTED | fixed |
| Ф2-F2-N1 | opus-review | Form XObject decompression failure other than the size limit still fails the whole document | NIT | deferred |
| Ф2-F2-N2 | opus-review | Malformed page content reported as plain empty/no_text without a flag | NIT | deferred |
| Ф2-F2-N3 | opus-review | No synthetic test combining repeated Do with a non-trivial ToUnicode map | NIT | deferred |
| Ф2-F3-B1 | opus-review | /Encoding reference chain of 129 hops bypasses the ToUnicode pre-scan (12.7 KB PDF -> 276.9 MiB) | ACCEPTED | fixed (closed by STRUCT-1) |
| Ф2-F3-B2 | opus-review | Per-Do iteration over every /Font entry (1000 entries x 340k Do -> 35.9 s) | ACCEPTED | fixed (closed by STRUCT-1) |
| Ф2-F3-B3 | opus-review | ToUnicode byte budget not charged on non-limit decode errors (300 fonts -> 17.6 GiB inflated) | ACCEPTED | fixed (closed by STRUCT-1) |
| Ф2-F3-B4 | opus-review | Unbounded q stack clones TextState with a Vec<u8> font name (834-byte file -> 645 MiB) | ACCEPTED | fixed (closed by STRUCT-1) |
| Ф2-F3-D1 | opus-review | C0 per-component memory figures understated; macOS malloc cache footprint caveat | ACCEPTED | fixed (folded into STRUCT-1 plan amendment) |
| Ф2-F3-N1 | opus-review | FontKey::Direct uses a raw pointer address as scope id | NIT | deferred |
| Ф2-F3-N2 | opus-review | Implementer evidence used RUST_TEST_THREADS=1 | NIT | deferred |
| Ф2-F3-N3 | opus-review | font_map_guard test lacks /Encoding chain coverage | NIT | deferred |
| STRUCT-1 | user | Structural guard replacing per-finding fixes: one per-page work budget PDF_PAGE_WORK_LIMIT covering Ф2-F3-B1..B4 as classes | ACCEPTED | fixed |
| STRUCT-1-cal | orchestrator | Step 11 calibration halved PDF_PAGE_WORK_LIMIT to 2^26 (3.25 s at 2^27 -> 1.63 s); tests adapted | ACCEPTED | fixed |
| Ф2-F4-B1 | opus-review | Glyph-decode output not charged to the page work budget (8.9 KB PDF -> 3989 MiB, 8.25 s/page unflagged); fixed by charging output before lopdf allocates | ACCEPTED | fixed |
| Ф2-F4-N1 | opus-review | r3_errdecode.py probe never selects its fonts, no longer exercises failed-decode charging | NIT | deferred |
| Ф2-F5-N1 | opus-review | Glyph charge paths without unit tests (Differences-wrapped Identity-H, inherited font in Form XObject, form-owned font) | NIT | deferred |
| Ф2-F5-N2 | opus-review | scan_cmap accepts destinations >256 UTF-16 units and hex strings with comments that lopdf rejects (safe-side overcharge) | NIT | deferred |
| Ф2-F5-N3 | opus-review | show() looks up page_state.fonts.get(handle) twice (cosmetic) | NIT | deferred |
| Ф2-F5-B1 | codex-control | Unbounded lopdf load_metadata_mem preflight decompresses XRef/object streams beyond 64 MiB; removed, encryption derived from the bounded loader | ACCEPTED | fixed |
| Ф2-F5-B2 | codex-control | Bounded full loader permits object-stream parse amplification (4096 object ids over one 1 MiB string, ~60x Object amplification); user deferred, recorded as plan risk R17 | DEFER | deferred |
| Ф2-F5-B3 | codex-control | Cursor encode had no length limit while decode rejects >2048 bytes (deep quote-named dirs break the AC6 chain); fixed by compact binary cursor | ACCEPTED | fixed |
| Ф2-F5-B2-doc | codex-control | Documentation half of Ф2-F5-B2: record R17 and correct the C0 budget statements | ACCEPTED | fixed |
| Ф2-F5-D1 | codex-control | Connector `search` walks without a depth limit through the same walker and would omit not-descended directories without a skippedTooDeep counter | DEFER | deferred |
| Ф2-F5-D2 | codex-control | Ф5 container-listing cursor puts ZIP member components (names up to 65,535 bytes) into p and could exceed CURSOR_MAX_LEN | DEFER | deferred |
| Ф2-F6-B1 | opus-review | C4 MAX_NAME_BYTES = 255 is false on APFS (UTF-8 names up to 765 bytes); replaced by an exact per-child cursor-length check | ACCEPTED | fixed |
| Ф2-F6-N1 | opus-review | Padded compressed-XRef test passed with or without the old preflight; test now fails if an unbounded decode returns | ACCEPTED | fixed |
| Ф2-F6-N2 | opus-review | C0 PDF_ENCRYPTED_LIMIT wording: a #xx-escaped /Encrypt key bypasses the raw pre-check and is bounded by PDF_LIMIT (512 MiB) | ACCEPTED | fixed |
| Ф2-F6-D1 | opus-review | search WalkEvent::Inaccessible carries no position, so resuming from an earlier cursor could double-count the same failure (pre-existing) | DEFER | deferred |
| Ф2-F7-N1 | opus-review | Source guard in production_document_loads_use_only_the_bounded_helper misses IncrementalDocument::load_mem and aliased imports (test-only) | NIT | deferred |
| Ф2-F7-N2 | opus-review | walk_child_fit_switches_at_the_exact_raw_cursor_limit prints expected debug_assert! panic noise under --nocapture | NIT | deferred |
| Ф2-F7-N3 | opus-review | Node Cyrillic/quote-named/sorted-cursor tests compare against UTF-16 sorted order, matching walker pre-order only for these fixtures | NIT | deferred |

## Deferred

1. **Ф2-F5-B2 / plan risk R17** (codex-control, SECURITY): object-stream / parse amplification in the bounded PDF loader. Deliberately deferred by the user on 2026-10-19 and recorded in the plan as residual risk R17 together with the corrected C0 budget statements (Ф2-F5-B2-doc). **Must be revisited before Ф5.** Also accepted as residual: the encrypted-PDF filter bypass (a #xx-escaped /Encrypt key is bounded by PDF_LIMIT, 512 MiB).
2. **Ф2-F5-D1**: connector `search` walks with no depth limit and would omit not-descended directories without a skippedTooDeep counter.
3. **Ф2-F5-D2**: Ф5 container-listing cursors with long ZIP member names could exceed CURSOR_MAX_LEN; address when Ф5 starts.
4. **Ф2-F6-D1**: search WalkEvent::Inaccessible has no position, so resuming from an earlier cursor could double-count the same failure (pre-existing behavior).
5. **Ф2-F1-D1**: fetch regression — UTF-8 files with unlisted extensions (README, .tex, .srt) return UNSUPPORTED_FORMAT instead of text.

## Nits (informational)

Final-round (Opus round 7, verdict REVIEW_OK) nits; all are test-only:

- The source guard in `production_document_loads_use_only_the_bounded_helper` matches only `\bDocument::load*`, so it would miss `IncrementalDocument::load_mem(..)` and calls through an aliased import (`use lopdf::Document as D`). Neither test would catch such a preflight inside `PdfDocument::load`. (`native/pdf.rs` tests; Ф2-F7-N1)
- `walk_child_fit_switches_at_the_exact_raw_cursor_limit` checks the over-limit case in debug builds by catching `encode`'s `debug_assert!` panic, so `--nocapture` runs print expected panic messages. A raw-length helper shared by `encode` and the test would avoid this. (`native/cursor.rs`; Ф2-F7-N2)
- The Node Cyrillic, quote-named and sorted-cursor tests compare the chain's union with `[...expected].sort()` (UTF-16 order on full relative paths), which matches the walker's pre-order only for these fixtures; an explicit pre-order expected list would hold for any tree. (`test/native.test.mjs`; Ф2-F7-N3)

Earlier-round nits are listed in the findings history (ids ending in `-N<n>`, status deferred).

Final verdicts: Opus round 7 — REVIEW_OK (SPEC: OK, QUALITY: OK); Codex Astra control round 2 — REVIEW_OK.

## Open at accept

None — outcome is clean.
