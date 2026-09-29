import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { existsSync } from "node:fs";
import { mkdtemp, mkdir, readFile, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { test } from "node:test";

const source = await readFile(new URL("../../../scripts/installer/macos/package-dmg.sh", import.meta.url), "utf8");
const background = await readFile(
  new URL("../../../assets/installer/macos/dmg-background.svg", import.meta.url),
  "utf8",
);
assert.match(background, /Z8 Codex 安装指南/);
assert.match(background, /Z8 Codex 和 Z8 Codex 管理工具/);
assert.doesNotMatch(background, /Codex\+\+/);
assert.match(background, /width="1200" height="760" viewBox="0 0 1200 760"/);
const start = source.indexOf('DMG_WORK_DIR="$(mktemp');
assert.ok(start >= 0, "the real DMG lifecycle must be exercised");
const preflightStart = source.indexOf("validate_bundle_version() {");
const preflightEnd = source.indexOf('\nrm -rf "$DIST"', preflightStart);
assert.ok(preflightStart >= 0 && preflightEnd > preflightStart, "version and architecture preflight must precede output replacement");
const appStart = source.indexOf("create_app() {");
const appEnd = source.indexOf("\nsign_app() {", appStart);
assert.ok(appStart >= 0 && appEnd > appStart, "the real bundle writer must be exercised");
const verifyStart = source.indexOf("verify_app() {");
const verifyEnd = source.indexOf('\nprepare_icon\n', verifyStart);
assert.ok(verifyStart >= 0 && verifyEnd > verifyStart, "the real signature verification must be exercised");
function resolveWindowsBash() {
  const override = process.env.GIT_BASH_PATH?.trim();
  if (override) return override;

  const candidates = [
    join(process.env.ProgramFiles || "C:\\Program Files", "Git", "bin", "bash.exe"),
    join(process.env.ProgramFiles || "C:\\Program Files", "Git", "usr", "bin", "bash.exe"),
  ];
  const execPath = spawnSync("git", ["--exec-path"], { encoding: "utf8" }).stdout.trim();
  if (execPath) {
    const gitRoot = resolve(execPath, "..", "..", "..");
    candidates.push(join(gitRoot, "bin", "bash.exe"), join(gitRoot, "usr", "bin", "bash.exe"));
  }
  return candidates.find((candidate) => existsSync(candidate)) ?? candidates[0];
}

const bash = process.platform === "win32" ? resolveWindowsBash() : "/bin/bash";

async function runArchitecturePreflight(
  packageArch: string,
  launcherArch: string,
  managerArch: string,
  version = "1.3.0",
  buildNumber = "",
) {
  const parent = resolve(tmpdir());
  const dir = await mkdtemp(join(parent, "z8-macos-arch-test-"));
  try {
    const fixture = `
set -euo pipefail
ARCH="$MOCK_PACKAGE_ARCH"
VERSION="$MOCK_VERSION"
BINARY_DIR="$PWD/bin"
mkdir -p "$BINARY_DIR"
for executable in codex-plus-plus codex-plus-plus-manager; do
  printf '#!/bin/sh\\n' > "$BINARY_DIR/$executable"
  chmod +x "$BINARY_DIR/$executable"
done
lipo() {
  case "$2" in
    */codex-plus-plus) printf '%s\\n' "$MOCK_LAUNCHER_ARCH" ;;
    */codex-plus-plus-manager) printf '%s\\n' "$MOCK_MANAGER_ARCH" ;;
    *) return 1 ;;
  esac
}
`;
    const result = spawnSync(bash, ["--noprofile", "--norc"], {
      cwd: dir,
      input: `${fixture}\n${source.slice(preflightStart, preflightEnd)}\nprintf 'bundle:%s:%s\\n' "$BUNDLE_SHORT_VERSION" "$BUNDLE_BUILD_VERSION"\n`,
      encoding: "utf8",
      timeout: 15_000,
      env: {
        ...process.env,
        BASH_ENV: "",
        MOCK_PACKAGE_ARCH: packageArch,
        MOCK_LAUNCHER_ARCH: launcherArch,
        MOCK_MANAGER_ARCH: managerArch,
        MOCK_VERSION: version,
        MACOS_BUILD_NUMBER: buildNumber,
      },
    });
    assert.ifError(result.error);
    assert.equal(result.signal, null, result.stderr);
    return result;
  } finally {
    assert.equal(dirname(resolve(dir)), parent);
    await rm(dir, { recursive: true, force: true });
  }
}

for (const [packageArch, nativeArch] of [["x64", "x86_64"], ["arm64", "arm64"]]) {
  test(`DMG input preflight accepts two native ${packageArch} binaries`, async () => {
    const result = await runArchitecturePreflight(packageArch, nativeArch, nativeArch);
    assert.equal(result.status, 0, result.stderr);
  });

  test(`DMG input preflight rejects a mislabeled ${packageArch} manager`, async () => {
    const wrongArch = nativeArch === "arm64" ? "x86_64" : "arm64";
    const result = await runArchitecturePreflight(packageArch, nativeArch, wrongArch);
    assert.equal(result.status, 1, result.stderr);
    assert.match(result.stderr, /expected .* binary/);
  });
}

test("DMG input preflight rejects a fat binary under a single-architecture name", async () => {
  const result = await runArchitecturePreflight("arm64", "arm64 x86_64", "arm64");
  assert.equal(result.status, 1, result.stderr);
  assert.match(result.stderr, /expected arm64 binary/);
});

test("DMG input preflight rejects unsupported architecture", async () => {
  const result = await runArchitecturePreflight("i386", "x86_64", "x86_64");
  assert.equal(result.status, 1, result.stderr);
  assert.match(result.stderr, /unsupported macOS architecture/);
});

test("DMG preflight keeps a prerelease filename but writes numeric bundle versions", async () => {
  const result = await runArchitecturePreflight("arm64", "arm64", "arm64", "1.3.0-rc.2", "42");
  assert.equal(result.status, 0, result.stderr);
  assert.match(result.stdout, /bundle:1\.3\.0:42/);
});

for (const [version, buildNumber] of [["1.3.0<&", "42"], ["1.3.0", "42-rc"]]) {
  test(`DMG preflight rejects invalid bundle metadata ${version}/${buildNumber}`, async () => {
    const result = await runArchitecturePreflight("x64", "x86_64", "x86_64", version, buildNumber);
    assert.equal(result.status, 1, result.stderr);
    assert.match(result.stderr, /invalid macOS/);
  });
}

test("DMG bundle writer records numeric versions, Z8 identity, and the expected executable", async () => {
  const parent = resolve(tmpdir());
  const dir = await mkdtemp(join(parent, "z8-macos-plist-test-"));
  try {
    const fixture = `
set -euo pipefail
STAGE="$PWD/stage"
ICON_ICNS="$PWD/z8-codex.icns"
ICON_NAME="z8-codex.icns"
BUNDLE_SHORT_VERSION="1.3.0"
BUNDLE_BUILD_VERSION="42"
mkdir -p "$STAGE"
printf icon > "$ICON_ICNS"
printf '#!/bin/sh\\n' > launcher
chmod +x launcher
`;
    const result = spawnSync(bash, ["--noprofile", "--norc"], {
      cwd: dir,
      input: `${fixture}\n${source.slice(appStart, appEnd)}\ncreate_app 'Z8 Codex' 'CodexPlusPlus' "$PWD/launcher" 'com.z8.codex' 'true'\ncat "$STAGE/Z8 Codex.app/Contents/Info.plist"\n`,
      encoding: "utf8",
      timeout: 15_000,
      env: { ...process.env, BASH_ENV: "" },
    });
    assert.ifError(result.error);
    assert.equal(result.status, 0, result.stderr);
    assert.match(result.stdout, /<key>CFBundleVersion<\/key>\s*<string>42<\/string>/);
    assert.match(result.stdout, /<key>CFBundleShortVersionString<\/key>\s*<string>1\.3\.0<\/string>/);
    assert.match(result.stdout, /<key>CFBundleIdentifier<\/key>\s*<string>com\.z8\.codex<\/string>/);
    assert.match(result.stdout, /<key>CFBundleExecutable<\/key>\s*<string>CodexPlusPlus<\/string>/);
  } finally {
    assert.equal(dirname(resolve(dir)), parent);
    await rm(dir, { recursive: true, force: true });
  }
});

test("DMG bundle verification rejects a failing codesign verification", async () => {
  const parent = resolve(tmpdir());
  const dir = await mkdtemp(join(parent, "z8-macos-sign-test-"));
  try {
    const fixture = `
set -euo pipefail
mkdir -p app/Contents
printf plist > app/Contents/Info.plist
printf 'APPL????' > app/Contents/PkgInfo
plutil() { return 0; }
codesign() {
  printf '%s\\n' "$*" > codesign-args.txt
  return 1
}
`;
    const result = spawnSync(bash, ["--noprofile", "--norc"], {
      cwd: dir,
      input: `${fixture}\n${source.slice(verifyStart, verifyEnd)}\nverify_app "$PWD/app"\n`,
      encoding: "utf8",
      timeout: 15_000,
      env: { ...process.env, BASH_ENV: "" },
    });
    assert.ifError(result.error);
    assert.equal(result.status, 1, result.stderr);
    assert.match(result.stderr, /codesign verification failed/);
    assert.match(await readFile(join(dir, "codesign-args.txt"), "utf8"), /^--verify --deep --strict /);
  } finally {
    assert.equal(dirname(resolve(dir)), parent);
    await rm(dir, { recursive: true, force: true });
  }
});

// Run the actual packaging tail with synthetic disk commands, never real mounts.
const fixture = `
set -euo pipefail
DMG="out/final.dmg"
DIST="out"
STAGE="stage"
ATTACHED=1
CONVERT_CALLS=0
DETACH_CALLS=0
OUTPUT_WAITS=0
mktemp() { printf '%s\\n' "work"; }
sleep() {
  if [ "$SCENARIO" = delayed-output ] && [ "$CONVERT_CALLS" -gt 0 ]; then
    OUTPUT_WAITS=$((OUTPUT_WAITS + 1))
    if [ "$OUTPUT_WAITS" -eq 2 ]; then printf 'compressed image' > "$DMG"; fi
  fi
  return 0
}
rm() { :; }
rmdir() { :; }
osascript() { cat >/dev/null; }
hdiutil() {
  printf '%s\\n' "$*" >> trace.log
  case "$1" in
    create) printf 'writable image' > "$DMG_WORK_PATH" ;;
    attach)
      case "$SCENARIO" in
        no-device) printf 'Apple_HFS\\t/Volumes/fixture\\n' ;;
        no-volume) printf '/dev/disk4\\tApple_HFS\\n' ;;
        *) printf '/dev/disk4\\tApple_HFS\\t/Volumes/fixture\\n' ;;
      esac
      ;;
    detach)
      DETACH_CALLS=$((DETACH_CALLS + 1))
      case "$SCENARIO" in
        busy|info-error) return 1 ;;
        delayed|no-device) if [ "$DETACH_CALLS" -eq 1 ]; then return 1; fi ;;
        force-only) if [ "\${3:-}" != -force ]; then return 1; fi ;;
        already-gone) ATTACHED=0; return 1 ;;
      esac
      ATTACHED=0
      ;;
    info)
      if [ "$SCENARIO" = info-error ]; then return 1; fi
      if [ "$ATTACHED" -eq 1 ]; then printf '/dev/disk4\\tApple_HFS\\n'; fi
      printf '/dev/disk40\\tApple_HFS\\n'
      ;;
    convert)
      CONVERT_CALLS=$((CONVERT_CALLS + 1))
      if [ "$ATTACHED" -ne 0 ]; then return 1; fi
      case "$SCENARIO" in
        fail-convert) return 1 ;;
        missing-output|delayed-output) return 0 ;;
        empty-output) : > "$DMG"; return 0 ;;
        retry-convert) if [ "$CONVERT_CALLS" -lt 3 ]; then return 1; fi ;;
        late-convert) if [ "$CONVERT_CALLS" -lt 7 ]; then return 1; fi ;;
      esac
      printf 'compressed image' > "$DMG"
      ;;
    *) return 99 ;;
  esac
}
`;

async function runScenario(scenario: string) {
  const parent = resolve(tmpdir());
  const dir = await mkdtemp(join(parent, "cpp-dmg-test-"));
  try {
    await mkdir(join(dir, "work"));
    await mkdir(join(dir, "out"));
    const result = spawnSync(bash, ["--noprofile", "--norc"], {
      cwd: dir,
      input: `${fixture}\n${source.slice(start)}`,
      encoding: "utf8",
      timeout: 15_000,
      env: { ...process.env, BASH_ENV: "", SCENARIO: scenario },
    });
    assert.ifError(result.error);
    assert.equal(result.signal, null, result.stderr);
    const trace = await readFile(join(dir, "trace.log"), "utf8");
    return { ...result, trace };
  } finally {
    assert.equal(dirname(resolve(dir)), parent);
    await rm(dir, { recursive: true, force: true });
  }
}

for (const scenario of ["success", "retry-convert", "late-convert", "delayed-output", "delayed", "already-gone", "force-only"]) {
  test(`DMG packaging succeeds after ${scenario}`, async () => {
    const result = await runScenario(scenario);
    assert.equal(result.status, 0, result.stderr);
    assert.match(result.stdout, /out\/final\.dmg/);
    assert.match(result.trace, /detach \/dev\/disk4(?:\n| )/);
    if (scenario === "retry-convert") {
      assert.equal(result.trace.match(/^convert /gm)?.length, 3);
    }
    if (scenario === "late-convert") {
      assert.equal(result.trace.match(/^convert /gm)?.length, 7);
    }
    if (scenario === "delayed-output") {
      assert.equal(result.trace.match(/^convert /gm)?.length, 1);
    }
    if (scenario === "delayed") {
      // A normal detach failure is followed immediately by force detach. This
      // handles the transient state where the volume is gone but the device is
      // still registered (the macOS CI failure fixed upstream).
      assert.equal(result.trace.match(/^detach /gm)?.length, 2);
      assert.match(result.trace, /detach \/dev\/disk4 -force/);
    }
    if (scenario === "force-only") {
      // Force detach is retried inside the loop, so the second call succeeds.
      assert.equal(result.trace.match(/^detach /gm)?.length, 2);
      assert.match(result.trace, /detach \/dev\/disk4 -force/);
    }
  });
}

for (const scenario of ["no-device", "no-volume"]) {
  test(`DMG packaging cleans up failed attach parsing: ${scenario}`, async () => {
    const result = await runScenario(scenario);
    assert.equal(result.status, 1, result.stderr);
    assert.match(result.stderr, /failed to find mounted DMG device and volume/);
    assert.doesNotMatch(result.trace, /^convert /m);
    if (scenario === "no-device") {
      assert.equal(result.trace.match(/^detach \/Volumes\/fixture$/gm)?.length, 1);
    } else {
      assert.match(result.trace, /detach \/dev\/disk4/);
    }
  });
}

for (const scenario of ["fail-convert", "missing-output", "empty-output"]) {
  test(`DMG packaging rejects ${scenario} instead of reporting create success`, async () => {
    const result = await runScenario(scenario);
    assert.equal(result.status, 1, result.stderr);
    if (scenario === "fail-convert") {
      assert.match(result.stderr, /failed to create DMG after 12 attempts/);
      assert.equal(result.trace.match(/^convert /gm)?.length, 12);
    } else {
      assert.match(result.stderr, /DMG output is missing or empty after conversion/);
      assert.equal(result.trace.match(/^convert /gm)?.length, 1);
    }
    assert.doesNotMatch(result.stdout, /out\/final\.dmg/);
  });
}

for (const scenario of ["busy", "info-error"]) {
  test(`DMG packaging refuses conversion when detach is ${scenario}`, async () => {
    const result = await runScenario(scenario);
    assert.equal(result.status, 1, result.stderr);
    assert.doesNotMatch(result.trace, /^convert /m);
    assert.match(result.stderr, /failed to detach DMG/);
  });
}
