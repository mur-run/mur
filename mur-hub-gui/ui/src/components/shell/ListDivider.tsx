import type { ResizableColumn } from "./useResizableColumn";

export function ListDivider({ column, label }: { column: ResizableColumn; label: string }) {
  return (
    <div
      className="list-divider"
      role="separator"
      aria-orientation="vertical"
      aria-label={label}
      title={label}
      onPointerDown={column.onPointerDown}
      onDoubleClick={column.reset}
    />
  );
}
