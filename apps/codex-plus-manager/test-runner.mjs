import { readdir } from "node:fs/promises";
import { createRequire } from "node:module";
import { dirname, join } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
import { spawn } from "node:child_process";

const packageDir = dirname(fileURLToPath(import.meta.url));
const srcDir = join(packageDir, "src");
const testFiles = (await readdir(srcDir, { withFileTypes: true }))
  .filter((entry) => entry.isFile() && entry.name.endsWith(".test.ts"))
  // The child cwd is fixed below, so relative paths avoid Node 20's Windows
  // handling of drive-letter and file:// arguments while remaining caller
  // independent.
  .map((entry) => join("src", entry.name))
  .sort();

if (testFiles.length === 0) {
  throw new Error("No frontend TypeScript test files were found under src/");
}

// Resolve the loader before starting Node's test worker pool. When `tsx` is
// missing, Node otherwise reports the same module-resolution error once per
// test file, which obscures the actual environment problem and can look like
// 23 independent assertion failures.
const require = createRequire(import.meta.url);
let tsxLoader;
try {
  tsxLoader = require.resolve("tsx");
} catch (error) {
  const detail = error instanceof Error ? error.message : String(error);
  console.error(
    `[frontend-test] Missing locked dev dependency \"tsx@4.19.3\" in ${packageDir}.`,
  );
  console.error(
    `[frontend-test] Install dependencies with \"npm ci --no-audit --no-fund\" and retry \"npm test\".`,
  );
  console.error(`[frontend-test] No test files were executed (${testFiles.length} discovered).`);
  console.error(`[frontend-test] Resolver detail: ${detail}`);
}

if (!tsxLoader) {
  process.exit(1);
}

// Keep the runner deterministic on constrained CI/desktop environments. The
// suite contains renderer and packaging tests that can each allocate a large
// temporary fixture tree. Node's test worker pool can still exhaust Windows'
// passwd/resource handle budget when all files are passed to one invocation,
// even with --test-concurrency=1. Run one file per child and aggregate the
// exit codes so the environment reports one actionable failure per file.
const runTestFile = (testFile) =>
  new Promise((resolve) => {
    const child = spawn(
      process.execPath,
      ["--import", pathToFileURL(tsxLoader).href, "--test", "--test-concurrency=1", testFile],
      {
        cwd: packageDir,
        stdio: "inherit",
        shell: false,
      },
    );

    child.once("error", (error) => {
      console.error(`[frontend-test] ${testFile}: ${error}`);
      resolve(1);
    });

    child.once("exit", (code, signal) => {
      resolve(signal ? 1 : code ?? 1);
    });
  });

const failedFiles = [];
for (const testFile of testFiles) {
  const code = await runTestFile(testFile);
  if (code !== 0) failedFiles.push(testFile);
}

if (failedFiles.length > 0) {
  console.error(`[frontend-test] ${failedFiles.length} test file(s) failed:`);
  for (const testFile of failedFiles) console.error(`  - ${testFile}`);
  process.exitCode = 1;
}
