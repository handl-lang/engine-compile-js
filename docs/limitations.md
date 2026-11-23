# Limitations

The honest edge list for `handl-plugin-compile-js`: what the backend
does not yet do, and how each gap fails. The design rule throughout is
the honesty ladder in
[docs/js-runtime-model.md](js-runtime-model.md) §6 — nothing here
silently miscompiles; every entry is a `CompileError`, a runtime throw,
or a documented subset.

## Effect system (`handle`/`perform`)

- **Surface `handle … with h acc s0` syntax is declined upstream.**
  `Term::Handle` carries the `acc` field and the JS lowering +
  prelude implement the full `(payload, s) ~> (result, s')`
  accumulator path (seed, `__acc__` slot, `acc_result_split`,
  carried restart) — but `handl-kit-arrow-lower` still declines the
  surface `acc` clause because the shared `Term` interpreter lacks
  the `(payload, s)` convention. Until that lands upstream, `.hnl`
  sources cannot spell `acc`; the e2e coverage injects the seed into
  the lowered `Term::Handle` directly (`tests/e2e.rs`,
  `e2e_handle_acc_*`).
- **Multi-shot `resume` is a runtime throw.** A handler impl with
  arity ≥ 2 suspends to its `handle` driver at eval; JS async arrows
  have no delimited continuation, so `__applyHandler` throws
  `handler impl is ≥2-ary: multi-shot 'resume' is not supported`
  (`src/runtime/prelude.rs:139-144`). Under `acc` the 2-ary shape *is*
  the `(payload, s)` convention and dispatches normally.
- **Serialized-`Term` handler impls are not interpreted.** Eval
  accepts an impl JSON that deserializes to a `Term`; on this target
  handler values are JS arrow functions / records only, so a
  `__term__`-style payload fails the "not an arrow value" check
  rather than silently mis-running.
- **`perform`/`io.perform` op spellings are compile-denied.** Eval's
  `perform_dispatch` needs the resume journal this subset does not
  carry (`src/lower/primitive.rs:702-712`). The runtime `perform`
  member remains for host/driver JS and the `IO(sig)` former
  post-pass inside `handle`.
- **Compound `dim` terms lower to `null`.** A `dim` the lowerer
  cannot resolve to a literal label arrives as the unnamed dimension
  — every handler is eligible (`src/runtime/prelude.rs:61-69`).
- **Impls apply to op args, not `incoming`.** `dispatch` feeds the
  evaluated argument array to the impl; eval's `dispatch_op` applies
  impls to the op arrow's *incoming* value
  (`src/runtime/prelude.rs:325-339`). Related subset gaps: a lone
  array argument is indistinguishable from a packed multi-arg tuple
  in `__opRef`'s handled-arrow path, and bound-arrow records compose
  under `>>>` where eval would apply them
  (`src/runtime/prelude.rs:526-528`).
- **`dispatch_all_ops` is not full parity.** It routes the pure
  arg-op family through `dispatch` so `handle` frames can intercept
  `add`/`lt`/etc., but ops that read the incoming value or carry
  `lit` payloads (`elem`, `field_access`, `record`, `json`,
  `proj_env`, …) keep dedicated emit and bypass handlers even under
  the flag (`src/config.rs:288-307`).

## Proof / rewrite vocabulary

- **`Term::Rewrite` (`~>`) is a `CompileError`** — eval returns
  `NotImplemented` for it too (`src/lower/proof.rs`).
- **`path`/`equiv`/`handle_dyn` op spellings and `Term::Opaque` are
  compile errors** (`src/lower/primitive.rs:696-712`,
  `src/lower/mod.rs:421,437`).

## Module surface

- **`commonjs` `require()` yields a Promise.** Module sources emit
  `module.exports = (async () => { …; return <namespace>; })()`, so
  `require()` of a compiled `.hnl` module gives a `Promise` of the
  export namespace, not the namespace itself
  (`src/config.rs:77-96`).
- **`iife`/`inline` cannot express ES `import`** — any surviving
  import is a compile error; resolve `.hnl` deps through
  `JsConfig::modules`/`entry_imports` so they inline
  (`src/module.rs:591-606`).
- **`worker_entry` import rules**: under `esm` only `node:`/`bun:`/
  `deno:` builtins and absolute URLs (`https://…`) may survive;
  relative/filesystem-absolute specifiers are compile errors
  (`src/compiler.rs:142-164`). `https:` imports are *kept* but
  untested offline — the browser e2e treats a network/CORS failure
  as SKIP (`tests/browser_e2e.mjs`, `tests/browser/worker_https.hnl`).

## Prelude coverage

- **`str`, `char`, `hash_map`, `obj`, `fetch-js` members are PARTIAL**
  — only the surface `.hnl` sources actually call is mirrored
  (`src/runtime/prelude.rs`, per-member comments). Missing members
  surface as `undefined`/throw at call time, not at compile time.
- **`assert`/`runtime_assert` throw at runtime** — they are
  AST-level assertions that never reach the lowered `Term`, matching
  `term_interpreter`'s "not supported" stance
  (`src/runtime/prelude.rs:559-567`).

## Verification gaps

- **Bench has no eval baseline.** `bench/` compares JS variants
  against each other (JS-vs-JS); no `hnl` eval extension is built in
  this harness, so absolute parity-vs-eval performance is unmeasured
  (`docs/strategy-evaluation.md`).
- **`op_lowering: Prelude` and `unsafe_inline_foreign` are
  experimental** — kept configurable pending measurements; defaults
  (`Mixed`, safe inlining) are unchanged per the T-017 data.
- **Cross-backend parity is unverified.** This crate's tests cover
  eval-parity shapes and node/Chromium execution only; the sibling
  `engine-compile-*` backends (e.g. the inkwell-gated LLVM arm) are
  outside this test surface.

## Upstream divergences

Fixed during this work (kept for the record): `>>>` flow composition
(T-019), `export data` surface form end-to-end (T-009/T-020), and the
`Term::Handle` `acc` field adoption across kernel/host/backends
(T-021–T-027).

Still open upstream:

- **`handle … acc` surface syntax** — declined in
  `handl-kit-arrow-lower` pending a shared-`Term`-interpreter
  convention for `(payload, s)` impls (see above).
- **Loader RHS-inlining divergence** — `lib-loader`'s
  `src/index.hnl` lacked `export data` handling (bootstrap fallback
  covers it) and `loader.rs`'s bootstrap mirror diverges; noted by
  T-023 for upstream parity follow-up.
