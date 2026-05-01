import type { MultimodalArtifact } from "./types";

interface Props {
  artifact: MultimodalArtifact;
  onRemove: (sha256: string) => void;
}

function fmtBytes(n: number): string {
  if (n < 1024) return `${n} B`;
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KB`;
  return `${(n / 1024 / 1024).toFixed(1)} MB`;
}

export function ThumbnailItem({ artifact, onRemove }: Props) {
  return (
    <div
      className="flex items-center gap-2 rounded border px-2 py-1 text-xs"
      style={{
        borderColor: "var(--color-border)",
        background: "var(--color-bg-secondary)",
      }}
    >
      <span aria-hidden="true">
        {artifact.kind === "pdf" ? "PDF" : artifact.kind === "image" ? "IMG" : "TXT"}
      </span>
      <span className="opacity-75">{fmtBytes(artifact.size_bytes)}</span>
      {artifact.page_count != null && (
        <span className="opacity-75">{artifact.page_count} pp</span>
      )}
      <button
        type="button"
        aria-label="Remove attachment"
        onClick={() => onRemove(artifact.sha256)}
        className="ml-1"
        style={{ color: "var(--color-fg)", opacity: 0.5 }}
      >
        ×
      </button>
    </div>
  );
}
