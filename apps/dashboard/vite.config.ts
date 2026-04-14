import { defineConfig, loadEnv } from "vite";
import react from "@vitejs/plugin-react";

const HTTP_ENDPOINTS = [
  "/health",
  "/status",
  "/readiness",
  "/history",
  "/journal",
  "/settings",
  "/strategies",
  "/strategies/validate",
  "/runtime/commands",
  "/commands",
];

export default defineConfig(({ mode }) => {
  const env = loadEnv(mode, process.cwd(), "");
  const controlApiProxyTarget =
    env.VITE_CONTROL_API_PROXY_TARGET ?? "http://127.0.0.1:8080";
  const controlApiWebSocketProxyTarget =
    env.VITE_CONTROL_API_WS_PROXY_TARGET ?? "ws://127.0.0.1:8081";

  return {
    plugins: [react()],
    server: {
      host: "127.0.0.1",
      port: 4173,
      proxy: {
        ...Object.fromEntries(
          HTTP_ENDPOINTS.map((endpoint) => [
            endpoint,
            {
              target: controlApiProxyTarget,
              changeOrigin: true,
            },
          ]),
        ),
        "/events": {
          target: controlApiWebSocketProxyTarget,
          ws: true,
          changeOrigin: true,
        },
      },
    },
    test: {
      environment: "jsdom",
      setupFiles: "./src/test/setup.ts",
      css: true,
    },
  };
});
