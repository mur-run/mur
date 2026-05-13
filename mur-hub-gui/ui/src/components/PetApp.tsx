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

const CLICK_MS = 300;

export function PetApp() {
  const agentName = getAgentName();
  const [expression, setExpression] = useState<string>("idle");
  const [imageSrc, setImageSrc] = useState<string>("");
  const [contextMenu, setContextMenu] = useState<ContextMenu>({ visible: false, x: 0, y: 0 });
  const clickTimeRef = useRef<number>(0);
  const isDraggingRef = useRef(false);

  // Load expression image whenever expression changes.
  useEffect(() => {
    invoke<string>("pet_get_expression", { agentName, expression }).then((src) => {
      setImageSrc(src);
    });
  }, [agentName, expression]);

  // Listen for expression-change events emitted by the backend (spawn wave sequence).
  useEffect(() => {
    const unsub = listen<string>("pet-expression", (ev) => {
      setExpression(ev.payload);
    });
    return () => { unsub.then((f) => f()); };
  }, []);

  // Persist position whenever the window is moved via drag.
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

  function handleMouseDown(e: React.MouseEvent) {
    if (e.button !== 0) return;
    clickTimeRef.current = Date.now();
    isDraggingRef.current = false;
    // Engage native window drag; Tauri moves the window.
    getCurrentWindow().startDragging().then(() => {
      isDraggingRef.current = true;
    }).catch(() => {});
  }

  function handleMouseUp() {
    // Short tap without drag → smile for 2s.
    if (!isDraggingRef.current && Date.now() - clickTimeRef.current < CLICK_MS) {
      setExpression("smile");
      setTimeout(() => setExpression("idle"), 2000);
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

  const initials = agentName.split("-").slice(0, 2).map((w) => w[0]?.toUpperCase() ?? "").join("");

  return (
    <div className="pet-root" onContextMenu={handleContextMenu}>
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
