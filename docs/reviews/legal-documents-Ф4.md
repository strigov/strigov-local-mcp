# Review: legal-documents — Ф4

- **Date**: 2026-10-19
- **Phase scope**: DOCX (tables, comments, tracked changes), XLSX/XLS, EML and MSG readers with provenance
- **Base commit**: bb891e9
- **Rounds**: 5 (Opus 4.1 + Codex Astra 4.2 control): 3 Opus + 2 Codex control
- **Outcome**: CLEAN

## Findings history

Source: `docs/plans/legal-documents.ledger.json`, entries with ids starting `Ф4-`. Ф4-PRE-* are plan-stage corrections recorded after the implementer's BLOCKED report. Plan-only commits during the phase: 11c1df0, bb45df4 (measured sheet budget <= 512 MiB per call, DOCX revision-mark rules, Risks R19/R20), c2bf805 (EML: read every text body part).

| id | reviewer | finding (one line) | decision | final status |
|----|----------|--------------------|----------|--------------|
| Ф4-PRE-1 (plan-stage) | orchestrator | mail-parser 0.11.9 decodes IBM866 only with feature `full_encoding`, otherwise Cyrillic is garbled; plan said no feature needed; feature enabled (adds only encoding_rs, already in the tree) | ACCEPTED | fixed |
| Ф4-PRE-2 (plan-stage) | orchestrator | `cfb::CompoundFile::create` writes a v4 file that calamine 0.36.1 rejects (EmptyRootDir) for Workbook streams >= 4096 bytes; XLS copy must be CFB v3; CONTINUE/rgce sentence corrected | ACCEPTED | fixed |
| Ф4-F1-B1 | opus-review | XLS BIFF-version check (`version < 0x0600`) differs from calamine's parse_bof, so BOF 0x0000/0x0001 bypasses the mandated formula bound (743 MiB peak probe) | ACCEPTED | fixed |
| Ф4-F1-B2 | opus-review | Any XLSX with a chart sheet failed as DOCUMENT_INVALID (NotAWorksheet); calamine treats it as an empty range | ACCEPTED | fixed |
| Ф4-F1-B3 | opus-review | DOCX body-level nested w:ins/w:del make the revision stack prefix every later paragraph (997 B DOCX -> 413.5 MB peak); stack depth capped, document still read | ACCEPTED | fixed |
| Ф4-F1-B4 | orchestrator | XLS DIMENSIONS with rl == rf (empty sheet) was rejected, failing ordinary workbooks with an empty sheet (promoted from NIT) | ACCEPTED | fixed |
| Ф4-F1-B5 | orchestrator | Paragraph-mark revisions rendered as {++++}/{----} and doubled insertion/deletion counts (promoted from DEFER) | ACCEPTED | fixed |
| Ф4-F1-B6 | orchestrator | XLSX extent counted empty styled cells, inflating printed rows/columns and risking FILE_TOO_LARGE (promoted from DEFER) | ACCEPTED | fixed |
| Ф4-F1-B7 | orchestrator | ExactBuffer reserved exactly on every ~8 KiB write; possible multi-second stall on Windows for large XLSX (promoted from CANNOT_VERIFY) | ACCEPTED | fixed (geometric growth) |
| Ф4-F1-P1 | orchestrator | Plan C0 sheet memory statement wrong: measured about 475 MiB at the 4M-cell cap vs stated ~128 MiB; plan-only correction to a per-call budget <= 512 MiB | ACCEPTED | fixed (plan, bb45df4) |
| Ф4-F1-D1 | opus-review | DOCX per-paragraph overhead ~780 B (JSON locator); 9.5 KB DOCX with 1M empty paragraphs -> 789 MB | DEFER | deferred (plan R19) |
| Ф4-F1-D2 | opus-review | zip builds central-directory metadata before the ZIP_ENTRY_LIMIT check (plan's chosen order) | DEFER | deferred (plan R20) |
| Ф4-F1-D3 | opus-review | CANNOT_VERIFY: MSRV 1.92 build and Windows x64 MSVC build not checkable on this host | DEFER | deferred |
| Ф4-F1-N1 | opus-review | Encrypted XLS (FILEPASS) gives FILE_TOO_LARGE/DOCUMENT_INVALID instead of an encrypted message | NIT | deferred |
| Ф4-F1-N2 | opus-review | MSG fixed() reads PtypInteger32 as u64; non-zero padding gives silent cp1251 fallback | NIT | deferred |
| Ф4-F1-N3 | opus-review | Body-level commentRangeStart outside a paragraph dropped while commentRangeEnd emits a marker | NIT | deferred |
| Ф4-F1-N4 | opus-review | shared_strings re-emits sst attributes in double quotes from raw values (theoretical break) | NIT | deferred |
| Ф4-F1-N5 | opus-review | Test gap: CONTINUE-split padded FORMULA not asserted to be charged its concatenated length | NIT | deferred |
| Ф4-F1-N6 | opus-review | README: 'shared strings до 64 МиБ / 48 записей' unclear | NIT | deferred |
| Ф4-F1-N7 | opus-review | MailDocument::unit returns INVALID_CURSOR vs DOCUMENT_INVALID elsewhere | NIT | deferred |
| Ф4-F2-N1 | opus-review | B4 stricter than calamine: rejects DIMENSIONS (1,0,0,0), (0,5,5,2), (0,0,3,0); no known writer emits them | NIT | deferred |
| Ф4-F2-N2 | opus-review | B2 half-covered: a WorkSheet-typed part without sheetData gives DOCUMENT_INVALID; calamine returns empty | NIT | deferred |
| Ф4-F2-N3 | opus-review (promoted by orchestrator) | Tracked inserted table row (w:trPr/w:ins) counted twice in insertions | ACCEPTED | fixed |
| Ф4-F2-N4 | opus-review | Stale comment '448 MiB' in xls_formula_memory_budget | NIT | deferred |
| Ф4-F2-N5 | opus-review | ExactBuffer name misleading after geometric growth | NIT | deferred |
| Ф4-C1-B1 | codex-control | Orphan BIFF STRING records bypass the 4M-cell range bound (calamine allocates 16,777,216 cells); fixed by rejection | ACCEPTED | fixed |
| Ф4-C1-B2 | codex-control | Plan-mandated body_text(0) read only the first text body part of an EML; user decided 'read all parts', plan revised in c2bf805 | ACCEPTED | fixed |
| Ф4-F3-D1 | opus-review | EML/MSG analog of R19: each body paragraph is a Unit with a JSON locator (~800 B); 5 MB EML with 1M empty paragraphs -> 814 MiB peak | DEFER | deferred (extends R19) |
| Ф4-F3-N1 | opus-review | bodySource 'none' when the first text_body entry is an inline image | NIT | deferred |
| Ф4-F3-N2 | opus-review | Stray blank line where body parts join when a part ends in CRLF | NIT | deferred |

## Deferred

1. **Ф4-F3-D1** (extends R19): EML/MSG body paragraphs each become a Unit with a serde_json locator (~800 B; native/email.rs:131-140). Probe: synthetic 5 MB EML, 1M empty paragraphs, 814 MiB peak; extrapolates to about 10 GB at the 64 MiB EML_LIMIT; MSG takes the same path. Ordinary mail has at most thousands of paragraphs. Record in Risks at the next plan revision.
2. **Ф4-F1-D1 / R19** (plan Risks R19): DOCX per-paragraph unit ~780 B; 9.5 KB DOCX with 1M empty paragraphs reached 789 MB. Crafted-file residual.
3. **Ф4-F1-D2 / R20** (plan Risks R20): zip builds central-directory metadata before the ZIP_ENTRY_LIMIT check; zip 8.6.0 caps the reservation at the directory offset, so memory scales with file size, not the declared count.
4. **R7 bounds** (plan Risks R7): calamine cannot be capped from outside, so inputs are bounded before it runs (our own containers, pre-scanned extents, text and formula charges). Residual: the shared-string table does not scale with input bytes (empty `<si/>` is 5 bytes of XML, 24-48 bytes of table), bounded by the `SHEET_TEXT_LIMIT / 48` entry cap.
5. **Threat model** (crafted-input DoS is residual unless the fix is cheap; accidental or corrupt files must be bounded): R19, R20, Ф4-F3-D1 and the R7 residual belong to this accepted crafted-file DoS class. Cheap fixes were applied (Ф4-F1-B3 revision-stack cap, Ф4-C1-B1 orphan STRING).
6. **Ф4-F1-D3**: MSRV 1.92 build (mail-parser/hashify edition 2024) and Windows x64 MSVC build are unverified (no rustup/target on the review host, Homebrew rustc 1.99). Verify at the Ф5 release / `docs/windows-validation.md`.
7. **XLS memory measurement** (plan Ф4 step 8, mandatory): `/usr/bin/time -l` on the release test binary running `xls_formula_memory_budget` on the final tree, including BOF version 0 and 0x0001 cases: max reader 335.2 MiB, whole process 438.7 MiB, against the C0 sheet budget of 512 MiB. Within budget.

## Nits (informational)

- Ф4-F3-N1: bodySource reads "Mail body · none" when the first text_body entry is an image (native/email.rs:50); later entries still supply the text, none is lost.
- Ф4-F3-N2: stray blank line where body parts join when a part ends in CRLF (native/email.rs:56-63, :131); text and cursor chain are complete.
- Ф4-C1-B1: the fix took the rejection option rather than counting (0,0) in the extent; the ledger allowed both; no ordinary-file impact.
- Ф4-F1-N1 to N7, Ф4-F2-N1, N2, N4, N5: see the findings table (all deferred NITs).
