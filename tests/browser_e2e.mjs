#!/usr/bin/env node
// T-014: real-browser e2e for `worker_entry` bundles via Playwright.
//
//   node tests/browser_e2e.mjs [--strict]
//
// What it does:
//   1. builds `examples/emit_worker` (cargo, offline) and compiles the
//      `.hnl` fixtures under tests/browser/ into self-contained ESM
//      worker bundles (`JsConfig::worker_entry`);
//   2. launches Chromium through the `playwright` Node package;
//   3. in a real page (fake https origin via request interception —
//      Chromium rejects module workers from blob: URLs), loads each
//      bundle as a **module Web Worker** — no HTTP server, no node
//      shims: `self` is the genuine `DedicatedWorkerGlobalScope`;
//   4. asserts the round-trip + ArrayBuffer transfer-list detach
//      (worker_echo.hnl) and the `https://` ES-import smoke
//      (worker_https.hnl — SKIP-tolerant when the network or CORS
//      blocks the fetch).
//
// Skip behavior: missing node modules/playwright, missing browser
// binaries, or a missing cargo toolchain print `SKIP` and exit 0 —
// this stage is optional and must never break the default gate.
// `--strict` (or BROWSER_E2E_STRICT=1) turns skips into failures.
// A compile error or an assertion failure is always a failure.
//
// Playwright resolution order: $PLAYWRIGHT_NODE_PATH, `require` from
// this repo, then well-known global prefixes (/usr/lib/node_modules,
// /usr/local/lib/node_modules). Browser resolution: playwright's own
// registry first; on "executable doesn't exist" we scan
// $PLAYWRIGHT_BROWSERS_PATH (default ~/.cache/ms-playwright) for the
// newest chromium / chromium_headless_shell build and pass it as
// `executablePath` — this lets a playwright npm package drive a
// browser cache populated by a different playwright version (e.g.
// python playwright's chromium-1248 vs npm 1.56's expected -1194).

import { spawnSync } from "node:child_process";
import { existsSync, readdirSync } from "node:fs";
import { createRequire } from "node:module";
import { homedir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const STRICT =
  process.argv.includes("--strict") || process.env.BROWSER_E2E_STRICT === "1";
const TIMEOUT_MS = 15_000;

let skipped = 0;
function skip(reason) {
  skipped += 1;
  console.log(`browser-e2e: SKIP — ${reason}`);
  if (STRICT) {
    console.error("browser-e2e: --strict — skip is a failure");
    process.exit(1);
  }
}
function fail(msg) {
  console.error(`browser-e2e: FAIL — ${msg}`);
  process.exit(1);
}
function pass(name, detail = "") {
  console.log(`browser-e2e: PASS — ${name}${detail ? ` (${detail})` : ""}`);
}

// ------------------------------------------------------------------
// 1. toolchain probes
// ------------------------------------------------------------------

const cargo = spawnSync("cargo", ["--version"], { encoding: "utf8" });
if (cargo.error || cargo.status !== 0) {
  skip("cargo not on PATH — cannot build examples/emit_worker");
  process.exit(0);
}

const EMITTER = join(ROOT, "target", "debug", "examples", "emit_worker");
const build = spawnSync(
  "cargo",
  ["build", "--offline", "--example", "emit_worker"],
  { cwd: ROOT, encoding: "utf8" },
);
if (build.status !== 0) {
  // A stale-but-usable binary beats a hard fail: sibling dep repos are
  // edited concurrently and `cargo build` can break transiently while
  // the previously built emitter still runs.
  if (!existsSync(EMITTER)) {
    fail(
      `cargo build --example emit_worker failed and no emitter binary exists:\n${(build.stderr || build.stdout || "").slice(-2000)}`,
    );
  }
  console.log(
    "browser-e2e: WARN — cargo build failed; using existing emitter binary",
  );
}

function compileWorker(fixture) {
  const rel = join("tests", "browser", fixture);
  const r = spawnSync(EMITTER, [join(ROOT, rel)], {
    cwd: ROOT,
    encoding: "utf8",
    maxBuffer: 64 * 1024 * 1024,
  });
  if (r.status !== 0) {
    fail(`emit_worker ${fixture} failed:\n${(r.stderr || "").slice(-2000)}`);
  }
  return r.stdout;
}

const echoBundle = compileWorker("worker_echo.hnl");
const httpsBundle = compileWorker("worker_https.hnl");
pass("compile", "worker_echo.hnl + worker_https.hnl → ESM worker bundles");

// ------------------------------------------------------------------
// 2. playwright + browser resolution
// ------------------------------------------------------------------

const require = createRequire(import.meta.url);
function loadPlaywright() {
  const candidates = [
    process.env.PLAYWRIGHT_NODE_PATH,
    "playwright",
    "/usr/lib/node_modules/playwright",
    "/usr/local/lib/node_modules/playwright",
    join(homedir(), ".npm-global/lib/node_modules/playwright"),
  ].filter(Boolean);
  for (const c of candidates) {
    try {
      return require(c);
    } catch {
      /* try next */
    }
  }
  return null;
}
const pw = loadPlaywright();
if (!pw || !pw.chromium) {
  skip("playwright Node package not resolvable (set PLAYWRIGHT_NODE_PATH)");
  process.exit(0);
}

const browsersRoot =
  process.env.PLAYWRIGHT_BROWSERS_PATH || join(homedir(), ".cache", "ms-playwright");

/** Newest chromium-ish executable under the browsers cache, or null. */
function findBrowserExecutable() {
  if (!existsSync(browsersRoot)) return null;
  const builds = [];
  for (const entry of readdirSync(browsersRoot)) {
    const m = /^(chromium(?:_headless_shell)?)-(\d+)$/.exec(entry);
    if (!m) continue;
    const dir = join(browsersRoot, entry);
    const candidates = [
      join(dir, "chrome-headless-shell-linux64", "chrome-headless-shell"),
      join(dir, "chrome-linux", "chrome"),
      join(dir, "chrome-linux64", "chrome"),
    ];
    for (const exe of candidates) {
      if (existsSync(exe)) builds.push({ rev: Number(m[2]), exe });
    }
  }
  builds.sort((a, b) => b.rev - a.rev);
  return builds[0]?.exe ?? null;
}

async function launch() {
  try {
    return await pw.chromium.launch({ args: ["--no-sandbox"] });
  } catch (e) {
    const exe = findBrowserExecutable();
    if (!exe) throw e;
    console.log(`browser-e2e: playwright default launch failed (${String(e.message).split("\n")[0]}); using ${exe}`);
    return await pw.chromium.launch({ executablePath: exe, args: ["--no-sandbox"] });
  }
}

// Chromium refuses `new Worker(blobUrl, {type:"module"})` (the module
// graph has no base URL to resolve against and the load fails before
// any listener runs), so the bundles are served over a fake https
// origin via request interception — same-origin module workers, real
// `DedicatedWorkerGlobalScope`, no HTTP server needed.
const ORIGIN = "https://worker-e2e.test";

let browser;
try {
  browser = await launch();
} catch (e) {
  skip(`no usable chromium (playwright launch failed: ${String(e.message).split("\n")[0]})`);
  process.exit(0);
}
pass("browser", "chromium launched via playwright");

const context = await browser.newContext();
const page = await context.newPage();
await page.route("**/*", (route) => {
  const { pathname } = new URL(route.request().url());
  if (pathname === "/worker_echo.mjs") {
    return route.fulfill({
      status: 200,
      contentType: "text/javascript",
      body: echoBundle,
    });
  }
  if (pathname === "/worker_https.mjs") {
    return route.fulfill({
      status: 200,
      contentType: "text/javascript",
      body: httpsBundle,
    });
  }
  // The https fixture's `import "https://esm.sh/tslib"` is a real
  // network fetch — let it through; only same-origin paths are stubbed.
  if (pathname.startsWith("/") && !route.request().url().startsWith(ORIGIN)) {
    return route.continue();
  }
  return route.fulfill({
    status: 200,
    contentType: "text/html",
    body: "<!doctype html><title>browser-e2e</title>",
  });
});
await page.goto(`${ORIGIN}/`);

// Runs entirely inside the real page: same-origin module Worker →
// postMessage → collect replies. `expectCount` is the number of
// messages to await; the returned value is the array of `event.data`s.
async function runWorkerInPage(path, message, expectCount) {
  return page.evaluate(
    async ({ path, message, expectCount, timeoutMs, origin }) => {
      const worker = new Worker(`${origin}${path}`, { type: "module" });
      try {
        return await new Promise((resolve, reject) => {
          const acc = [];
          const timer = setTimeout(
            () => reject(new Error(`worker reply timeout (${timeoutMs}ms)`)),
            timeoutMs,
          );
          worker.onmessage = (e) => {
            acc.push(e.data);
            if (acc.length === expectCount) {
              clearTimeout(timer);
              // `page.evaluate` cannot serialize ArrayBuffer — fold
              // each into {__arrayBuffer, bytes} so the transfer-list
              // assertions can run node-side.
              const sanitize = (v) => {
                if (v instanceof ArrayBuffer) {
                  return {
                    __arrayBuffer: v.byteLength,
                    bytes: Array.from(new Uint8Array(v)),
                  };
                }
                if (Array.isArray(v)) return v.map(sanitize);
                if (v && typeof v === "object") {
                  return Object.fromEntries(
                    Object.entries(v).map(([k, x]) => [k, sanitize(x)]),
                  );
                }
                return v;
              };
              resolve(acc.map(sanitize));
            }
          };
          worker.onerror = (e) => {
            clearTimeout(timer);
            reject(
              new Error(
                `worker error: ${e.message || "unknown"} (${e.filename || ""}:${e.lineno || 0})`,
              ),
            );
          };
          worker.postMessage(message);
        });
      } finally {
        worker.terminate();
      }
    },
    { path, message, expectCount, timeoutMs: TIMEOUT_MS, origin: ORIGIN },
  );
}

// ------------------------------------------------------------------
// 4. assertions
// ------------------------------------------------------------------

try {
  // --- round-trip + transfer list ---------------------------------
  const msgs = await runWorkerInPage("/worker_echo.mjs", { n: 21 }, 2);
  const [explicit, reply] = msgs;

  if (
    !explicit.buf ||
    explicit.buf.__arrayBuffer !== 16 ||
    !Array.isArray(explicit.buf.bytes)
  ) {
    fail(`transfer failed — buf did not arrive as a 16-byte ArrayBuffer: ${JSON.stringify(msgs)}`);
  }
  if (!explicit.buf.bytes.every((b) => b === 7)) {
    fail(`transfer bytes wrong — expected sixteen 0x07 bytes, got ${explicit.buf.bytes}`);
  }
  if (explicit.echoed !== 21 || reply.echoed !== 21) {
    fail(`bad echo round-trip: ${JSON.stringify(msgs)}`);
  }
  if (reply.remaining !== 0) {
    fail(`transfer did not detach in worker — byteLength after transfer is ${reply.remaining}, expected 0`);
  }
  pass(
    "worker round-trip + transfer",
    "explicit postMessage + auto-reply, ArrayBuffer detached (byteLength 0), 16×0x07 bytes intact",
  );

  // --- https: import smoke (skip-tolerant) --------------------------
  try {
    const httpsMsgs = await runWorkerInPage("/worker_https.mjs", { n: 5 }, 1);
    const m = httpsMsgs[0];
    if (m && m.pong === "https-ok" && m.echo === 5) {
      pass("https import", "module worker loaded https://esm.sh/tslib and answered");
    } else {
      fail(`https worker replied unexpectedly: ${JSON.stringify(httpsMsgs)}`);
    }
  } catch (e) {
    const reason = String(e && e.message ? e.message : e);
    // Network/CORS/module-load failures are environmental — the
    // worker_entry contract (keep https: specifiers) is already
    // covered by the compile step; only a *reply-shape* failure above
    // is a hard fail.
    skip(`https import smoke could not run (${reason})`);
  }
} finally {
  await browser.close();
}

console.log(
  `browser-e2e: done — ${skipped} skip(s), 0 failure(s)`,
);
