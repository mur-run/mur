// "Voice never leaves this Mac" trust primitive (per roadmap §4.1).
// Apple Intelligence's "your face stays here" pattern, applied to
// speech: the user sees this every time they open Settings → Voice
// while voice is enabled, reinforcing the on-device promise.

export function PrivacyBadge() {
  return (
    <div
      className="rounded-md border p-3 text-sm"
      style={{
        borderColor: "var(--color-success, #047857)",
        background: "var(--color-bg-secondary)",
      }}
    >
      <div className="font-medium" style={{ color: "var(--color-fg)" }}>
        Your voice never leaves this Mac.
      </div>
      <div
        className="mt-1"
        style={{ color: "var(--color-fg-muted, var(--color-fg))", opacity: 0.85 }}
      >
        Speech recognition and synthesis run on-device. Audio is never
        uploaded to a server. Voice models live in
        <code className="mx-1">~/Library/Application Support/mur/voices/</code>
        and are verified with cryptographic signatures before use.
      </div>
    </div>
  );
}
