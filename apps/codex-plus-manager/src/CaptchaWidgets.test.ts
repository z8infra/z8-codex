import assert from "node:assert/strict";
import test from "node:test";
import { captchaFailureText } from "./CaptchaWidgets";

test("Cloudflare 110200 explains the desktop hostname configuration", () => {
  assert.match(captchaFailureText("110200", "localhost"), /localhost/);
  assert.match(captchaFailureText("110200", "localhost"), /Hostname/);
  assert.match(captchaFailureText("110200", "localhost"), /110200/);
});

test("other captcha failures keep their diagnostic code and retry guidance", () => {
  assert.equal(captchaFailureText("200500"), "安全验证加载失败（错误码：200500），请检查网络后重试。");
  assert.equal(captchaFailureText(null), "安全验证加载失败，请检查网络后重试。");
});
