import { useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { useT } from "../i18n";

function getAgentName(): string {
  const hash = window.location.hash; // #/pet/<name>
  return decodeURIComponent(hash.slice("#/pet/".length));
}

interface ContextMenu {
  visible: boolean;
  x: number;
  y: number;
}

interface BubbleState {
  text: string;
  dwell_ms: number;
  ack_required: boolean;
}

const CLICK_MS = 300;
const DRAG_THRESHOLD_PX = 4;

export function PetApp() {
  const { t } = useT();
  const agentName = getAgentName();
  const [expression, setExpression] = useState<string>("idle");
  const [imageSrc, setImageSrc] = useState<string>("");
  const [bubble, setBubble] = useState<BubbleState | null>(null);
  const [contextMenu, setContextMenu] = useState<ContextMenu>({ visible: false, x: 0, y: 0 });
  const clickTimeRef = useRef<number>(0);
  const pressRef = useRef<{ x: number; y: number } | null>(null);

  // Load expression image whenever expression changes.
  useEffect(() => {
    invoke<string>("pet_get_expression", { agentName, expression }).then((src) => {
      setImageSrc(src);
    });
  }, [agentName, expression]);

  // Listen for expression changes from the backend state machine. The listener
  // must be window-targeted: a global listen() (target Any) would also receive
  // the events emit_to() sends to every other pet.
  useEffect(() => {
    const unsub = getCurrentWindow().listen<string>("pet-expression", (ev) => {
      setExpression(ev.payload);
    });
    return () => { unsub.then((f) => f()); };
  }, []);

  // Listen for bubble messages from the backend.
  useEffect(() => {
    const unsub = getCurrentWindow().listen<string>("pet-bubble", (ev) => {
      if (ev.payload) {
        setBubble({ text: ev.payload, dwell_ms: 6000, ack_required: false });
      } else {
        setBubble(null);
      }
    });
    return () => { unsub.then((f) => f()); };
  }, []);

  // Persist position whenever the window is moved.
  useEffect(() => {
    const win = getCurrentWindow();
    const unsub = win.listen("tauri://move", () => {
      win.outerPosition().then((pos) => {
        invoke("pet_reposition", { agentName, x: pos.x, y: pos.y }).catch(() => {});
      });
    });
    return () => { unsub.then((f) => f()); };
  }, [agentName]);

  // Close context menu on click outside.
  useEffect(() => {
    if (!contextMenu.visible) return;
    function close() { setContextMenu((m) => ({ ...m, visible: false })); }
    window.addEventListener("click", close, { once: true });
    return () => window.removeEventListener("click", close);
  }, [contextMenu.visible]);

  // ESC closes bubble.
  useEffect(() => {
    function onKey(e: KeyboardEvent) {
      if (e.key === "Escape" && bubble) {
        setBubble(null);
      }
    }
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [bubble]);

  // Click vs drag: a native startDragging() on mousedown would swallow the
  // mouseup (the window server owns the drag loop), so clicks could never be
  // detected. Only hand off to the native drag once the cursor actually moves.
  function handleMouseDown(e: React.MouseEvent) {
    if (e.button !== 0) return;
    clickTimeRef.current = Date.now();
    pressRef.current = { x: e.screenX, y: e.screenY };
  }

  function handleMouseMove(e: React.MouseEvent) {
    const press = pressRef.current;
    if (!press || (e.buttons & 1) === 0) return;
    if (
      Math.abs(e.screenX - press.x) + Math.abs(e.screenY - press.y) >
      DRAG_THRESHOLD_PX
    ) {
      pressRef.current = null;
      getCurrentWindow().startDragging().catch(() => {});
    }
  }

  function handleMouseUp() {
    if (pressRef.current && Date.now() - clickTimeRef.current < CLICK_MS) {
      invoke("hub_emit_event", { agentName, eventName: "user.click.pet" }).catch(() => {});
    }
    pressRef.current = null;
  }

  function handleContextMenu(e: React.MouseEvent) {
    e.preventDefault();
    setContextMenu({ visible: true, x: e.clientX, y: e.clientY });
  }

  async function handleReturnToHub() {
    setContextMenu((m) => ({ ...m, visible: false }));
    await invoke("pet_return_to_hub", { agentName });
  }

  async function handleClose() {
    setContextMenu((m) => ({ ...m, visible: false }));
    await invoke("pet_close", { agentName });
  }

  function handleBubbleAck() {
    setBubble(null);
    invoke("pet_ack_bubble", { agentName }).catch(() => {});
  }

  const initials = agentName
    .split("-")
    .slice(0, 2)
    .map((w) => w[0]?.toUpperCase() ?? "")
    .join("");

  return (
    <div className="pet-root" onContextMenu={handleContextMenu}>
      {bubble && (
        <Bubble
          text={bubble.text}
          dwellMs={bubble.dwell_ms}
          onClose={handleBubbleAck}
        />
      )}

      <div
        className={`pet-sprite pet-sprite--${expression}`}
        onMouseDown={handleMouseDown}
        onMouseMove={handleMouseMove}
        onMouseUp={handleMouseUp}
      >
        {imageSrc ? (
          <img src={imageSrc} alt={agentName} className="pet-image" draggable={false} />
        ) : (
          <div className="pet-avatar-fallback">{initials}</div>
        )}
      </div>

      {contextMenu.visible && (
        <div
          className="pet-context-menu"
          style={{ left: contextMenu.x, top: contextMenu.y }}
        >
          <button className="pet-menu-item" onClick={handleReturnToHub}>📥 {t("pet.returnToHub")}</button>
          <div className="pet-menu-divider" />
          <button className="pet-menu-item pet-menu-item--danger" onClick={handleClose}>✕ {t("common.close")}</button>
        </div>
      )}
    </div>
  );
}

// ─── Bubble ──────────────────────────────────────────────────────────────────

interface BubbleProps {
  text: string;
  dwellMs: number;
  onClose: () => void;
}

function Bubble({ text, dwellMs, onClose }: BubbleProps) {
  const { t } = useT();
  const [remaining, setRemaining] = useState(dwellMs);
  const hoveredRef = useRef(false);
  const intervalRef = useRef<ReturnType<typeof setInterval> | null>(null);

  useEffect(() => {
    intervalRef.current = setInterval(() => {
      if (hoveredRef.current) return;
      setRemaining((r) => {
        const next = r - 100;
        if (next <= 0) { onClose(); return 0; }
        return next;
      });
    }, 100);
    return () => {
      if (intervalRef.current) clearInterval(intervalRef.current);
    };
  }, [onClose]);

  const pct = Math.max(0, remaining / dwellMs) * 100;

  return (
    <div
      className="pet-bubble"
      onMouseEnter={() => { hoveredRef.current = true; }}
      onMouseLeave={() => { hoveredRef.current = false; }}
    >
      <div className="pet-bubble-text">{text}</div>
      <div className="pet-bubble-progress">
        <div className="pet-bubble-bar" style={{ width: `${pct}%` }} />
      </div>
      <button className="pet-bubble-close" onClick={onClose} aria-label={t("pet.dismiss")}>✕</button>
    </div>
  );
}
