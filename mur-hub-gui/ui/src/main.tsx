import React from "react";
import ReactDOM from "react-dom/client";
import App from "./App";
import { LanguageProvider } from "./i18n";
import { ErrorBoundary } from "./components/ErrorBoundary";
import "./styles/index.css";

// Pet windows are transparent; the themed body background would otherwise
// paint them as an opaque square. Tag before the first render to avoid a flash.
if (window.location.hash.startsWith("#/pet/")) {
  document.body.classList.add("pet-window");
}

ReactDOM.createRoot(document.getElementById("root")!).render(
  <React.StrictMode>
    <ErrorBoundary>
      <LanguageProvider>
        <App />
      </LanguageProvider>
    </ErrorBoundary>
  </React.StrictMode>,
);
