//! Multimodal artifact ingestion (D3, partial).
//!
//! The full multimodal pipeline lives in
//! [`mur_common::multimodal`] (the schema) plus a constellation of
//! D-track milestones that handle decoding (PDF page extraction, EXIF
//! scrubbing, image re-encoding) and OCR. The runtime-side glue —
//! "given some bytes + a mime type + an `agent_home`, write a sidecar
//! and a provenance entry" — was previously open-coded by each caller
//! (see `mur-core/src/cmd/agent_companion/card/accept.rs`).
//!
//! [`pipeline::process_artifact`] is the smallest reusable wrapper that
//! satisfies the M-c2.4 contract: hash the bytes, write a `.txt`
//! sidecar at `<agent_home>/telemetry/inputs/{sha}.txt`, append a
//! `ProvenanceEntry` to `<agent_home>/telemetry/inputs.jsonl`, and
//! return both the sha and the absolute ledger path. Real OCR / PDF
//! text extraction lands in subsequent D-track milestones; today the
//! sidecar carries either the raw text body (for `text/*`), a
//! synthetic `--- page 1 ---` marker prefix (for `application/pdf`,
//! enough for B0's `untrusted_pdf_text` heuristic), or an empty file
//! (for images — the OCR plumbing wires in later).

pub mod pipeline;
