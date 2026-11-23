# engine-compile-js

`handl-plugin-compile-js` — JavaScript compiler for the Handl arrow
calculus. Part of the `engine-compile-*` compiler-backend family: lowers Handl
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
rationale — including why CPS/direct-transpile were rejected.

Output formats: `esm`, `commonjs`, `iife`, `inline`; targets: node, bun,
deno, browser. `JsConfig::worker_entry` produces self-contained worker
bundles (`esm` module workers or `iife`/`inline` classic workers) with
ES imports restricted to resolvable specifiers.

`tests/e2e.rs` is the proof harness: it compiles `.hnl` fixtures through
the public API and executes the emitted JS under real `node`, asserting
on stdout/exit status.

## Development

```bash
cargo run --manifest-path xtask/Cargo.toml -- gate   # fmt → clippy → test → doc
```

`xtask/` provides the gate tasks (`fmt`, `clippy`, `test`, `doc`, `check`,
`gate`); `ci.sh` is the same gate entry for the bupha shell runner. Deps
are git deps on `branch = "main"`; locally the workspace-root
`.cargo/config.toml` patches them to sibling checkouts (dev-only, never
committed).

## License

MIT.
