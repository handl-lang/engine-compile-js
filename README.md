# tool-compile-js

`handl-plugin-compile-js` — JavaScript compiler for the Handl arrow
calculus. Part of the `tool-compile-*` compiler-backend family: lowers Handl
`Term` arrow terms to JavaScript source.

## Overview

The pipeline is parse → desugar → lower → emit → prelude: `.hnl` module
sources are split into statement chunks, desugared (`handl_runtime`'s
`apply_desugar`), lowered from `Term` to a JS AST (`src/lower/*`), emitted
as source (`src/emit/*`, with `src/module.rs` handling the
`import`/`export`/`const` surface), and prepended with the `__handl_rt`
runtime prelude (`src/runtime/prelude.rs`).

The runtime layer exists because Handl semantics don't map onto plain JS:
arrows are uniform single-input `async` functions, Handl data encodes as
`postMessage`-safe marker records, and effects dispatch through a
handler stack JS has no primitive for. See
[docs/js-runtime-model.md](docs/js-runtime-model.md) for the full
rationale — including why CPS/direct-transpile were rejected — and
[docs/limitations.md](docs/limitations.md) for the honest edge list.

## Capability matrix

Statuses: **e2e** = exercised under real `node` in `tests/e2e.rs`;
**unit** = covered in `src/tests.rs` / `tests/security.rs`; **throw** =
fails loudly at compile time or runtime rather than miscompiling.

| Feature | Status | Where |
|---|---|---|
| Module compile: `esm`, `commonjs`, `iife`, `inline` | e2e | `src/module.rs::emit_module_surface` |
| `.hnl` dep inlining via `JsConfig::modules`/`entry_imports` | e2e | `src/config.rs` |
| ES `import` surface: named, `{a as b}` aliases, `* as ns`, side-effect | e2e | `src/module.rs` |
| `export` decls → real ESM named exports / CJS-IIFE / inline namespace object | e2e | `src/module.rs` |
| Signature exports (`export f: T ~> U`) → `export let f = null` | e2e | `src/module.rs` |
| `F := new Inductive {…}` family decls (incl. `F := {…}` / `F :: …` spellings), parameterized `Opt<a>`, ctors + `match` | e2e | `src/module.rs`, `src/runtime/prelude.rs` (`ctor_name`/`ctor_fields`) |
| `export F := new Inductive {…}` — family exported; ctors ride the family record (`F.Ctor`), cross-module ctor literals | e2e | `src/module.rs` |
| family decls nested in `do` blocks (eval-scoped) | e2e | `splice_decl_blocks` |
| Ops: full `__handl_rt` prelude set (record/flow/ctor/match-guard/eq/json/`tag`/`bytes`/`math`/`str`/`char`/`option`/`type`/`hash_map`/`obj`/`fetch-js`/`result` + `js` contract `call`/`invoke`/`construct`/`on`/`off`/`await`/`error`) | e2e | `src/runtime/prelude.rs` |
| Foreign ops: inline `globalThis[m][n](…)` or `jsCall` dispatch; handler interception via `dispatchForeign` | e2e + unit | `src/lower/primitive.rs`, `src/security.rs` |
| `handle`/`perform` effect subset: op dispatch, grades/`dim`, carried-handler reinstall, `return` clause, `IO(sig)` former interpretation | e2e | `src/runtime/prelude.rs` §4 of runtime-model doc |
| `handle … acc` accumulators (`Term::Handle{acc}`: `(payload, s) ~> (result, s')` threading, pair result, carried restart) | e2e | `src/lower/higher.rs`, `src/runtime/prelude.rs` |
| `perform`/`io.perform` spellings (flat, qualified, `from "io" import perform`, foreign `io`) → `__handl_rt.perform`: covered ops dispatch in-flight incl. `acc` state threading; uncovered ops mint the residual `type.io` former `handle`'s `__interpretIO` interprets (`perform(sym(…))` throws `unsupported op: sym` — `sym` is not in the prelude) | e2e | `src/lower/primitive.rs::lower_perform_call`, `src/lower/control.rs`, `src/runtime/prelude.rs` §4; `tests/e2e.rs::e2e_perform_*` |
| `worker_entry` bundles: `esm` module workers, `iife`/`inline` classic workers, `js.on("self","message")` round-trip + `postMessage` transfer lists | e2e (node `worker_threads` **and** real Chromium via Playwright) | `src/compiler.rs`, `tests/e2e.rs`, `tests/browser_e2e.mjs` |
| Source maps (`JsConfig::source_map`) | e2e | `src/emit/source_map.rs` |
| `SecurityScanner` post-emit pass (`security_scan`, default on) | unit + e2e | `src/security.rs`, `src/emit/mod.rs` |
| `OpLowering::{Mixed, Prelude}`, `dispatch_all_ops`, `loop_max_iterations`, `no_inline_foreign`, `unsafe_inline_foreign` | e2e + unit | `src/config.rs`, `src/lower/*` |
| Fuzz/property parity vs eval (seeded arithmetic/record pipelines) | e2e | `tests/e2e.rs::e2e_fuzz_*` |
| `Term::Rewrite` (`~>`) | compile error | `src/lower/proof.rs` |
| `path`/`equiv`/`handle_dyn` op spellings, `Term::Opaque` | compile error | `src/lower/primitive.rs`, `src/lower/mod.rs` |
| Multi-shot `resume` (≥2-ary impls), serialized-`Term` impls | runtime throw | `src/runtime/prelude.rs::__applyHandler` |
| `assert` / `runtime_assert` (AST-level, never reach `Term`) | runtime throw (eval parity) | `src/runtime/prelude.rs` |
| Surface `handle … with h acc s0` syntax | declined upstream (arrow-lower) | see `docs/limitations.md` |

## Worker entries

`JsConfig::worker_entry = true` compiles a `.hnl` module source into a
self-contained bundle: runtime prelude + inlined `.hnl` dep graph, no
filesystem resolution at run time. The worker's message wiring lives in
the `.hnl` source — the `js/worker` convention: register
`js.on("self", "message", arrow)` (browser workers) or
`js.on(port, "message", arrow)` over a concrete event source (Node's
`parentPort`, or the parent-side `worker_threads` `Worker` — an
EventEmitter). `js.on` feature-detects `addEventListener` vs
`on`/`off`: EventTargets unwrap `MessageEvent.data`, emitters take the
emit argument verbatim; it then awaits the arrow and posts a non-null
result back automatically.

Constraints (compile-time): format must be `esm` (module worker —
`new Worker(url, { type: "module" })`), `iife`, or `inline` (classic
worker script — `new Worker(url)`, `importScripts`, Blob URL);
`commonjs` is rejected. Under `esm`, surviving ES imports must be
builtin schemes (`node:`/`bun:`/`deno:`) or absolute URLs
(`https://…`); under `iife`/`inline` every ES `import` is a compile
error. Resolve relative `.hnl` imports through `JsConfig::modules` /
`entry_imports` so they inline.

Minimal worker source (`worker.hnl`):

```hnl
from "js" import { on, call, construct }

ab := construct("", "ArrayBuffer", [16], [], "js_object", false)

on("self", "message", (msg) ~> do {
  call("self", "postMessage", [{ echoed: msg.n, buf: ab }, [ab]], [], "void", false)
  remaining := call("Reflect", "get", [ab, "byteLength"], [], "js_number", false)
  { echoed: msg.n, remaining: remaining }
})
```

Compile and drive under Node (`worker.mjs` = compiled bundle):

```js
import { Worker } from "node:worker_threads";
const worker = new Worker(new URL("./worker.mjs", import.meta.url));
worker.on("message", (m) => console.log(m));   // { echoed: 21, remaining: 0 }
worker.postMessage({ n: 21 });
```

`remaining: 0` proves the `ArrayBuffer` was *transferred* (detached in
the worker), not cloned. The same fixture runs in real Chromium —
`tests/browser_e2e.mjs` compiles `tests/browser/*.hnl` via
`cargo run --example emit_worker` and loads the bundles as module Web
Workers through Playwright (request interception stands in for an HTTP
server). It is an optional stage: `node tests/browser_e2e.mjs`
(`--strict` fails on missing playwright/browsers instead of skipping);
missing toolchain pieces print `SKIP` and exit 0.

See `tests/e2e.rs::e2e_worker_entry_node_worker_roundtrip_and_transfer`
for the `node:worker_threads` spelling (`parentPort`) and
`tests/browser/worker_echo.hnl` for the `self` spelling.

## Configuration

`JsConfig` (`src/config.rs`) — defaults shown; `JsConfig::esm_deno()` /
`cjs_node()` are the common starting points.

| Field | Type | Default | Notes |
|---|---|---|---|
| `target` | `Box<dyn JsTarget>` | `Deno` / `Node` | `Deno`, `Node`, `Bun`, `Browser` |
| `format` | `Box<dyn JsOutputFormat>` | `Esm` / `CommonJs` | `Esm`, `CommonJs`, `Iife`, `Inline` |
| `debug` | `bool` | `false` | forwarded to `CompilerConfig` |
| `no_inline_foreign` | `bool` | `false` | all foreign calls go through `__handl_rt.jsCall` |
| `unsafe_inline_foreign` | `bool` | `false` | experimental: bypass the inline safety check (doc-hidden) |
| `source_map` | `bool` | `false` | emits `main.js.map` artifact + `sourceMappingURL` |
| `modules` | `Vec<JsModule>` | `[]` | `.hnl` deps to inline; each emits once as `__mod_<n>` |
| `entry_imports` | `Vec<(String, String)>` | `[]` | resolved import edges for `CompilerInput::source` |
| `worker_entry` | `bool` | `false` | self-contained worker bundle; see above |
| `op_lowering` | `OpLowering` | `Mixed` | `Mixed` = raw JS ops for the scalar family; `Prelude` = all scalar ops through `__handl_rt` members (experimental, pending bench) |
| `loop_max_iterations` | `u64` | `100_000` | `Term::Loop` feedback bound (`__cnt < N`) |
| `security_scan` | `bool` | `true` | `SecurityScanner` rejects `eval(`/`Function(` patterns in emitted JS |
| `dispatch_all_ops` | `bool` | `false` | route the pure arg-op family through `dispatch` so `handle` frames can intercept them (slower; still not full parity) |

## Testing & benchmarks

```bash
cargo test --lib                 # 95 unit tests (src/tests.rs)
cargo test --test e2e            # 43 node e2e tests (tests/e2e.rs; skips without node)
cargo test --test security       # 12 foreign-descriptor/inlining tests
cargo test --test bench_fixtures # bench fixture compile checks
cargo run --example emit_module -- file.hnl   # compile a module to JS
cargo run --example emit_worker -- file.hnl   # compile a worker_entry bundle
cargo run --example bench -- --quick          # strategy bench (needs node)
node tests/browser_e2e.mjs                    # real-Chromium worker e2e (optional)
bash bench/run.sh                             # full bench → bench/results.json
cargo run --manifest-path xtask/Cargo.toml -- gate   # fmt → clippy → test → doc
```

`bench/` measures emitted-JS wall time / RSS / size / compile time /
startup across `{esm, commonjs, iife, inline} × {mixed, prelude}`;
results and the keep-defaults analysis live in
[docs/strategy-evaluation.md](docs/strategy-evaluation.md).

## Docs

- [docs/js-runtime-model.md](docs/js-runtime-model.md) — why the
  `__handl_rt` prelude exists (async arrows, marker records, handler
  stack, promise boxing) and the honesty ladder.
- [docs/limitations.md](docs/limitations.md) — what the backend does
  not yet do, and why each gap fails loudly.
- [docs/strategy-evaluation.md](docs/strategy-evaluation.md) — P-002/T-017
  empirical evaluation of `OpLowering` and module formats.

## Development

`xtask/` provides the gate tasks (`fmt`, `clippy`, `test`, `doc`, `check`,
`gate`); `workflows/gate.yaml` is the bupha/peregrin gate entry. Deps
are git deps on `branch = "main"`; locally the workspace-root
`.cargo/config.toml` patches them to sibling checkouts (dev-only, never
committed).

## License

MIT.
