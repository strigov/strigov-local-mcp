# Review: legal-documents — Ф3

- **Date**: 2026-10-19
- **Phase scope**: Visual access to PDF pages and scans: page view as MCP image content via bounded decoding of DCT/Flate/CCITT/JBIG2/JPX scan images with the hayro codecs, single-page-PDF fallback for composite pages (MSRV raised to 1.92)
- **Base commit**: a1cd399
- **Rounds**: 7 (Opus 4.1 + Codex Astra 4.2 control): 5 Opus + 2 Codex control
- **Outcome**: CLEAN

## Findings history

Source: `docs/plans/legal-documents.ledger.json`, entries with ids starting `Ф3-`. Ф3-PRE-* are plan-stage items recorded before implementation.

| id | reviewer | finding (one line) | decision | final status |
|----|----------|--------------------|----------|--------------|
| Ф3-PRE-1 (plan-stage) | orchestrator | Plan routed every JBIG2/JPX/composite page to the single-page-PDF channel (ChatGPT asks "Allow file materialization?" per embedded PDF); user chose hayro codecs (JBIG2, JPX, CCITT) so these scans arrive as images | ACCEPTED | fixed |
| Ф3-PRE-2 (plan-stage) | orchestrator | hayro codecs declare MSRV 1.92 while the plan fixed rust-version 1.88; user chose to raise to 1.92 (Cargo.toml and docs) | ACCEPTED | fixed |
| Ф3-PRE-D1 (plan-stage) | orchestrator | Duplicate PageView declaration for branch C under P1-B2 may need a cfg note; old plan listed `fax` for branch B too | DEFER | deferred |
| Ф3-F1-B1 | opus-review | JPX header caps bypassed by a JP2 with two jp2c boxes (server reads the first, hayro keeps the last) | ACCEPTED | fixed |
| Ф3-F1-B2 | opus-review | JBIG2 halftone grid (HGW/HGH) not pre-scanned; allocation up to HGW*HGH*4 bytes | ACCEPTED | fixed (superseded by Ф3-F3-S1 allowlist) |
| Ф3-F1-D1 | opus-review | CCITT change lists grow with the data (Columns up to 2^28 -> ~1 GiB); page-view budget understated | DEFER | deferred |
| Ф3-F1-D2 | opus-review | JPEG decode memory beyond max_alloc (progressive coefficient buffers, ~0.8 GiB crafted) | DEFER | deferred |
| Ф3-F1-D3 | opus-review | single_page_pdf strips only some trailer keys; /AA or other keys can keep other pages' objects in the output | DEFER | deferred |
| Ф3-F1-D4 | opus-review | JPX ColorSpace check runs a second Image::new after full decode; should move before decode | DEFER | deferred |
| Ф3-F1-D5 | opus-review | Possible flake in a Ф2 test (cumulative_page_content_budget_...) under parallel threads | DEFER | deferred |
| Ф3-F1-N1 | opus-review | Page-PDF JSON reports pageCount: 1 (emitted payload) instead of the source page count | NIT | deferred |
| Ф3-F1-N2 | opus-review | `region` on a PDF without `page` is silently ignored | NIT | deferred |
| Ф3-F1-N3 | opus-review | not_viewable text omits the page classification the plan asks for (unreachable in the A+pdf build) | NIT | deferred |
| Ф3-F1-N4 | opus-review | Visible glyphs mapped by ToUnicode to whitespace are not counted as visible text (crafted files only) | NIT | deferred |
| Ф3-F2-B1 | opus-review | Regression from the F1-B1 fix: post-Image::new dimension equality check rejected valid JPX (hayro's ilog2 level selection) | ACCEPTED | fixed |
| Ф3-F2-B2 | opus-review | JBIG2 custom Huffman table segments (type 53) uncapped (promoted from reviewer DEFER) | ACCEPTED | fixed (superseded by Ф3-F3-S1 allowlist) |
| Ф3-F2-D1 | opus-review | JPX memory figure understated (5 B vs ~15 B per sample; page view peaks ~PDF + 0.8 GiB) | DEFER | deferred |
| Ф3-F2-D2 | opus-review | lopdf predictor buffers from /DecodeParms /Columns allocated before data is read (pre-existing since Ф2, ~140 MiB) | DEFER | deferred |
| Ф3-F2-N1 | opus-review | tiff_pages reads every IFD before page 1, so one bad later IFD fails page 1 (fails closed) | NIT | deferred |
| Ф3-F3-B1 | opus-review | Type-53 segments capped individually but not in number; hayro retains every parsed table (~40x) | ACCEPTED | fixed (resolved by Ф3-F3-S1) |
| Ф3-F3-D1 | opus-review | JPX with fewer decomposition levels than the view needs falls back though a less-reduced decode fits; real encoders use >= 5 levels | DEFER | deferred |
| Ф3-F3-D2 | opus-review | JBIG2 segment count not capped (~77 B retained per segment) | ACCEPTED | fixed (resolved by Ф3-F3-S1, JBIG2_MAX_SEGMENTS) |
| Ф3-F3-D3 | opus-review | Palette JPX always decodes at full resolution; large palette scans fall back | DEFER | deferred |
| Ф3-F3-N1 | opus-review | `.max(1)` on JPX targets can never take effect after the ilog2 clamp | NIT | deferred |
| Ф3-F3-S1 | orchestrator | Impasse diagnosis: hayro-jbig2 retains state for nearly every segment type with no overall limit; user chose a segment-type allowlist plus whole-call totals in the prescan | ACCEPTED | fixed |
| Ф3-F3-TM | orchestrator | Explicit threat model added to the plan (user 2026-10-19): ordinary/corrupted files must be bounded; crafted-file DoS is an accepted residual unless the fix is cheap and local | ACCEPTED | fixed |
| Ф3-C1-B1 | codex-control | Visible annotation appearances (/Annots with /AP: stamps, signatures) lost in page views; user chose "Показывать" (page with a visible annotation is composite) | ACCEPTED | fixed |
| Ф3-C1-B2 | codex-control | Visual walker ignored clipping (W / W*), returning the whole image where only part is visible; user chose "Умная проверка" (track rectangular clips; non-admissible clip -> composite) | ACCEPTED | fixed |
| Ф3-C1-D1 | orchestrator | Text render modes 4-7 clip from ET on; glyph clips are not counted by page_paint | DEFER | deferred |
| Ф3-C1-D2 | orchestrator | Single-page fallback does not remove page-level /AA and does not handle reference XObjects (/Ref); pre-existing | DEFER | deferred |
| Ф3-F5-N1 | opus-review | Non-integer /F (e.g. 4.0) or indirect /Rect elements make an annotation count as invisible | NIT | deferred |
| Ф3-F5-N2 | opus-review | Test gaps: entry-only 129-hop chain under a direct /Annots; only a positive W* test | NIT | deferred |

## Deferred

1. **Ф3-F1-D1**: CCITT change lists grow with the data (cap Columns, e.g. <= 2^20, or correct the budget).
2. **Ф3-F1-D2**: JPEG progressive coefficient buffers are outside IMAGE_DECODE_ALLOC (~0.8 GiB crafted).
3. **Ф3-F1-D3**: harden single_page_pdf by rebuilding the trailer with only /Root (and /ID).
4. **Ф3-F1-D4**: move the JPX ColorSpace check before the full decode.
5. **Ф3-F1-D5**: possible parallel-thread flake in a Ф2 content-budget test; follow nextCursor to remove the risk.
6. **Ф3-F2-D1**: JPX memory figure understated (~15 B per sample; page view peaks ~PDF + 0.8 GiB); fix the plan/README figure (R18 class).
7. **Ф3-F2-D2**: lopdf predictor buffers allocated from /DecodeParms /Columns before reading data (pre-existing since Ф2).
8. **Ф3-F3-D1**: JPX with few decomposition levels falls back instead of decoding at a smaller reduction.
9. **Ф3-F3-D3**: palette JPX always decodes at full resolution; large palette scans fall back.
10. **Ф3-C1-D1**: text render modes 4-7 glyph clips are not counted by page_paint.
11. **Ф3-C1-D2**: single-page fallback keeps page-level /AA and does not handle /Ref XObjects; revisit in the final review.
12. **Ф3-PRE-D1**: PageView declaration duplicate under P1-B2 and the stale `fax` mention for branch B.

Residual risks recorded in the plan for this phase (`docs/plans/2026-10-17-legal-documents.md`): **R18** (hayro codecs have no allocation limits or cooperative cancellation: CPU for one call after a decode starts, and memory in JBIG2 symbol dictionaries and JPX per-tile bookkeeping not coverable from headers; allocation failure aborts the worker, R6). Under the plan's threat model (Ф3-F3-TM, Risks section "Threat model") the accepted crafted-file DoS class (process abort, memory pressure, one long call) is a residual unless the fix is cheap and local, as with R17 and R18; confidentiality, path confinement and read-only access stay absolute.

## Nits (informational)

Final-round (Opus round 5, verdict REVIEW_OK; Codex control round 2 REVIEW_OK with no findings) nits:

- Non-standard annotation encodings count as invisible (`native/pdf_page.rs:504-506`, `:516-521`): an /F that is present but not an integer (a Real such as 4.0, or an unresolvable reference), or a /Rect with indirect-reference elements, makes the annotation invisible, so such a page could be image-extracted without its stamp. Real producers write direct /F 4 and numbers. Optional fix: treat a non-integer /F as 0 and dereference /Rect elements. (Ф3-F5-N1)
- Two small test gaps: an entry-only 129-hop chain under a direct /Annots is not tested on its own, and the only W* test is positive (a cutting W* clip was verified by probe to fall back). (Ф3-F5-N2)
- Ledger and plan wording differed for Ф3-C1-B2 (rotated clips); the code follows the plan (non-axis-aligned clip -> composite), and the ledger was aligned by the orchestrator.
