import { useEffect, useRef, useState } from "react";
import { useT } from "../i18n";
import { MUR_MASCOT_SVG } from "./murMascotSvg";

export type MascotMood = "idle" | "happy" | "worried" | "excited";

interface MascotProps {
  size?: number;
  floating?: boolean;
  className?: string;
  mood?: MascotMood;
  /** Fleet-context message shown on hover; click quips take priority. */
  bubble?: string | null;
}

const QUIPS = [
  "mascot.quip.0",
  "mascot.quip.1",
  "mascot.quip.2",
  "mascot.quip.3",
  "mascot.quip.4",
  "mascot.quip.5",
  "mascot.quip.6",
  "mascot.quip.7",
  "mascot.quip.8",
  "mascot.quip.9",
] as const;

const QUIP_DWELL_MS = 2800;

export function Mascot({
  size = 66,
  floating = false,
  className = "",
  mood = "idle",
  bubble,
}: MascotProps) {
  const { t } = useT();

  const [isFluttering, setIsFluttering] = useState(false);
  const [showHoverBubble, setShowHoverBubble] = useState(false);
  const [localBubble, setLocalBubble] = useState<string | null>(null);

  const quipTimer = useRef<ReturnType<typeof setTimeout>>();

  useEffect(() => () => clearTimeout(quipTimer.current), []);

  // ── Click: flutter bounce + quip bubble ─────────────────────────────────
  function handleClick() {
    setIsFluttering(true);
    setTimeout(() => setIsFluttering(false), 500);

    const key = QUIPS[Math.floor(Math.random() * QUIPS.length)];
    setLocalBubble(t(key));
    clearTimeout(quipTimer.current);
    quipTimer.current = setTimeout(() => setLocalBubble(null), QUIP_DWELL_MS);
  }

  const displayBubble =
    localBubble ?? (showHoverBubble && bubble ? bubble : null);

  // Flutter overrides float so the bounce plays cleanly.
  const floatClass = isFluttering
    ? "bird--flutter"
    : floating
      ? "bird--float"
      : "";
  const moodClass = mood !== "idle" ? `bird--${mood}` : "";

  return (
    <div className={`bird-wrap ${className}`}>
      {displayBubble && (
        <div className="bird-bubble" role="status">
          <span className="bird-bubble__text">{displayBubble}</span>
          <button
            className="bird-bubble__close"
            aria-label="Dismiss"
            onClick={(e) => {
              e.stopPropagation();
              setLocalBubble(null);
              setShowHoverBubble(false);
            }}
          >
            ✕
          </button>
        </div>
      )}
      <div
        className={`bird ${floatClass} ${moodClass}`}
        style={{ width: size, height: size }}
        onMouseEnter={() => setShowHoverBubble(true)}
        onMouseLeave={() => setShowHoverBubble(false)}
        onClick={handleClick}
        // ponytail: trusted, self-authored SVG (no user input) → safe inject.
        dangerouslySetInnerHTML={{ __html: MUR_MASCOT_SVG }}
      />
    </div>
  );
}
