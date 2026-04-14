// @vitest-environment node

import { describe, expect, it } from "vitest";

import viteConfig from "../vite.config";

describe("vite dev proxy", () => {
  it("includes journal and settings routes from the local control plane", async () => {
    const resolved =
      typeof viteConfig === "function"
        ? await viteConfig({
            command: "serve",
            mode: "test",
            isSsrBuild: false,
            isPreview: false,
          })
        : viteConfig;

    const proxy =
      "server" in resolved && resolved.server && "proxy" in resolved.server
        ? resolved.server.proxy
        : undefined;

    expect(proxy).toBeTruthy();
    expect(proxy).toHaveProperty("/journal");
    expect(proxy).toHaveProperty("/settings");
    expect(proxy).toHaveProperty("/events");
  });
});
