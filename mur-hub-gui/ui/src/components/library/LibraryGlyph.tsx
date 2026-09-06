import type { ReactNode } from "react";
import type { LibraryKind } from "../detail/library/libraryModel";
import { Ico } from "../agents/GridCard";

// Copied from Sidebar.tsx GLYPHS (skills / workflows / mcp / plugins) so the
// list and the nav draw the same icon; if the sidebar's icons change, change these.
const GLYPH: Record<LibraryKind, ReactNode> = {
  skill: <path d="M12 2 2 7l10 5 10-5Zm0 15L2 12v5l10 5 10-5v-5Z" />,
  workflow: (
    <>
      <rect x="3" y="3" width="6" height="6" rx="1" />
      <rect x="15" y="15" width="6" height="6" rx="1" />
      <path d="M9 6h6a3 3 0 0 1 3 3v6" />
    </>
  ),
  mcp: (
    <>
      <rect x="4" y="4" width="16" height="16" rx="2" />
      <path d="M9 9h6v6H9z" />
    </>
  ),
  plugin: (
    <path d="M12.2 2h-.4a2 2 0 0 0-2 2v.2a2 2 0 0 1-1 1.7l-.4.3a2 2 0 0 1-2 0l-.2-.1a2 2 0 0 0-2.7.7l-.2.4a2 2 0 0 0 .7 2.7l.2.1a2 2 0 0 1 1 1.7v.5a2 2 0 0 1-1 1.7l-.2.1a2 2 0 0 0-.7 2.7l.2.4a2 2 0 0 0 2.7.7l.2-.1a2 2 0 0 1 2 0l.4.3a2 2 0 0 1 1 1.7V20a2 2 0 0 0 2 2h.4a2 2 0 0 0 2-2v-.2a2 2 0 0 1 1-1.7l.4-.3a2 2 0 0 1 2 0l.2.1a2 2 0 0 0 2.7-.7l.2-.4a2 2 0 0 0-.7-2.7l-.2-.1a2 2 0 0 1-1-1.7v-.5a2 2 0 0 1 1-1.7l.2-.1a2 2 0 0 0 .7-2.7l-.2-.4a2 2 0 0 0-2.7-.7l-.2.1a2 2 0 0 1-2 0l-.4-.3a2 2 0 0 1-1-1.7V4a2 2 0 0 0-2-2Z" />
  ),
};

/** The neutral kind tile used for Library rows (28 px) and detail headers (48 px). */
export function LibraryGlyph({ kind, large }: { kind: LibraryKind; large?: boolean }) {
  return (
    <span className={`library-glyph${large ? " library-glyph--lg" : ""}`} aria-hidden="true">
      <Ico>{GLYPH[kind]}</Ico>
    </span>
  );
}
