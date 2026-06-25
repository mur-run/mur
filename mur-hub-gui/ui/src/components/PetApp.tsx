import { useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { useT } from "../i18n";
import { PetFace } from "./PetFace";

interface PetAppearance {
  style_preset: string;
  family: string;
  has_ai_art: boolean;
}

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
  const [appearance, setAppearance] = useState<PetAppearance | null>(null);
  const [bubble, setBubble] = useState<BubbleState | null>(null);
  const [contextMenu, setContextMenu] = useState<ContextMenu>({ visible: false, x: 0, y: 0 });
  const [muted, setMuted] = useState(false);
  const mutedRef = useRef(false); // read inside the bubble listener (stable closure)
  const clickTimeRef = useRef<number>(0);
  const pressRef = useRef<{ x: number; y: number } | null>(null);

  // Resolve the pet's style/family once; decides AI art vs. vector mascot.
  useEffect(() => {
    invoke<PetAppearance>("pet_get_appearance", { agentName })
      .then(setAppearance)
      .catch(() => setAppearance({ style_preset: "default-blob", family: "chibi", has_ai_art: false }));
  }, [agentName]);

  // OS file-drop onto this pet (forwarded from the backend): show a "reading"
  // bubble, then the agent's quick take. The full content also opens in chat.
  // This is user-initiated, so it shows even when muted.
  useEffect(() => {
    const unsub = getCurrentWindow().listen<string[]>("pet://drop", (ev) => {
      const paths = ev.payload;
      if (!paths || paths.length === 0) return;
      setBubble({ text: t("pet.dropThinking"), dwell_ms: 60000, ack_required: false });
      invoke<{ reply: string; text_files: number; skipped: string[] }>("pet_drop_files", {
        agentName,
        paths,
      })
        .then((res) => {
          const base = res.reply || t("pet.dropOpened");
          const skipped = res.skipped?.length
            ? ` ${t("pet.dropSkipped", { count: res.skipped.length })}`
            : "";
          setBubble({
            text: base + skipped,
            dwell_ms: res.reply ? 12000 : 6000,
            ack_required: false,
          });
        })
        .catch((e) => setBubble({ text: String(e), dwell_ms: 6000, ack_required: false }));
    });
    return () => { unsub.then((f) => f()); };
  }, [agentName, t]);

  // Load the AI expression image when (and only when) real art exists; otherwise
  // the vector PetFace renders the expression and no fetch is needed.
  useEffect(() => {
    if (!appearance?.has_ai_art) {
      setImageSrc("");
      return;
    }
    invoke<string>("pet_get_expression", { agentName, expression }).then(setImageSrc);
  }, [agentName, expression, appearance?.has_ai_art]);

  // Listen for expression changes from the backend state machine. The listener
  // must be window-targeted: a global listen() (target Any) would also receive
  // the events emit_to() sends to every other pet.
  useEffect(() => {
    const unsub = getCurrentWindow().listen<string>("pet-expression", (ev) => {
      setExpression(ev.payload);
    });
    return () => { unsub.then((f) => f()); };
  }, []);

  // Listen for bubble messages from the backend. When muted, drop them — this is
  // the pet's "do not disturb": no pop-up nags.
  useEffect(() => {
    const unsub = getCurrentWindow().listen<string>("pet-bubble", (ev) => {
      if (mutedRef.current) return;
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
      // Greeting expression + open the chat panel (single-click is primary).
      invoke("hub_emit_event", { agentName, eventName: "user.click.pet" }).catch(() => {});
      void handleChat();
    }
    pressRef.current = null;
  }

  function handleContextMenu(e: React.MouseEvent) {
    e.preventDefault();
    // The pet window is small (200×200); clamp the menu so every item stays
    // visible (anchor near the edges) instead of clipping past the window.
    const MENU_W = 150;
    const MENU_H = 188;
    const x = Math.max(2, Math.min(e.clientX, window.innerWidth - MENU_W));
    const y = Math.max(2, Math.min(e.clientY, window.innerHeight - MENU_H));
    setContextMenu({ visible: true, x, y });
  }

  function closeMenu() {
    setContextMenu((m) => ({ ...m, visible: false }));
  }

  async function handleChat() {
    closeMenu();
    await invoke("pet_open_chat", { agentName, draft: null }).catch(() => {});
  }

  function handleToggleMute() {
    closeMenu();
    setMuted((m) => {
      const next = !m;
      mutedRef.current = next;
      if (next) setBubble(null);
      return next;
    });
  }

  function handleHide1h() {
    closeMenu();
    const win = getCurrentWindow();
    win.hide().catch(() => {});
    // Re-show after an hour; the backend event loop keeps running while hidden.
    window.setTimeout(() => { win.show().catch(() => {}); }, 60 * 60 * 1000);
  }

  async function handleSettings() {
    closeMenu();
    await invoke("open_dashboard", { agentName }).catch(() => {});
  }

  async function handleReturnToHub() {
    closeMenu();
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
        onDoubleClick={handleChat}
        title={t("pet.chat")}
      >
        {imageSrc ? (
          <img src={imageSrc} alt={agentName} className="pet-image" draggable={false} />
        ) : appearance ? (
          <PetFace
            presetId={appearance.style_preset}
            family={appearance.family}
            expression={expression}
            size={150}
          />
        ) : (
          <div className="pet-avatar-fallback">{initials}</div>
        )}
      </div>

      {contextMenu.visible && (
        <div
          className="pet-context-menu"
          style={{ left: contextMenu.x, top: contextMenu.y }}
        >
          <button className="pet-menu-item" onClick={handleChat}>💬 {t("pet.chat")}</button>
          <button className="pet-menu-item" onClick={handleToggleMute}>
            {muted ? `🔔 ${t("pet.unmute")}` : `🔇 ${t("pet.mute")}`}
          </button>
          <button className="pet-menu-item" onClick={handleHide1h}>👁 {t("pet.hide1h")}</button>
          <button className="pet-menu-item" onClick={handleSettings}>⚙ {t("pet.settings")}</button>
          <div className="pet-menu-divider" />
          <button className="pet-menu-item" onClick={handleReturnToHub}>📥 {t("pet.returnToHub")}</button>
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

  // The same <Bubble> instance is reused when the message changes (e.g. the
  // file-drop "reading…" 60s bubble → the 12s reply). useState only seeds
  // `remaining` once, so reset it whenever the message/dwell changes — else the
  // countdown (and the progress bar) keep the stale value and over-run.
  useEffect(() => {
    setRemaining(dwellMs);
  }, [dwellMs, text]);

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
