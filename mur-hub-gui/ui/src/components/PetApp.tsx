import { useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { getCurrentWindow } from "@tauri-apps/api/window";

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

export function PetApp() {
  const agentName = getAgentName();
  const [expression, setExpression] = useState<string>("idle");
  const [imageSrc, setImageSrc] = useState<string>("");
  const [bubble, setBubble] = useState<BubbleState | null>(null);
  const [contextMenu, setContextMenu] = useState<ContextMenu>({ visible: false, x: 0, y: 0 });
  const clickTimeRef = useRef<number>(0);
  const isDraggingRef = useRef(false);

  // Load expression image whenever expression changes.
  useEffect(() => {
    invoke<string>("pet_get_expression", { agentName, expression }).then((src) => {
      setImageSrc(src);
    });
  }, [agentName, expression]);

  // Listen for expression changes from the backend state machine.
  useEffect(() => {
    const unsub = listen<string>("pet-expression", (ev) => {
      setExpression(ev.payload);
    });
    return () => { unsub.then((f) => f()); };
  }, []);

  // Listen for bubble messages from the backend.
  useEffect(() => {
    const unsub = listen<string>("pet-bubble", (ev) => {
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

  function handleMouseDown(e: React.MouseEvent) {
    if (e.button !== 0) return;
    clickTimeRef.current = Date.now();
    isDraggingRef.current = false;
    getCurrentWindow().startDragging().then(() => {
      isDraggingRef.current = true;
    }).catch(() => {});
  }

  function handleMouseUp() {
    if (!isDraggingRef.current && Date.now() - clickTimeRef.current < CLICK_MS) {
      invoke("hub_emit_event", { agentName, eventName: "user.click.pet" }).catch(() => {});
    }
    isDraggingRef.current = false;
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
          <button className="pet-menu-item" onClick={handleReturnToHub}>📥 Return to Hub</button>
          <div className="pet-menu-divider" />
          <button className="pet-menu-item pet-menu-item--danger" onClick={handleClose}>✕ Close</button>
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
      <button className="pet-bubble-close" onClick={onClose} aria-label="Dismiss">✕</button>
    </div>
  );
}
