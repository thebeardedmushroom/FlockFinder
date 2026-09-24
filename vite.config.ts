import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

// Set by `tauri android dev` when the device must reach the dev server on a LAN address
// (physical phones); emulators use localhost through `adb reverse`.
const host = process.env.TAURI_DEV_HOST;

// https://vite.dev/config/
export default defineConfig({
  plugins: [react()],
  clearScreen: false,
  server: {
    port: 14210,
    strictPort: true,
    host: host || false,
    hmr: host ? { protocol: "ws", host, port: 14211 } : undefined,
    watch: { ignored: ["**/src-tauri/**"] },
  },
  build: {
    target: "es2022",
    sourcemap: false,
  },
  worker: {
    format: "es",
  },
  test: {
    environment: "jsdom",
    include: ["src/**/*.test.ts", "src/**/*.test.tsx"],
  },
});
