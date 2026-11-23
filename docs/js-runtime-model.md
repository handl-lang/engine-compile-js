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

- every emitted arrow is `async` (`src/emit/mod.rs:412-423`,
  `src/lower/mod.rs:164-167`), so `await` is legal anywhere an
  expression is;
- every application passes **one** argument — the lone call arg, or the
  `[args]` tuple packed by `__handl_rt.__appInput`
  (`src/runtime/prelude.rs:620-621`);
- every application site awaits the result (`(await fn(input))`).

The cost is that nothing in the emitted JS is ever sync — including
arithmetic. The benefit is a uniform calling convention that matches
eval's app-arg model exactly, with no arity analysis.

## 2. Handl data has no JS-native shape

Pairs, eithers, tagged constructors, bytes, syms and arrows do not
exist in JS. They are encoded as **marker records** on plain objects
(`src/runtime/prelude.rs:18-25, 217-231, 632-651`):

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
case (`worker_entry`, see `src/config.rs:208-228`). Functions and
`__resource__` handles cannot cross the boundary and are documented
as such in `js.on`'s reply convention
(`src/runtime/prelude.rs:534-547`).

Because markers are ordinary records, ops that inspect data
(`is_obj`, `record_merge`, `ctor_name`, `ctor_fields`, `flow`) must
*know* the encoding — that knowledge cannot live in JS, so it lives in
the prelude, where it is written once and tested once.

## 3. Calculus ops are runtime helpers

Cheap ops (`add`, `eq`, `and`, `elem`, literal records) emit raw JS
inline. Ops whose semantics encode eval parity go through
`__handl_rt`:

- `record_merge` (`src/runtime/prelude.rs:298-310`) — tagged objects
  are *not* mergeable records; `is_obj` excludes every marker shape
  (`src/runtime/prelude.rs:264-275`).
- `flow` (`src/runtime/prelude.rs:307-329`) — the `>>>` dispatch:
  arrow∘arrow composition vs application, preserving eval's
  `Arrow{first, second}` record shape for `__tag__` introspection.
- `ctor_name` / `ctor_fields` (`src/runtime/prelude.rs:337-347`) —
  ctor-object pattern introspection (ADR-0152 §2).
- `match` discriminant guards (`is_obj`, `has_field`, `obj_except`,
  `tag_fields`, `match_fail`, …) — total-vs-throwing distinctions that
  mirror `term_interpreter`/`native_ops` exactly.

If a semantic detail were inlined per call site, every lowering would
have to re-derive it and drift. The prelude makes parity a property of
one audited file.

## 4. Effects need a dispatch layer JS doesn't have

Eval implements `handle`/`perform`/`dispatch_op` over a
**dynamic-extent handler stack** with deep-handler reinstall
(`attach_carried_handler`). JS has no delimited continuations, so the
prelude re-implements the observable contract:

- `__handl_rt.__handlers` is the handler stack
  (`src/runtime/prelude.rs:41-46`);
- `handle` pushes the impl, runs the body, applies the `return`
  clause, and stamps `__carried__` on arrow results
  (`src/runtime/prelude.rs:80-118`);
- `__apply` reinstalls carried frames around each application
  (`src/runtime/prelude.rs:68-78`) — the deep-dispatch channel;
- `dispatch`/`perform`/`__interpretIO` route covered ops to handler
  impls and mint `IO` residuals for uncovered ones
  (`src/runtime/prelude.rs:122-196`).

This is a *subset* — grades coverage and carried-vs-stack precedence
have documented gaps (see the comments at `src/runtime/prelude.rs:50-66`
and the parity plan in
`project/proposals/proposal-0001-effect-semantics-parity-…md`). Where
eval semantics can't be expressed, the compiler fails loudly rather
than approximating (see §6).

## 5. Promises are boxed so only `js.await` settles them

Foreign thenables are wrapped as `{__handl_promise__: p}`
(`src/runtime/prelude.rs:434-442`) at every FFI boundary (`call`,
`invoke`, `construct`, `jsCall`). Emitted `(await x)` expressions would
otherwise settle a raw thenable early — before the program's `await`
op has a chance to route it through Handl's `ok`/`err` `Result`
channel. `__handl_rt.await` (`src/runtime/prelude.rs:837-844`, lowered
at `src/lower/primitive.rs:733-755`) is the **only** settle point: it
unwraps the box and maps resolution/rejection onto
`tag.tag("ok"|"err", …)`, matching eval's `js.await` contract. A
sink `catch` inside `__promiseBox` marks boxed promises observed so
un-awaited ones don't trip host "unhandled rejection" reporting.

## 6. The honesty ladder

The compiler never silently miscompiles:

1. **Supported** → correct JS, with prelude helpers where semantics
   demand them.
2. **Unsupported at compile time** → `CompileError`. E.g.
   `Term::Rewrite`/proof operators with no JS lowering are rejected
   (`src/lower/proof.rs`), and `worker_entry` rejects formats that
   can't load as a worker (`src/compiler.rs:28-45`).
3. **Unknown at compile time** → runtime throw. A bare op reference
   resolves through `__opRef`, which throws eval's own
   `unsupported op: <name>` diagnostic
   (`src/runtime/prelude.rs:363-375`); unhandled ops fall through
   `dispatch` to the same error (`src/runtime/prelude.rs:155-166`).

Known-incomplete stubs (`assert`, `runtime_assert` — AST-level
assertions that never reach the lowered `Term`) throw with an
explanatory message rather than passing silently
(`src/runtime/prelude.rs:355-356`).

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
