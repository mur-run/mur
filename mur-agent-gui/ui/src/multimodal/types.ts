export type ArtifactKind = "image" | "pdf" | "text";

export interface MultimodalArtifact {
  sha256: string;
  kind: ArtifactKind;
  mime: string;
  size_bytes: number;
  ocr_text: string | null;
  page_count: number | null;
  created_at: string;
  decoder_version: string;
  ocr_engine_version: string | null;
}
