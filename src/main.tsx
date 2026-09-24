import React from "react";
import ReactDOM from "react-dom/client";
import "maplibre-gl/dist/maplibre-gl.css";
import "./styles.css";
import App from "./App";
import { inTauri } from "./lib/ipc";
import { invoke } from "@tauri-apps/api/core";

// `npm run dev` + http://localhost:14210/?mock=1 runs the UI against an in-memory mock of
// the Rust backend fed by the bundled Overpass fixture (development only).
if (import.meta.env.DEV && !inTauri() && new URLSearchParams(window.location.search).has("mock")) {
  const { installDevMock } = await import("./lib/devMock");
  installDevMock();
}

// Forward webview errors into the Rust log file so problems are diagnosable from the
// terminal / log directory without opening devtools.
if (inTauri()) {
  const forward = (level: string, message: string) => {
    void invoke("frontend_log", { level, message }).catch(() => {});
  };
  window.addEventListener("error", (e) => forward("error", `${e.message} (${e.filename}:${e.lineno})`));
  window.addEventListener("unhandledrejection", (e) =>
    forward("error", `unhandled rejection: ${String((e as PromiseRejectionEvent).reason)}`),
  );
  const origError = console.error.bind(console);
  console.error = (...args: unknown[]) => {
    origError(...args);
    forward("error", args.map((a) => (a instanceof Error ? a.message : String(a))).join(" "));
  };
  const origWarn = console.warn.bind(console);
  console.warn = (...args: unknown[]) => {
    origWarn(...args);
    forward("warn", args.map((a) => (a instanceof Error ? a.message : String(a))).join(" "));
  };
}

ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(
  <React.StrictMode>
    <App />
  </React.StrictMode>,
);
