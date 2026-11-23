# The JS runtime model

Why `handl-plugin-compile-js` emits a runtime prelude (`__handl_rt`,
`src/runtime/prelude.rs`) instead of direct-transpiling Handl to plain
JavaScript. The short answer: **the two languages have different
semantics, and JS cannot express Handl's on its own**. The prelude is
where the semantic gap is paid for, once, so the emitted code can stay
small and honest.

## 1. Arrows are single-input async functions

A Handl arrow is a uniform `__in => …` value — one input, one output,
always potentially effectful. A JS function is multi-argument and
synchronous by default. The compiler resolves this by convention:

- every emitted arrow is `async` (`src/emit/mod.rs:412-424`,
  `src/lower/mod.rs:235-239`), so `await` is legal anywhere an
  expression is;
- every application passes **one** argument — the lone call arg, or the
  `[args]` tuple packed by `__handl_rt.__appInput`
  (`src/runtime/prelude.rs:831-833`);
- every application site awaits the result (`(await fn(input))`).

The cost is that nothing in the emitted JS is ever sync — including
arithmetic. The benefit is a uniform calling convention that matches
eval's app-arg model exactly, with no arity analysis.

## 2. Handl data has no JS-native shape

Pairs, eithers, tagged constructors, bytes, syms and arrows do not
exist in JS. They are encoded as **marker records** on plain objects
(`src/runtime/prelude.rs:18-25, 474-487, 645-651, 847-862`):

| Handl value | JS encoding |
|---|---|
| pair | `{__pair__: [a, b]}` |
| either | `{__either__: "left"\|"right", value: v}` |
| tagged ctor | `{__tag__: name, ...fields}` |
| bytes | `{__bytes__: [u8, …]}` |
| sym | `{__sym__: {name, handle}}` |
| arrow | async function (+ `__carried__` handler frames) |

The encoding is deliberately chosen so values survive
`postMessage` structured clone — a requirement for the worker use
case (`worker_entry`, see `src/config.rs:221-258`). Functions and
`__resource__` handles cannot cross the boundary and are documented
as such in `js.on`'s reply convention
(`src/runtime/prelude.rs:743-826`).

Because markers are ordinary records, ops that inspect data
(`is_obj`, `record_merge`, `ctor_name`, `ctor_fields`, `flow`) must
*know* the encoding — that knowledge cannot live in JS, so it lives in
the prelude, where it is written once and tested once.

## 3. Calculus ops are runtime helpers

Cheap ops (`add`, `eq`, `and`, `elem`, literal records) emit raw JS
inline. Ops whose semantics encode eval parity go through
`__handl_rt`:

- `record_merge` (`src/runtime/prelude.rs:509-517`) — tagged objects
  are *not* mergeable records; `is_obj` excludes every marker shape
  (`src/runtime/prelude.rs:474-487`).
- `flow` (`src/runtime/prelude.rs:529-541`) — the `>>>` dispatch:
  arrow∘arrow composition vs application, preserving eval's
  `Arrow{first, second}` record shape for `__tag__` introspection.
- `ctor_name` / `ctor_fields` (`src/runtime/prelude.rs:550-558`) —
  ctor-object pattern introspection (ADR-0152 §2).
- `match` discriminant guards (`is_obj`, `has_field`, `obj_except`,
  `tag_fields`, `match_fail`, …) — total-vs-throwing distinctions that
  mirror `term_interpreter`/`native_ops` exactly (`src/runtime/prelude.rs:464-501`).

If a semantic detail were inlined per call site, every lowering would
have to re-derive it and drift. The prelude makes parity a property of
one audited file.

## 4. Effects need a dispatch layer JS doesn't have

Eval implements `handle`/`perform`/`dispatch_op` over a
**dynamic-extent handler stack** with deep-handler reinstall
(`attach_carried_handler`). JS has no delimited continuations, so the
prelude re-implements the observable contract:

- `__handl_rt.__handlers` is the handler stack
  (`src/runtime/prelude.rs:53-54`);
- `handle` pushes the impl (seeding a `__acc__` slot when the
  `Term::Handle` carries an `acc` seed — ADR-0158), runs the body,
  applies the `return` clause / `acc` postlude, and stamps
  `__carried__` on arrow results
  (`src/runtime/prelude.rs:145-291`);
- `__apply` reinstalls carried frames around each application
  (`src/runtime/prelude.rs:106-117`) — the deep-dispatch channel;
- `dispatch`/`perform`/`__interpretIO` route covered ops to handler
  impls and mint `IO` residuals for uncovered ones
  (`src/runtime/prelude.rs:298-404`);
- `dispatchForeign` gives foreign ops the same handler-interception
  channel before falling back to inline/`jsCall`
  (`src/runtime/prelude.rs:362-375`).

Under `acc`, covered-op impls run the `(payload, s) ~> (result, s')`
convention: `__dispatchCovered` reads the covering frame's `__acc__`,
applies the impl to the `[payload, s]` pair, splits the
`(result, s')` pair result (`acc_result_split`), and writes `s'` back
via `__setFrameAcc` — which *replaces* the stack entry so the carried
seed keeps `s0` and each later application restarts the accumulator
(`src/runtime/prelude.rs:145-228`). A handle with no `return` clause
and a live `__acc__` yields the `(result, s_final)` pair.

This is still a *subset*: multi-shot `resume` (≥2-ary impls),
serialized-`Term` impls, and compound `dim` terms that don't resolve
to a literal label all fail loudly (see §6 and
`docs/limitations.md`). Where eval semantics can't be expressed, the
compiler fails loudly rather than approximating.

## 5. Promises are boxed so only `js.await` settles them

Foreign thenables are wrapped as `{__handl_promise__: p}`
(`src/runtime/prelude.rs:645-651`) at every FFI boundary (`call`,
`invoke`, `construct`, `jsCall`). Emitted `(await x)` expressions would
otherwise settle a raw thenable early — before the program's `await`
op has a chance to route it through Handl's `ok`/`err` `Result`
channel. `__handl_rt.await` (`src/runtime/prelude.rs:1049-1054`, lowered
at `src/lower/primitive.rs:968-990`) is the **only** settle point: it
unwraps the box and maps resolution/rejection onto
`tag.tag("ok"|"err", …)`, matching eval's `js.await` contract. A
sink `catch` inside `__promiseBox` marks boxed promises observed so
un-awaited ones don't trip host "unhandled rejection" reporting.

## 6. The honesty ladder

The compiler never silently miscompiles:

1. **Supported** → correct JS, with prelude helpers where semantics
   demand them. This now includes the module surface on all four
   formats (`src/module.rs::emit_module_surface` — ESM named exports,
   CJS `require()` + namespace-object `module.exports`, IIFE/inline
   completion-value namespaces), `export data` family/ctor exports,
   import aliases, the `acc` accumulator path of `handle`, and the
   `perform`/`io.perform` spellings — flat, qualified, and `from "io"
   import perform` forms all emit `__handl_rt.perform`, whose covered
   ops dispatch in-flight (including `acc` state threading) and whose
   uncovered ops mint the residual `type.io` former that `handle`'s
   `__interpretIO` interprets.
2. **Unsupported at compile time** → `CompileError`. `Term::Rewrite`
   is rejected outright (`src/lower/proof.rs` — eval returns
   `NotImplemented` for it too); the `path`/`equiv`/`handle_dyn`
   op *spellings* and `Term::Opaque` are denied
   in the lowerer (`src/lower/primitive.rs:700-711`,
   `src/lower/mod.rs:421,437`); `worker_entry` rejects `commonjs` and
   unresolvable surviving imports (`src/compiler.rs:37-44, 142-164`),
   and `iife`/`inline` reject *any* surviving ES `import`
   (`src/module.rs:591-606`).
3. **Unknown at compile time** → runtime throw. A bare op reference
   resolves through `__opRef`, which throws eval's own
   `unsupported op: <name>` diagnostic
   (`src/runtime/prelude.rs:574-587`); unhandled ops fall through
   `dispatch` to the same error (`src/runtime/prelude.rs:340-352`).

Known-incomplete stubs (`assert`, `runtime_assert` — AST-level
assertions that never reach the lowered `Term`) throw with an
explanatory message rather than passing silently
(`src/runtime/prelude.rs:566-567`). Effect features with no JS-async
analogue — multi-shot `resume` (≥2-ary impls), serialized-`Term`
handler impls — throw at dispatch time (`src/runtime/prelude.rs:131-144`).
The full edge list, including the upstream-divergence notes, lives in
`docs/limitations.md`.

## 7. Why not CPS / direct transpile?

Two alternatives were considered and rejected (full evaluation plan:
`project/proposals/proposal-0002-js-backend-strategy-evaluation-…md`):

- **CPS transform**: the textbook answer for effects without
  delimited continuations. Rejected on cost/benefit — it would
  restructure *every* emitted function, pessimize the common
  no-handler case, and still need the same prelude for data encoding
  and op dispatch. The dynamic-extent stack covers the handler subset
  eval actually exercises today.
- **Direct transpile / per-op inlining**: impossible for the ops in §3
  (their semantics *are* the encoding knowledge) and for effects (§4).
  Inlining is used only where it's already free — arithmetic and
  literals emit raw JS.

The prelude is the design's pressure valve: semantics that JS can't
express natively are concentrated in one auditable, testable runtime
layer, and the emitted code stays a thin, readable mapping from
Handl terms to JS expressions.
