import { defineConfig } from "vite";
import preact from "@preact/preset-vite";

// Tauri 前端：开发期固定端口（tauri.conf.json 的 devUrl 指向这里），
// 构建产物输出到 dist/（tauri.conf.json 的 frontendDist）。
const host = process.env.TAURI_DEV_HOST;

export default defineConfig({
  plugins: [preact()],
  clearScreen: false,
  server: {
    port: 5173,
    strictPort: true,
    host: host || false,
    hmr: host ? { protocol: "ws", host, port: 5174 } : undefined,
    watch: { ignored: ["**/src-tauri/**"] },
  },
  envPrefix: ["VITE_", "TAURI_ENV_"],
  build: {
    target: "chrome105",
    minify: "esbuild",
    sourcemap: false,
  },
});
