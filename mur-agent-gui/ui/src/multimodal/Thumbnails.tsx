import type { MultimodalArtifact } from "./types";
import { ThumbnailItem } from "./ThumbnailItem";

interface Props {
  artifacts: MultimodalArtifact[];
  onRemove: (sha256: string) => void;
}

export function Thumbnails({ artifacts, onRemove }: Props) {
  if (artifacts.length === 0) return null;
  return (
    <div className="flex flex-wrap gap-1 p-2">
      {artifacts.map((a) => (
        <ThumbnailItem key={a.sha256} artifact={a} onRemove={onRemove} />
      ))}
    </div>
  );
}
