+++
id = "P-001"
title = "Effect-semantics parity: handle/rewrite/path/equiv stubs vs eval"
status = "open"
created = "2026-11-23"
owner = "devin"
priority = "high"
links = ["T-006"]
epic = "compile-js-prod"
+++

# Effect-semantics parity: `handle` / `rewrite` / `path` / `equiv`

Tracking: T-006 (compile-js-prod P5). Design only — no runtime changes in
this proposal.

## 1. Current state

### What compile-js emits today

`src/runtime/prelude.rs:35-38` — four stubs on `__handl_rt`:

```js
handle:   (body, target, impl) => (input) => (typeof impl === "function" ? impl(input) : impl),
rewrite:  (source, target, witness) => source,
path:     (from, to, witness) => from,
equiv:    (a, b, path) => a,
```

- `handle` **never runs `body`**. It returns an arrow that applies `impl`
  to the input (or returns `impl` itself when it isn't callable). For the
  canonical `handle (arr HttpGet)(url) with {HttpGet: h}` shape the
  result is the handler *record*, not the handled computation — silently
  wrong, not just incomplete.
- `rewrite` ignores `target` and `witness`, returns `source`.
- `path` returns `from`; `equiv` returns `a` (not even an equality
  check).

Lowering sites:

- `src/lower/higher.rs:78-96` — `Term::Handle{body,target,impl_}` lowers
  `body` and `impl_` to JS arrows and emits
  `await __handl_rt.handle(bodyFn, "target", implFn)(input)`.
- `src/lower/proof.rs:79-95` — `Term::Rewrite{source,target,witness}`
  emits `await __handl_rt.rewrite(sourceFn, targetFn, "witness")(input)`.
  `proof.rs:1-8` documents that `Path`/`Equiv`/`Refine`/`CellId`/`JElim`/
  `Whisker` are *derived* operators folded into `Term::Opaque` (ADR-0119
  §7) — **no `Term` variant reaches this lowerer for them**. The
  `path`/`equiv` prelude members are reachable only through the generic
  `Op{name}` fallback in `src/lower/primitive.rs:521-553`
  (`__handl_rt.<name>(args)` for ops with args, `__opRef(name)` for
  0-arg references).

### What eval actually does

Three different "eval" surfaces exist; they disagree with each other and
with the stubs:

**`Term::Handle` — tree-walk engine** (`engine-exec-tree-walk/src/lib.rs:1406-1541`):

1. Evaluate `impl_` to a *handler value* (a record `{op: impl}` /
   `{impls: {op: impl}}`, or a plain arrow for single-op handlers).
2. Optional `acc` child seeds a `__acc__` slot on the frame
   (`native_ops.rs:3584-3591`).
3. ≥2-ary clauses take a **multi-shot resumption** path: a replay journal
   (`ResumeJournal`) suspends covered `perform`s/ops and re-runs the body
   per `resume` (`lib.rs:1437-1474`). Requires delimited continuations —
   not expressible over JS async functions.
4. Otherwise push the frame on `handler_stack`, eval `body`, then
   `interpret_io_former` (`native_ops.rs:3876-3912`): an `IO(sig)` former
   result has each sig field dispatched through the covering handler and
   the entry replaced by the handler's result; uncovered ops stay data.
5. `return` clause postlude: `h["return"](r)` (or `(r, s_final)` under
   `acc`) applies after the pop; without `return`, `acc` handles yield
   the `(result, s_final)` pair (`lib.rs:1497-1531`,
   `native_ops.rs:3995-4009`).
6. `attach_carried_handler` (`native_ops.rs:3500-3538`): `arr <op>`
   dispatch objects and closures produced under `handle` carry the
   handler so later applications reinstall it — `handle` binds a
   *reusable* handled arrow, not a one-shot.
7. Fallback: when `impl_` doesn't evaluate to a value (pure arrow
   scheme), compile-time `Term::eliminate_handlers`
   (`kernel/.../term.rs:803`, `substitute_capability` at :922) rewrites
   bare `arr(target)` refs in the body; unhandled capabilities are a
   compile-time error.

**`Term::Handle` — `handle_dyn` op** (`engine-exec/src/native_ops.rs:3930-3969`,
shared shell `run_delimited_default` :3982-4014): same semantics for
compiled backends that emit `Op{name:"handle_dyn"}` instead of
`Term::Handle`; the tree-walk `Handle` arm is the reference.

**`Term::Handle` — `term_interpreter`** (`engine/src/term_interpreter.rs:158-167`):
simpler static model — `impl_` evaluates to a handler value inserted
into an immutable `handlers` map keyed by `target`; `eval_op`'s fallback
(:883-908) tries labels `[dim_label, Foreign(name), op_name]`, reads
`handler.impls[op]` or `handler[op]`, deserializes the entry as a `Term`
and interprets it. No handler stack, no IO-former pass, no `return`/
`acc`/resume/carry.

**Op dispatch** (`dispatch_op`, `lib.rs:2322-2425`): innermost-first
scan of `handler_stack` with a `grades` coverage check, then handlers
carried on the incoming value, then `lang.handler_for_grade(dim)`.
Impls apply as arrow values directly or as serialized `Term`s; `acc`
frames thread `(payload, s) ~> (result, s')` and write `s'` back
(`native_ops.rs:2387-2404`).

**`Term::Rewrite`**: *not implemented at eval anywhere* —
`term_interpreter.rs:178` (`unsupported Term`),
`engine-exec-tree-walk/src/lib.rs:1680` (`Err(NotImplemented)`),
`engine/src/nbe.rs:229` (`NotImplemented("rewrite")`). The
`Model::rewrite` method (`lib.rs:4523+`) implements the *rewrite-step*
semantics — `Named` witness looks up `lang.law`/`lang.rewrite_rule`,
`Compute` always rewrites, then compares evaluated source/target — but
it is invoked by the rewriting machinery (`engine-transform-rewrite`,
eqsat), not by ordinary program eval. The current `rewrite` stub
(return `source`) is *closer* to "do nothing" but presents a result as
if the rewrite ran.

**`Path`/`Equiv`**: not `Term` variants at all — `Term::Opaque` records
(`kernel/.../term.rs:215-229`). The tree-walk scheme evaluator gives
`"Path"` → identity incoming (`lib.rs:1681`), `"Equiv"` → eval `a`,`b`,
boolean equality, optionally forced by a `Path`-tagged witness
(`lib.rs:1682-1699`, `:4488-4518`). The prelude `path`/`equiv` stubs
return `from`/`a` — `path` matches the identity reading by accident;
`equiv` does not (eval returns a bool, the stub returns `a`).

### What produces these terms

`host/crates/handl-kit-arrow-lower/src/lib.rs`:

- `lower_handle` (:2036-2058) constructs `Term::Handle` for the `handle`
  AST node — the only effect term built from surface syntax.
  `target` comes from `payload.name` or `op_target_name(body)`; `impl_`
  is `unquote_term`'d.
- `lower_arr` (:2060-2085) produces `Op{name, args:[]}` — the dispatch
  object `handle` exists to intercept.
- `Term::Rewrite` is only *passed through* (:844-849); nothing in
  arrow-lower constructs it. The surface `rewrite` node evals to a
  tagged *declaration* record via `eval.rewrite`
  (`kit-prelude-lang/.../eval.yaml:1283-1302` — `tag.construct "rewrite"`
  over `{name, source, target}`), and `law`/`axiom`/`rewrite` were
  removed from the canonical grammar (grammar.yaml:560, ADR-0119).

### Usage census (real `.hnl`)

- Expression-position `handle <expr> with <handler-record>`:
  `playground/effects.hnl:63-68`, `playground/demo/04_effects.hnl:70-73`,
  `lib-collections/src/collections.hnl:215`,
  `host/tests/smoke/effects.hnl` (typecheck-only; stubs diverge at run).
- Statement-position `handle "cap.name" with fn` (legacy `cap`/`op`
  registry): `lib-bytes/src/index.hnl:56-63`, `lib-serialize`,
  `lib-mood`, `lib-transform`. **compile-js already skips these** —
  `handle` is in `DECL_ONLY_KINDS` (`src/module.rs:55-57`), emitted as a
  comment only.
- `perform(op, arg)` / `IO<sig>`: `host/tests/smoke/effects.hnl` and the
  `io` module's `perform` host fn (`native_ops.rs:3577`). compile-js has
  **no** `perform`/`IO` support — `Op{name:"perform"}` falls to
  `__handl_rt.perform(...)` → `undefined` member call → TypeError.
- `path`/`equiv`/`rewrite`/`transport`/`j`/cubical cells: proof-level
  machinery; `transport` has a native op (`native_ops.rs:2755` — proof
  erasure, returns `args[1]`), `j` evals to `proj_child 2`
  (eval.yaml:1379+). No evidence these reach compile-js as runnable
  terms outside `Term::Opaque` data.

**Verdict on demand:** `handle` is live surface syntax used by demos and
lib-collections; `rewrite`/`path`/`equiv` are effectively unreachable
from `.hnl` source today (proof machinery, `Term::Opaque`, or
unsupported-at-eval anyway).

## 2. Options

### (a) Port eval semantics into prelude helpers

Feasible subset for `handle`:

- `__handl_rt.__handlers` — a JS stack (array) of handler values.
- `handle(body, target, impl)` becomes: evaluate `impl` (already a JS
  value — arrow or record), push, `await body(input)` in `try/finally`
  pop, then run the `IO`-former post-pass + `return` clause.
- **Dispatch interception is the hard part.** Body ops compile to
  `__handl_rt.<name>(args)` / `__opRef(name)` *statically* — there is no
  runtime dispatch point. Parity requires routing every non-builtin op
  call through a `__handl_rt.dispatch(name, args)` helper that consults
  the handler stack innermost-first (`handler_impl` = `impls[op] ??
  record[op]`), awaits the impl (JS arrows are async), and only then
  falls back to the builtin/`unsupported op` path. That is a change to
  `lower_op_body`'s fallback arm (`primitive.rs:521-553`) plus
  `__opRef`, not just prelude helpers.
- Handler impls that are *serialized `Term`s* rather than JS arrows
  cannot be applied on this target (no Term interpreter in the emitted
  JS) — eval supports both (`dispatch_op` :2377-2424). On JS only the
  arrow-value half is portable.
- `acc` clause: feasible — a mutable `__acc__` slot on the frame record;
  `(payload, s) ~> (result, s')` impls are just 2-ary async arrows.
- `return` clause + `IO(sig)` former pass: feasible — both are pure
  record inspections in JS (`__former__` sym check needs the sym
  encoding; `is_obj` already excludes `__sym__` markers, and the IO
  former is a plain object carrying `__former__`/`indices`).
- **Not portable:** multi-shot `resume` (≥2-ary clauses) — needs
  delimited continuations; JS async functions cannot be re-entered.
  Deep-handler carry (`attach_carried_handler`) — arrow values are bare
  JS functions; there is no arrow-record field to hang `handlers` on
  (the `flow` composite at prelude.rs:161-171 works around this with
  `Object.assign` on the function, so a parallel `__carried__` property
  is *possible* but every application site must check it — the
  `BoundRef::Arrow` call emit at primitive.rs:596-599 and the `flow`
  composite would both need to consult/reinstall it).
- `rewrite`: eval itself returns `NotImplemented` — the honest port is
  a compile error or a throwing shim, not semantics.
- `path`/`equiv`: no native op semantics exist to port; eval answers
  "unsupported op" or handler dispatch.

### (b) Honest subset — compile-error on unsupported effect terms

- `Term::Handle` → `CompileError::Lower` with a diagnostic naming the
  unsupported surface (`handle … with` needs the handler-stack
  runtime). Precedent exists: `check_sequential` already hard-errors on
  globular `Seq` (`primitive.rs:174-185`) rather than miscompiling.
- `Term::Rewrite` → compile error (eval can't run it either).
- `Op{name ∈ {handle, handle_dyn, perform, rewrite, path, equiv}}` →
  extend the op-name denylist so these never reach `__handl_rt.<name>`
  or `__opRef`; emit the eval-faithful "unsupported op: <name>"
  diagnostic at compile time.
- Delete or repurpose the four stubs so nothing silently returns a
  wrong value.
- Cost: `handle (arr X)(args) with h` programs — including
  `playground/effects.hnl` demos and `lib-collections`'s
  `handled_printLn` — stop compiling. Today they compile to *wrong*
  code, so this is strictly more honest, but it is a capability
  regression vs. the (broken) status quo only in the sense that users
  may have believed it worked.

### (c) Hybrid — support the subset real code needs

Port the **1-ary, no-`acc`, no-`resume`** slice of `handle`:

- Prelude: `__handlers` stack, `handle` (push → `await body(input)` →
  pop in `finally` → IO-former pass → `return` clause → result),
  `dispatch(name, argInput)` consulted by the generic op fallback.
- Lowerer: route unknown `Op` calls through `dispatch`; keep
  `__opRef` for 0-arg value refs (a handled op *reference* is the
  `arr <op>` dispatch object — on JS this can be a thunk
  `() => dispatch(name, …)` or a `{__arr__: name}` marker record the
  `App` path knows how to apply — needs a small design decision).
- Hard-error (compile time) on: `acc` clause presence (detectable? —
  `lower_handle` sees only `body`/`target`/`impl_`; the `acc` child is
  an AST-role concern arrow-lower may already fold or drop — **verify**
  whether `Term::Handle` can even carry `acc`; if not, no gate needed),
  ≥2-ary handler impls (arity is a *value* property — can only be
  detected at runtime → throw a clear error on dispatch), serialized-
  `Term` impls (same — runtime check, or statically when `impl_` lowers
  to a non-arrow), and deep-carried handlers across applications
  (defer: document as unsupported — `handle f with h` returning an
  arrow that *doesn't* reinstall `h` per application is a semantic gap
  to flag, or implement the `__carried__` function-property trick).
- `rewrite`/`path`/`equiv`: treat as (b) — compile-error/throwing
  shims, since eval parity is `NotImplemented` anyway.

## 3. Recommendation

**(c) hybrid, phased — but only if `handle` parity is a P5 deliverable;
otherwise (b) is the honest floor.**

Reasoning:

1. The current stubs are *worse than errors*: `handle` returns the
   handler record as the program's result; `equiv` returns `a` where
   eval returns a bool. Any consumer reading "compiles ⇒ runs" is being
   misled. At minimum, (b) must happen — the stubs should never return
   fabricated values.
2. `handle` is the only one of the four with live surface syntax and
   real `.hnl` usage (demos, `lib-collections`, the smoke corpus's
   typecheck side). Its core semantics — push handler, dynamic-extent
   dispatch, pop — maps cleanly onto JS *except* for the dispatch
   interception, which is a lowerer change (`primitive.rs` fallback +
   `__opRef`), not a prelude-only change. That keeps it inside this
   crate's remit.
3. `rewrite` has **no eval semantics to port** — `NotImplemented` in
   every evaluator. Porting `Model::rewrite` would invent behavior eval
   doesn't have. Compile-error is the parity answer.
4. `path`/`equiv` are `Term::Opaque` proof records, not ops; the stubs
   are speculative. Remove them or make them throw "unsupported op"
   like `__opRef` does for unknown names.
5. Async-arrow value model (`__handl_promise__`, prelude.rs:265-273;
   every arrow is `async __in => …`): handler impls are async arrows,
   so dispatch must `await` them — fine inside the already-async op
   bodies. But effects needing *reified* arrows — multi-shot `resume`
   (delimited continuations), serialized-`Term` impls (no Term
   interpreter in emitted JS), and carried handlers on bare function
   values — have no faithful JS encoding today. These must be
   compile-errors or explicit runtime throws, documented as subset
   boundaries.

Suggested sequencing: **P5a** — replace all four stubs with honest
failures (compile-error for `Term::Handle`/`Term::Rewrite`, denylist +
throwing shims for the op spellings); **P5b** — the `handle` 1-ary
subset (handler stack + `dispatch` + `return` clause + IO-former pass)
with runtime throws on `acc`/multi-shot/Term-impl/carried-handler
edges; **P5c** — revisit `attach_carried_handler` via a function-property
channel if demo parity demands it.

## 4. Implementation sketch (if porting `handle`)

**Prelude (`src/runtime/prelude.rs`)** — new members:

```js
__handlers: [],                       // handler stack, innermost last
handlerImpl: (h, op) => (h && typeof h === "object"
  ? (h.impls && h.impls[op]) ?? h[op] : undefined),   // native_ops.rs:3481
handle: async (body, target, impl_, input) => {       // lowerer passes thunks
  __handl_rt.__handlers.push(impl_);
  try {
    let v = await body(input);
    v = __handl_rt.__interpretIO(v);                  // native_ops.rs:3876
    const ret = __handl_rt.handlerImpl(impl_, "return");
    if (ret) v = await ret(v);                        // + acc variant later
    return v;
  } finally { __handl_rt.__handlers.pop(); }
},
__interpretIO: (v) => { /* __former__ sym check + sig-field dispatch
                          via dispatch(); uncovered ops stay data */ },
dispatch: async (name, argInput) => {                 // dispatch_op, lib.rs:2322
  for (let i = __handl_rt.__handlers.length - 1; i >= 0; i--) {
    const impl = __handl_rt.handlerImpl(__handl_rt.__handlers[i], name);
    if (impl !== undefined) {
      if (typeof impl !== "function")
        throw new Error("handler impl for op '" + name + "' is not an arrow value");
      return await impl(argInput);
    }
  }
  return __handl_rt.__opRef(name);                    // or the builtin path
},
```

**Lowerer changes:**

- `src/lower/higher.rs:78-96` — emit
  `__handl_rt.handle(() => bodyFn(input), "target", implValue, input)`
  shape (body as a thunk so the stack is live during its eval; `impl_`
  evaluates to a *value* — record or arrow — before the push, matching
  eval order). Note `impl_` currently lowers to an arrow `__in => record`
  — it must instead lower to the record/arrow *value* itself
  (`lower(impl_)` then call with a dummy? No — eval runs `impl_` as a
  term producing the handler value; on JS the lowered `impl_` arrow
  applied to `Value::Null` input gives the handler value: emit
  `(await implFn(null))` — verify against how eval's
  `interpret_with_handlers(impl_, Value::Null)` maps).
- `src/lower/primitive.rs:521-553` — the generic `Op` fallback becomes
  `__handl_rt.dispatch("name", argTuple)` for non-builtin names (or
  `dispatch` checks builtins first — decide order to match
  `dispatch_op`'s "handler may override" semantics: eval consults
  handlers *before* native ops for unknown names but builtins are
  matched earlier in `eval_op`'s big `match`; mirror that by keeping
  the builtin `match` arms ahead of `dispatch`).
- `__opRef` (prelude.rs:204-207) — 0-arg op *references* under a handler
  need the `arr <op>` dispatch-object encoding (e.g. `{__arr__: name}`
  marker + `App`/`flow` awareness) — smallest viable version: return a
  thunk that calls `dispatch(name, …)`; document the subset gap vs.
  eval's carried-handler reinstall.
- Denylist: `handle_dyn`, `perform`, `rewrite`, `path`, `equiv` (and
  `io.perform`) → `CompileError::Lower` in `lower_op_body` before the
  fallback; `Term::Rewrite` → compile error in `proof.rs`.

**Async/promise constraints:** every impl application is `await`ed —
consistent with the `async __in => …` model. Foreign-promise impls must
route through `__promiseBox`/`__promiseUnbox` (prelude.rs:265-273) so a
host `perform` doesn't settle early; on JS there is no `perform` yet, so
this only matters for JS-host-provided handler impls via `globalThis`.

**Test plan:**

- Unit (emit-shape): `handle` term emits `__handl_rt.handle` + thunk;
  unknown `Op` emits `__handl_rt.dispatch`; denylisted names error.
- Runtime (node/deno harness — check `tests/e2e.rs` conventions):
  - `handle (arr Eff)(x) with {Eff: impl}` returns `impl(x)`; nested
    handles resolve innermost-first; uncovered ops inside the body still
    hit builtins/`unsupported op`.
  - `return` clause applies post-body; record `{impls: {…}}` and flat
    `{op: …}` handler shapes both resolve.
  - Pop-on-throw: a throwing body leaves `__handlers` empty (finally).
  - Negative: `acc`/`resume`/Term-impl/carried-handler cases throw the
    documented subset error, not a silent wrong value.
- Parity spot-check vs `hnl run` on `playground/effects.hnl`-style
  programs once `perform`/`IO` land (out of scope here — note as
  follow-up; `IO<sig>` former interpretation can be unit-tested with a
  hand-built `{__former__, indices:[sig]}` record).
