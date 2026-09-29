import test from "node:test";
import assert from "node:assert/strict";
import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";

test("Z8 first-render navigation and overview omit upstream-only surfaces", async () => {
  // App chooses platform-specific controls at module load; no browser or Tauri
  // runtime is needed to inspect the actual initial React output.
  Object.defineProperty(globalThis, "navigator", {
    configurable: true,
    value: { userAgent: "Windows" },
  });
  const { App } = await import("./App");
  const markup = renderToStaticMarkup(createElement(App));
  const navigation = markup.match(/<nav\b[^>]*>([\s\S]*?)<\/nav>/)?.[1];

  assert.ok(navigation, "Manager renders its navigation");
  assert.match(navigation, /Z8 账户/);
  assert.doesNotMatch(navigation, /推荐内容|Zed 远程项目/);
  assert.doesNotMatch(markup, /赞助商推荐|暂无赞助商推荐/);
});
