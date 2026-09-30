import assert from "node:assert/strict";
import { existsSync, readdirSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const managerDir = dirname(fileURLToPath(import.meta.url));
const assetsDir = join(managerDir, "dist", "assets");
const files = readdirSync(assetsDir);

assert(files.some((file) => /^z8-logo-[^.]+\.png$/.test(file)), "Z8 logo missing from production assets");
for (const name of ["dream-reference", "portal-hero"]) {
  assert(!files.some((file) => file.startsWith(`${name}-`)), `${name} leaked into Z8 production assets`);
}

for (const path of [
  join(managerDir, "..", "..", "assets", "inject", "upstream", "dream-skin", "windows", "dream-reference.jpg"),
  join(managerDir, "..", "..", "assets", "inject", "upstream", "dream-skin", "macos", "portal-hero.png"),
]) {
  assert(existsSync(path), `upstream source asset must be preserved: ${path}`);
}

console.log("Z8 production assets: logo present; hidden Dream Skin previews absent; upstream source assets preserved");
