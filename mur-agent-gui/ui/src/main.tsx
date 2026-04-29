import React from "react";
import ReactDOM from "react-dom/client";
import { getCurrentWindow } from "@tauri-apps/api/window";
import App from "./App";
import Logs from "./Logs";
import "./index.css";

// One Vite bundle for both windows; pick the root component by window
// label so Settings and Logs share assets but render different views.
async function bootstrap() {
  let label = "settings";
  try {
    label = getCurrentWindow().label;
  } catch {
    // Outside Tauri (e.g. `npm run dev` in browser preview) — default
    // to settings.
  }

  const Root = label === "logs" ? Logs : App;
  ReactDOM.createRoot(document.getElementById("root")!).render(
    <React.StrictMode>
      <Root />
    </React.StrictMode>,
  );
}

bootstrap();
