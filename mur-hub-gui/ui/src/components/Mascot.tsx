import { useCallback, useEffect, useRef, useState } from "react";
import { useT } from "../i18n";

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
  const birdRef = useRef<HTMLDivElement>(null);

  const [pupilOffset, setPupilOffset] = useState({ x: 0, y: 0 });
  const [isBlinking, setIsBlinking] = useState(false);
  const [isFluttering, setIsFluttering] = useState(false);
  const [showHoverBubble, setShowHoverBubble] = useState(false);
  const [localBubble, setLocalBubble] = useState<string | null>(null);

  const blinkTimer = useRef<ReturnType<typeof setTimeout>>();
  const quipTimer = useRef<ReturnType<typeof setTimeout>>();

  // ── Pupil tracking ────────────────────────────────────────────────────
  const handleMouseMove = useCallback((e: React.MouseEvent<HTMLDivElement>) => {
    const rect = birdRef.current?.getBoundingClientRect();
    if (!rect) return;
    const dx = e.clientX - (rect.left + rect.width / 2);
    const dy = e.clientY - (rect.top + rect.height / 2);
    const dist = Math.sqrt(dx * dx + dy * dy) || 1;
    const intensity = Math.min(dist / 60, 1);
    const MAX = 5;
    setPupilOffset({
      x: (dx / dist) * MAX * intensity,
      y: (dy / dist) * MAX * intensity,
    });
  }, []);

  const handleMouseEnter = useCallback(() => setShowHoverBubble(true), []);

  const handleMouseLeave = useCallback(() => {
    setPupilOffset({ x: 0, y: 0 });
    setShowHoverBubble(false);
  }, []);

  // ── Periodic blink ────────────────────────────────────────────────────
  function scheduleBlink() {
    blinkTimer.current = setTimeout(
      () => {
        setIsBlinking(true);
        setTimeout(() => {
          setIsBlinking(false);
          scheduleBlink();
        }, 130);
      },
      3500 + Math.random() * 4000,
    );
  }

  useEffect(() => {
    scheduleBlink();
    return () => {
      clearTimeout(blinkTimer.current);
      clearTimeout(quipTimer.current);
    };
  }, []);

  // ── Click: wing flutter + quip bubble ─────────────────────────────────
  function handleClick() {
    setIsFluttering(true);
    setTimeout(() => setIsFluttering(false), 500);

    const key = QUIPS[Math.floor(Math.random() * QUIPS.length)];
    setLocalBubble(t(key));
    clearTimeout(quipTimer.current);
    quipTimer.current = setTimeout(() => setLocalBubble(null), QUIP_DWELL_MS);
  }

  // ── Derived state ─────────────────────────────────────────────────────
  const displayBubble =
    localBubble ?? (showHoverBubble && bubble ? bubble : null);

  // Flutter overrides float so the bounce animation plays cleanly
  const floatClass = isFluttering
    ? "bird--flutter"
    : floating
      ? "bird--float"
      : "";
  const moodClass = mood !== "idle" ? `bird--${mood}` : "";
  const blinkClass = isBlinking ? "bird--blink" : "";

  const pupilStyle: React.CSSProperties = {
    transform: `translate(${pupilOffset.x}px, ${pupilOffset.y}px)`,
    transition:
      pupilOffset.x === 0 && pupilOffset.y === 0
        ? "transform 0.3s ease-out"
        : "transform 0.08s linear",
  };

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
        ref={birdRef}
        className={`bird ${floatClass} ${moodClass} ${blinkClass}`}
        style={{ width: size, height: size * 0.94 }}
        onMouseMove={handleMouseMove}
        onMouseEnter={handleMouseEnter}
        onMouseLeave={handleMouseLeave}
        onClick={handleClick}
        role="img"
        aria-label="MUR mascot"
      >
        <div className="bird__feet" />
        <div className="bird__body" />
        <div className="bird__wing bird__wing--l" />
        <div className="bird__wing bird__wing--r" />
        <div className="bird__brow bird__brow--l" />
        <div className="bird__brow bird__brow--r" />
        <div className="bird__eye bird__eye--l">
          <div className="bird__pupil" style={pupilStyle} />
        </div>
        <div className="bird__eye bird__eye--r">
          <div className="bird__pupil" style={pupilStyle} />
        </div>
        <div className="bird__beak" />
      </div>
    </div>
  );
}
