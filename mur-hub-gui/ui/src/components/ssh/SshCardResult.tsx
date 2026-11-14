//! The SSH card's result pane, after Approve. Split out of `SshCard.tsx`
//! before slice 6c grows it.

import type { ApplyView } from "./sshApi";

export function SshCardResult({ result, note }: { result: ApplyView; note: string | null }) {
  return (
    <>
      <ul className="perm__list">
        {result.lines.map((l) => (
          <li key={l} className="perm__row">{l}</li>
        ))}
      </ul>
      {note && <p className="field-muted">{note}</p>}
    </>
  );
}
