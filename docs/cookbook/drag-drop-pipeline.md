# Drag-Drop Pipeline (D3)

Every dropped or pasted image / PDF passes through a 9-step sandboxed pipeline before its text reaches the LLM. The pipeline is on by default — there is no opt-out.

## Pipeline

1. **Dedupe** — Tauri issue #14134 fires duplicate `on_drag_drop_event`s on macOS; `DropDeduper` filters by sorted-paths within a 120 ms window.
2. **iCloud lazy-load fallback** — Apple Photos delivers iCloud images via the clipboard buffer with empty `paths`. `ClipboardSource` trait abstracts the read.
3. **HEIC normalization** — `sips` on macOS converts HEIC → PNG; the PNG goes through an `image-rs` re-encode pass to actually strip EXIF/XMP/iCCP/thumbnails (sips alone preserves those chunks). Linux/Windows stub returns a clear error directing the user to `libheif-dev`.
4. **Sandboxed decode** — `mur-agent-decoder` subprocess (process-isolated; full macOS SBPL + Landlock sandbox lands in B1). Image-rs decode + PNG sRGB re-encode strips container metadata. PDFium decode runs with JS disabled.
5. **Local OCR** — `OcrEngine` trait. `NoopOcr` is the universal fallback today (returns empty text). macOS Vision.framework + tesseract backends are deferred follow-ups (Tasks #56, #57).
6. **Unicode tag-character scrubber** — strips U+E0000-U+E007F (full tag block), ZWJ U+200D, bidi overrides U+202A-U+202E, bidi isolates U+2066-U+2069. Returns `(scrubbed, dropped_count)` for telemetry.
7. **Wrap text** — runtime `B0SafetyHook` injects `<untrusted_image_text source="user_drop">` (or `<untrusted_pdf_text>`) wrappers into the prompt on `on_prompt_submit`.
8. **Provenance entry** — appended to `<agent_dir>/telemetry/inputs.jsonl` (atomic via flock). Sibling `inputs/{sha256}.txt` carries the full extracted text so the runtime can reconstruct it.
9. **Set turn flag** — `B0SafetyHook` raises `after_untrusted_input` turn-flag. On the same turn, `pre_tool_use` denies side-effect tools (delete / spawn / send / egress / network / .write / .publish) via `Decision::AskUser` unless the user confirms.

## What's stripped

- All EXIF / XMP / iCCP / thumbnails on images (we re-encode from the raw RGBA buffer, then re-encode again via image-rs to strip what sips passes through).
- All PDF JavaScript (PDFium binding doesn't auto-execute `/JS`).
- Any text rendered at < 1 pt in PDFs is flagged "quarantined" so the model knows it's likely an injection.
- Unicode tag-character smuggling (Riley Goodside's invisible-letter trick).
- Bidi-override smuggling (RTL/LTR swap attacks).

## Acceptance gates

```bash
scripts/e2e/v1-d3-dragdrop.sh
```

The script enforces:

- A PDF with invisible "ignore previous instructions" text yields `<untrusted_pdf_text>` content with the page-N + quarantined marker — and `B0SafetyHook` denies side-effect tools on the same turn.
- HEIC with EXIF GPS strips all metadata after re-encode (no `eXIf` chunk in output PNG).
- Unicode tag-char smuggling string is scrubbed before reaching the LLM.

## Limits

- **Max 10 files per drop, 30 MB total** — enforced in the Tauri command BEFORE pipeline invocation.
- **Decoder timeout: 10 s per file** — `tokio::time::timeout` around the subprocess read; child is killed on timeout via `start_kill()` + `kill_on_drop(true)`.
- **HEIC**: macOS-only today (sips fallback). Linux/Windows requires `libheif-dev` + re-enabling the `libheif-rs` crate dep.
- **OCR**: NoopOcr today. Vision.framework + tesseract are tracked as follow-up tasks.
- **PDFium runtime**: requires `libpdfium` discoverable. Either drop a prebuilt binary in `.pdfium-bin/lib/` (gitignored), set `PDFIUM_DYNAMIC_LIB_PATH`, or have the lib on the system loader path. CI runners must `apt install libpdfium` (Linux) or `brew install pdfium` (community formula on macOS).

## What's NOT yet hooked

- The supervisor doesn't yet round-trip `PromptPatch.turn_flags` from `on_prompt_submit` into the next-tick `HookCtx`. The M3.8.2 deny-gate is dormant in production until that wiring lands; the unit/integration tests demonstrate the deny logic is correct.
- Production Tauri-clipboard wiring for `multimodal_paste` (`read_clipboard_image_via_plugin()` returns `None` today; the test bypass via `MUR_TEST_CLIPBOARD_IMAGE` is in place).
- PDF catalog hardening (drop `/JS`, `/EmbeddedFile`, `/Launch`, `/RichMedia`, `/SubmitForm` dictionary entries) — pdfium-render 0.8 doesn't expose direct catalog dict access; default-no-JS posture + `<1pt` quarantine is the v1 mitigation.

These gaps are explicitly documented in roadmap §4.3 + §6.1 and are not blockers for D3's v1 ship.

## Files of interest

- `mur-agent-gui/src-tauri/src/multimodal/` — pipeline, decoder client, dedupe, scrubber, HEIC, OCR
- `mur-agent-gui/src-tauri/src/bin/mur-agent-decoder.rs` — sandboxed subprocess
- `mur-agent-runtime/src/hooks/b0.rs` — wrapping + side-effect deny
- `mur-common/src/multimodal/` — shared types + `ProvenanceLedger`
