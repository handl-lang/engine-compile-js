# tool-compile-js

`handl-plugin-compile-js` — JavaScript compiler for the Handl arrow
calculus (ADR-0114 Phase 3). Part of the `tool-compile-*`
compiler-backend family: lowers Handl `Term` arrow terms to JavaScript
source. Rust 2024, `publish = false` (git-install only).

## Layout

| path | role |
| --- | --- |
| `src/compiler.rs` | `JsCompiler` — `compile`/`materialize`, `JsIr` (`compile-js` IR kind), artifact emission |
| `src/config.rs` | `JsConfig` — target (`Deno`/`Node`/`Bun`/`Browser`), format (`Esm`/`CommonJs`/`Iife`/`Inline`), `modules`/`entry_imports`, `worker_entry`, `OpLowering`, `security_scan`, `dispatch_all_ops`, `loop_max_iterations` |
| `src/lower/` | `Term` → JS AST — `mod.rs` dispatch, `control.rs` (Seq/First/Choice/Loop/Iter/Handle), `higher.rs`, `primitive.rs` (ops + foreign calls + `perform`), `proof.rs` |
| `src/emit/` | JS AST → source (`mod.rs`) + `source_map.rs` |
| `src/module.rs` | module surface — `import`/`export`/`const`, `data` decls + `match`, `emit_module_surface`, `emit_export_data_block` |
| `src/runtime/prelude.rs` | `__handl_rt` runtime prelude — async arrows, marker records, handler stack, foreign dispatch |
| `src/security.rs` | `SecurityScanner` post-emit pass, `ForeignDescriptor`/`is_safe_for_inlining` |
| `src/output.rs` | `JsOutput` artifacts |
| `src/tests.rs` | unit tests (`#[cfg(test)] mod tests` in lib) |
| `tests/e2e.rs` | node e2e — executes emitted JS under `node` |
| `tests/browser_e2e.mjs`, `tests/browser/` | real-Chromium worker e2e via Playwright (optional) |
| `bench/`, `examples/` | strategy-evaluation harness + `emit_module`/`emit_worker`/`dump_ast` examples |
| `xtask/` | repo gate tasks (`fmt`, `clippy`, `test`, `doc`, `gate`) — separate workspace |

## Contract

- **Input**: `.hnl` module sources via `JsCompiler::compile` →
  `JsOutput` artifacts (`main.js`, optional `main.js.map`). Pipeline:
  parse → `handl_runtime::desugar_pass::apply_desugar` → `Term` →
  lower → emit → prepend the `__handl_rt` prelude.
- **Runtime model**: arrows are uniform single-input `async`
  functions; Handl data encodes as `postMessage`-safe marker records;
  effects dispatch through a handler stack JS has no primitive for.
  See `docs/js-runtime-model.md`.
- **Refusals are named**: compile-time `CompileError`s or runtime
  throws — never silent miscompiles (the "honesty ladder",
  `docs/js-runtime-model.md` §6; edge list in `docs/limitations.md`).
- **`worker_entry`**: self-contained worker bundles (`esm`/`iife`/
  `inline` only — `commonjs` rejected); message wiring via the
  `js/worker` convention (`js.on("self"|port, "message", arrow)`).

## Conventions

- Git deps on `branch = "main"`; **no `[patch]` tables** in the
  manifest — local sibling overrides live in the gitignored
  workspace-root `.cargo/config.toml`.
- Edition 2024, `#![forbid(unsafe_code)]`, `#![warn(missing_docs)]`,
  `publish = false`.
- `tests/e2e.rs` needs `node` on PATH (the gate fails rather than
  silently skips); `node tests/browser_e2e.mjs` is the optional
  Playwright stage (`--strict` fails instead of skipping).
- English everywhere.

## Verification

```bash
cargo xtask gate        # fmt --check → clippy -D warnings → test → doc
cargo test --lib        # unit tests (src/tests.rs)
cargo test --test e2e   # node e2e (needs node on PATH)
node tests/browser_e2e.mjs   # optional real-Chromium worker e2e
cargo run --manifest-path xtask/Cargo.toml -- gate   # same gate, explicit
```

The bupha runner is the peregrin campaign in `workflows/gate.yaml`
(see `bupha.toml`) — there is no `ci.sh`.

<!-- myco:agent-awareness -->

## Agent awareness

- Session start / any work discussion: run `myco status --prompt`
  before proposing next steps — know the board first.
- User describes new work → offer to capture it (`myco todo add`,
  `issue new`, `proposal new`). Don't wait to be asked.
- Picking up a task: `myco resume <ID>` is the briefing.
- Wrapping up: `myco handoff <ID>` anything resumable; keep statuses
  honest (claimed → in-progress → blocked/done).

<!-- /myco:agent-awareness -->
