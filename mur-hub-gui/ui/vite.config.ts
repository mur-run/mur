import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

// Hub uses port 5174 so it can run alongside mur-agent-gui (5173) during
// the parallel migration phase (M-h0 → M-h7).
export default defineConfig({
  plugins: [react()],
  clearScreen: false,
  server: {
    port: 5174,
    strictPort: true,
  },
  build: {
    target: "es2020",
    minify: "esbuild",
    sourcemap: false,
  },
});
