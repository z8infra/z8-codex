import assert from "node:assert/strict";
import { readdirSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const managerDir = dirname(fileURLToPath(import.meta.url));
const assetsDir = join(managerDir, "dist", "assets");
const files = readdirSync(assetsDir);

assert(files.some((file) => /^z8-logo-[^.]+\.png$/.test(file)), "Z8 logo missing from production assets");
console.log("Z8 production assets: logo present; optional preview assets omitted");
