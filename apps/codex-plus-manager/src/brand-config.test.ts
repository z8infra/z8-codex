import test from "node:test";
import assert from "node:assert/strict";

import {
  isZ8FeatureEnabled,
  isZ8RouteDisabled,
  canApplyZ8StartupNavigation,
  resolveZ8DeepLink,
  shouldRequireZ8AccountOnStartup,
  Z8_BRAND,
  Z8_DISABLED_ROUTES,
  Z8_FEATURES,
} from "./brand-config";

test("Z8 brand config exposes a stable product identity", () => {
  assert.equal(Z8_BRAND.productName, "Z8 Codex");
  assert.equal(Z8_BRAND.managerName, "Z8 Codex");
  assert.equal(Z8_BRAND.windowTitle, "Z8 Codex");
  assert.equal(Z8_BRAND.mark, "Z8");
});

test("upstream-only navigation stays disabled for the Z8 release", () => {
  assert.equal(Z8_FEATURES.sponsorBoard, false);
  assert.equal(Z8_FEATURES.onlineUpdates, true);
  assert.equal(Z8_FEATURES.recommendations, false);
  assert.equal(Z8_FEATURES.dreamSkin, false);
  assert.equal(Z8_FEATURES.zedRemote, false);
  assert.equal(Z8_FEATURES.userScripts, false);
});

test("disabled upstream routes stay closed even when opened as a deep link", () => {
  assert.equal(isZ8RouteDisabled("grok"), true);
  assert.equal(resolveZ8DeepLink("grok"), "overview");
  for (const route of Z8_DISABLED_ROUTES) {
    assert.equal(isZ8RouteDisabled(route), true);
    assert.equal(resolveZ8DeepLink(route), "overview");
  }
  assert.equal(isZ8FeatureEnabled("recommendations"), false);
  assert.equal(isZ8FeatureEnabled(), true);
  assert.equal(resolveZ8DeepLink("about"), "about");
  assert.equal(resolveZ8DeepLink("unknown-upstream-page"), "overview");
});

test("first launch opens the Z8 account flow only for an unauthenticated overview", () => {
  assert.equal(shouldRequireZ8AccountOnStartup({ route: "overview", handledNavigation: false, showUpdate: false, status: "ok", authenticated: false }), true);
  assert.equal(shouldRequireZ8AccountOnStartup({ route: "overview", handledNavigation: false, showUpdate: false, status: "ok", authenticated: true }), false);
  assert.equal(shouldRequireZ8AccountOnStartup({ route: "about", handledNavigation: false, showUpdate: false, status: "ok", authenticated: false }), false);
  assert.equal(shouldRequireZ8AccountOnStartup({ route: "overview", handledNavigation: true, showUpdate: false, status: "ok", authenticated: false }), false);
  assert.equal(shouldRequireZ8AccountOnStartup({ route: "overview", handledNavigation: false, showUpdate: true, status: "ok", authenticated: false }), false);
});

test("startup navigation yields to a user route change", () => {
  assert.equal(canApplyZ8StartupNavigation({ route: "overview", startupRevision: 0, currentRevision: 0 }), true);
  assert.equal(canApplyZ8StartupNavigation({ route: "account", startupRevision: 0, currentRevision: 0 }), false);
  assert.equal(canApplyZ8StartupNavigation({ route: "overview", startupRevision: 0, currentRevision: 1 }), false);
});
