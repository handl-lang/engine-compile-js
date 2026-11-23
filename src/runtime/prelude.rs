//! Runtime prelude for primitive Handl operations in JavaScript.

use crate::config::JsOutputFormat;

/// Return the runtime prelude source for the requested output format.
///
/// The runtime object binds through `globalThis` (`||=`) rather than a
/// bare `const`/`let` redeclaration: several compiled module units
/// concatenated into one file — a consumer plus its `__handl_rt.<pkg>`
/// imports — then share a single `__handl_rt` instead of failing on a
/// duplicate declaration.
// `format!` (not `.to_string()`) is load-bearing: the literal's `{{`/`}}`
// escapes produce the single braces the JS needs.
#[allow(clippy::useless_format)]
pub fn runtime_prelude(_format: &dyn JsOutputFormat) -> String {
    format!(
        r#"var __handl_rt = globalThis.__handl_rt ||= {{
  mkPair: (a, b) => ({{__pair__: [a, b]}}),
  mkLeft: (v) => ({{__either__: "left", value: v}}),
  mkRight: (v) => ({{__either__: "right", value: v}}),
  isPair: (v) => (typeof v === "object" && v !== null && Array.isArray(v.__pair__)),
  fst: (p) => p.__pair__[0],
  snd: (p) => p.__pair__[1],
  isLeft: (v) => (typeof v === "object" && v !== null && v.__either__ === "left"),
  isRight: (v) => (typeof v === "object" && v !== null && v.__either__ === "right"),
  leftValue: (v) => v.value,
  rightValue: (v) => v.value,
  add: (a, b) => (a + b),
  sub: (a, b) => (a - b),
  mul: (a, b) => (a * b),
  div: (a, b) => (a / b),
  concat: (a, b) => (Array.isArray(a) ? a.concat(b) : String(a) + String(b)),
  projEnv: (env, name) => (env[name]),
  child: (input, role) => input[role],
  handle: (body, target, impl) => (input) => (typeof impl === "function" ? impl(input) : impl),
  rewrite: (source, target, witness) => source,
  path: (from, to, witness) => from,
  equiv: (a, b, path) => a,
  // --- arrow-lower op shims -------------------------------------------
  // Names match the op spellings the lowerer emits verbatim.
  truthy: (v) => !!v,
  "true": true,
  "false": false,
  id: (v) => v,
  elem: (v, i) => (Array.isArray(v) ? v[i] : (i === 0 ? __handl_rt.fst(v) : i === 1 ? __handl_rt.snd(v) : undefined)),
  // `product` is variadic (term_interpreter parity): 2 args is the
  // dispatch pair; the dynamic-key `match` lowering emits 3 args —
  // `[v, in, keys]` — where slot 2 carries the evaluated pattern keys.
  product: (...xs) => xs,
  paren: (v) => v,
  // `tag` — a *callable object*: invoking `__handl_rt.tag(v)` is the
  // either-inject op (`cond >>> tag >>> Choice`), while its members are
  // the `tag` host package (`handl-packages/tag`, ADR-0055) that
  // `var tag` resolves to — tagged values are objects carrying a string
  // `__tag__` key. `Op{{tag, []}}` is ambiguous between the two; this
  // covers both.
  tag: Object.assign(
    (v) => (__handl_rt.truthy(v) ? __handl_rt.mkLeft(v) : __handl_rt.mkRight(v)),
    {{
      tag: (name, payload) => Object.assign(
        {{__tag__: name}},
        (payload && typeof payload === "object" ? payload : {{value: payload}}),
      ),
      untag: (v) => {{
        if (v && typeof v === "object" && typeof v.__tag__ === "string") {{
          const {{__tag__, ...rest}} = v;
          return rest;
        }}
        return null;
      }},
      tag_of: (v) => ((v && typeof v === "object" && typeof v.__tag__ === "string") ? v.__tag__ : null),
      is_tag: (v) => !!(v && typeof v === "object" && typeof v.__tag__ === "string"),
      construct: (name, payload) => Object.assign(
        {{__tag__: name, __registry__: "handl.canonical"}},
        (payload && typeof payload === "object" ? payload : {{}}),
      ),
      match_type: (v, cases) => {{
        const t = __handl_rt.tag.tag_of(v);
        const c = (cases && typeof cases === "object" ? cases[t] ?? cases._ : undefined);
        return typeof c === "function" ? c(v) : v;
      }},
    }},
  ),
  pair_tag: (p) => {{
    const pair = Array.isArray(p) ? p : [__handl_rt.fst(p), __handl_rt.snd(p)];
    return __handl_rt.truthy(pair[0]) ? __handl_rt.mkLeft(pair[1]) : __handl_rt.mkRight(pair[1]);
  }},
  loop_outcome: (v) => ((v && typeof v === "object" && "state" in v)
    ? __handl_rt.mkRight(v.state)
    : ((v && typeof v === "object" && "out" in v) ? __handl_rt.mkLeft(v.out) : __handl_rt.mkLeft(v))),
  arr_len: (v) => (Array.isArray(v) ? v.length : -1),
  arr_drop: (v, n) => {{
    if (!Array.isArray(v)) {{ throw new Error("arr_drop: input is not an array"); }}
    return v.slice(n);
  }},
  match_fail: () => {{ throw new Error("match: no arm matched"); }},
  // `match` object-pattern ops — `has_field`/`is_obj` are the total
  // discriminant guards (a non-object reports false so the arm falls
  // through); `obj_except`/`tag_fields` run only behind those guards
  // and throw on non-objects (term_interpreter parity). `is_obj`
  // mirrors `TaggedValue::from`'s lenient wire decode: the marker
  // records (`__pair__`/`__either__`/`__bytes__`/`__sym__`/`__arrow__`/
  // `__resource__`) are tagged values, never `Obj` for patterns.
  // `obj_key` mirrors `try_object_pattern`'s key coercion
  // (`as_str().unwrap_or("")`) for computed pattern keys.
  is_obj: (v) => {{
    if (v === null || typeof v !== "object" || Array.isArray(v)) return false;
    const sym = v.__sym__;
    if (sym && typeof sym === "object" && Number.isInteger(sym.handle) && sym.handle !== 0
        && typeof sym.name === "string") return false;
    if (Array.isArray(v.__bytes__)
        && v.__bytes__.every((n) => Number.isInteger(n) && n >= 0 && n <= 255)) return false;
    if (Array.isArray(v.__pair__) && v.__pair__.length === 2) return false;
    if (v.__either__ === "left" || v.__either__ === "right") return false;
    if (v.__arrow__ === true) return false;
    const res = v.__resource__;
    if (res && typeof res === "object" && Number.isInteger(res.ptr)) return false;
    return true;
  }},
  obj_key: (v) => (typeof v === "string" ? v : ""),
  has_field: (v, k) => (__handl_rt.is_obj(v)
    && Object.prototype.hasOwnProperty.call(v, typeof k === "string" ? k : "")),
  tag_fields: (v) => {{
    if (!__handl_rt.is_obj(v)) {{ throw new Error("tag_fields: input is not an object"); }}
    return Object.keys(v).filter((k) => k !== "__tag__").sort().map((k) => v[k]);
  }},
  obj_except: (v, keys) => {{
    if (!__handl_rt.is_obj(v)) {{ throw new Error("obj_except: input is not an object"); }}
    const drop = new Set((keys || []).map((k) => (typeof k === "string" ? k : "")));
    const out = {{}};
    for (const k of Object.keys(v)) {{ if (!drop.has(k)) {{ out[k] = v[k]; }} }}
    return out;
  }},
  // `record_merge` — the object-spread fold (`{{a: 1, ...o, b: 2}}`
  // lowers to `record_merge(record_merge({{a:1}}, o), {{b:2}})`):
  // operands evaluate left-to-right, later keys overwrite earlier
  // ones, and a non-object operand is ignored
  // (term_interpreter/native_ops parity — `view_fields`' tagged-object
  // view maps onto `is_obj` here, so marker records like `__pair__`
  // are skipped rather than merged key-by-key).
  record_merge: (...args) => {{
    const out = {{}};
    for (const a of args) {{
      if (__handl_rt.is_obj(a)) {{
        for (const k of Object.keys(a)) out[k] = a[k];
      }}
    }}
    return out;
  }},
  // `flow` — the `>>>` value dispatch (`eval.flow`): both operands
  // evaluate on the incoming value; two arrow values compose into an
  // `Arrow{{first, second}}`-shaped value (`second(first(x))` on
  // application), anything else applies the right operand to the left
  // (`f(x)`). JS arrow values are async `__in => …` functions, so
  // "arrow-like" is `typeof === "function"`; the composite is itself a
  // callable carrying the `Arrow` tag/fields so chained `flow`s keep
  // composing and `__tag__` introspection still sees the record shape.
  // Subset gap: bound-arrow records (`{{__term__, __bound__}}`) lower
  // to the same function shape, so eval's "bound records apply rather
  // than compose" distinction is not observable on this target.
  flow: (a, b) => {{
    const isArrow = (v) => (typeof v === "function");
    if (isArrow(a) && isArrow(b)) {{
      return Object.assign(
        async (v) => await b(await a(v)),
        {{__tag__: "Arrow", first: a, second: b}},
      );
    }}
    if (isArrow(b)) return b(a);
    throw new Error("flow: right operand is not an arrow value");
  }},
  // `ctor_name`/`ctor_fields` — ctor-object pattern introspection
  // (ADR-0152 §2, emitted by `compile_pattern`'s ctor arms). The
  // scrutinee's `__ctor__` field carries a sym literal
  // (`{{__sym__: {{name, handle}}}}`); `ctor_name` is total — a
  // non-ctor input yields `null` so the pattern arm falls through —
  // and `ctor_fields` yields the sorted non-`__ctor__`/`type` field
  // values (the "sorted-fields fallback for positional binding"),
  // erroring on a non-object like `native_ops`.
  ctor_name: (v) => {{
    const c = (v && typeof v === "object" ? v.__ctor__ : null);
    const s = (c && typeof c === "object" ? c.__sym__ : null);
    return (s && typeof s === "object" && typeof s.name === "string" ? s.name : null);
  }},
  ctor_fields: (v) => {{
    if (!__handl_rt.is_obj(v)) {{ throw new Error("ctor_fields: input is not an object"); }}
    return Object.keys(v).filter((k) => k !== "__ctor__" && k !== "type").sort().map((k) => v[k]);
  }},
  // `assert`/`runtime_assert` — keyword/predicate assertions are
  // AST-level: the `assert` node's expr child and the
  // `runtime-assert` node's predicate/expr children never reach the
  // lowered `Term` (`Op{{assert, []}}`/`Op{{runtime_assert, []}}`
  // carry no payload), so — like `term_interpreter`'s "keyword
  // assertions are not supported by the arrow interpreter" — these
  // shims throw when evaluated rather than silently passing.
  assert: () => {{ throw new Error("assert: keyword assertions are not supported by the JS backend"); }},
  runtime_assert: () => {{ throw new Error("runtime_assert: predicate/expr are AST-level children; not supported by the JS backend"); }},
  // A bare `Op{{name, []}}` is an unbound-var / lazy op *reference*:
  // eval resolves it to a native op and errors "unsupported op" when
  // none exists — never a silent `undefined`. The shim returns the
  // runtime member when `name` is a real op/package (including ones a
  // JS host attached to `__handl_rt`) and throws the eval diagnostic
  // otherwise.
  __opRef: (name) => {{
    if (Object.prototype.hasOwnProperty.call(__handl_rt, name)) return __handl_rt[name];
    throw new Error("unsupported op: " + name);
  }},
  eq: (a, b) => __handl_rt.__eq(a, b),
  neq: (a, b) => !__handl_rt.__eq(a, b),
  __eq: (a, b) => {{
    if (a === b) return true;
    if (typeof a !== typeof b || a === null || b === null || typeof a !== "object") return false;
    const ka = Object.keys(a), kb = Object.keys(b);
    if (Array.isArray(a) !== Array.isArray(b) || ka.length !== kb.length) return false;
    return ka.every((k) => Object.prototype.hasOwnProperty.call(b, k) && __handl_rt.__eq(a[k], b[k]));
  }},
  and: (a, b) => (__handl_rt.truthy(a) && __handl_rt.truthy(b)),
  or: (a, b) => (__handl_rt.truthy(a) || __handl_rt.truthy(b)),
  pow: (a, b) => Math.pow(a, b),
  record: (...args) => {{
    const o = {{}};
    for (let i = 0; i + 1 < args.length; i += 2) o[args[i]] = args[i + 1];
    return o;
  }},
  json: (template, args) =>
    JSON.parse(args.reduce((s, a, i) => s.replace("$" + i, JSON.stringify(a)), template)),
  // Dynamic-import fallback for `Op{{import}}` terms that reach the
  // generic lowering (nested/non-module positions). File-level
  // `from "p" import {{...}}` statements emit real ES `import` decls.
  import: (spec) => import(spec),
  // The `tag` host package (`handl-packages/tag`): tagged values are
  // objects carrying a string `__tag__` key (ADR-0055). `var tag` lowers
  // to a 0-arg `Op{{tag}}`, which emits the `__handl_rt.tag` member —
  // this object — so `tag.tag_of`/`tag.untag` field accesses work.
  tag_pkg: null,
  jsCall: (module, name, args, _descriptors) => {{
    const host = globalThis[module];
    const fn = (host && typeof host[name] === "function")
      ? host[name].bind(host)
      : globalThis[name];
    if (typeof fn !== "function") {{
      throw new Error("unsafe or missing foreign call");
    }}
    return __handl_rt.__promiseBox(fn(...args));
  }},
  // The `js` contract package (`handl-packages/js`): `call`, `invoke`,
  // `error` and `await` are host-injected ops a JS host may provide on
  // `globalThis.js` (marshalling, error capture). These shims supply
  // honest defaults: `call` resolves `globalThis[obj][method]` like
  // `jsCall`, `invoke` calls `receiver[method]`, and synchronous
  // exceptions land in `__lastError` for `error()` to read — the same
  // convention the sdkgen adapters use. (`await` is not a shim: the
  // lowerer emits a real JS `await`; the host event loop owns
  // settlement, and a rejection propagates as a rejected arrow promise.)
  __lastError: null,
  // A JS promise carried through Handl data: `call`/`invoke`/`jsCall`
  // box thenables so the emitted `(await x)` wrappers — which settle any
  // raw thenable they meet — never resolve a foreign promise early or
  // turn its rejection into a thrown exception. `js.await`
  // (`__handl_rt.await`) is the only settle point; it unwraps the box.
  // The sink `catch` marks the promise observed — a boxed promise the
  // program never `js.await`s (e.g. a bound expression the lowering
  // re-emits and discards) is unread data, not a host-level fatal
  // "unhandled rejection".
  __promiseBox: (v) => {{
    if (v && typeof v.then === "function") {{
      v.catch(() => {{}});
      return {{__handl_promise__: v}};
    }}
    return v;
  }},
  __promiseUnbox: (v) =>
    ((v && typeof v === "object" && v.__handl_promise__) ? v.__handl_promise__ : v),
  call: (obj, method, args, descriptors, retKind, throws) => {{
    const host = globalThis.js;
    if (host && typeof host.call === "function") {{
      return __handl_rt.__promiseBox(host.call(obj, method, args, descriptors, retKind, throws));
    }}
    const target = globalThis[obj];
    // `globalThis[obj][method]` for namespaced APIs (`console.log`);
    // falling back to `globalThis[method]` covers bare global functions —
    // WebIDL `WindowOrWorkerGlobalScope` members surface as globals, so
    // the sdkgen adapter's `call("fetch", "fetch", ...)` reaches
    // `globalThis.fetch` itself.
    const fn = (target && typeof target[method] === "function")
      ? () => target[method](...args)
      : (typeof globalThis[method] === "function"
        ? () => globalThis[method](...args)
        : null);
    if (fn === null) {{
      __handl_rt.__lastError = new Error("unsafe or missing foreign call");
      return null;
    }}
    try {{
      __handl_rt.__lastError = null;
      return __handl_rt.__promiseBox(fn());
    }} catch (e) {{
      __handl_rt.__lastError = e;
      if (throws) {{ throw e; }}
      return null;
    }}
  }},
  invoke: (receiver, method, args, descriptors, retKind, throws) => {{
    const host = globalThis.js;
    if (host && typeof host.invoke === "function") {{
      return __handl_rt.__promiseBox(host.invoke(receiver, method, args, descriptors, retKind, throws));
    }}
    if (receiver == null || typeof receiver[method] !== "function") {{
      __handl_rt.__lastError = new Error("unsafe or missing foreign call");
      return null;
    }}
    try {{
      __handl_rt.__lastError = null;
      return __handl_rt.__promiseBox(receiver[method](...args));
    }} catch (e) {{
      __handl_rt.__lastError = e;
      if (throws) {{ throw e; }}
      return null;
    }}
  }},
  error: () => {{
    const host = globalThis.js;
    if (host && typeof host.error === "function") {{
      return host.error();
    }}
    return __handl_rt.__lastError;
  }},
  // `construct` — the `new`-expression counterpart of `call` (the `js`
  // contract package, `handl-packages/js`). Constructor resolution
  // mirrors `call`: `globalThis[obj][<path>]` for namespaced
  // constructors, `globalThis[<path>]` for bare globals — `method` may
  // be a dotted path walked from the resolved base, so
  // `construct("", "Worker", …)` reaches `globalThis.Worker` and
  // `construct("navigator", "gpu.requestAdapter", …)`-style deep paths
  // work too. `Reflect.construct` is the emitted `new Ctor(...args)`;
  // synchronous exceptions land in `__lastError` exactly like `call`.
  construct: (obj, method, args, descriptors, retKind, throws) => {{
    const host = globalThis.js;
    if (host && typeof host.construct === "function") {{
      return __handl_rt.__promiseBox(host.construct(obj, method, args, descriptors, retKind, throws));
    }}
    const path = String(method).split(".");
    let ctor = null;
    for (const base of [globalThis[obj], globalThis]) {{
      if (base == null) continue;
      let node = base;
      for (const seg of path) {{ node = (node == null) ? undefined : node[seg]; }}
      if (typeof node === "function") {{ ctor = node; break; }}
    }}
    if (ctor === null) {{
      __handl_rt.__lastError = new Error("unsafe or missing constructor");
      return null;
    }}
    try {{
      __handl_rt.__lastError = null;
      return __handl_rt.__promiseBox(Reflect.construct(ctor, Array.from(args || [])));
    }} catch (e) {{
      __handl_rt.__lastError = e;
      if (throws) {{ throw e; }}
      return null;
    }}
  }},
  // `on`/`off` — the JS→handl event trampoline (the `js` contract
  // package). `on(target, event, arrow)` wraps a compiled handl arrow —
  // an async `__in => …` JS function — in a listener and registers it
  // via `target.addEventListener`; it returns the JS listener so `off`
  // (or `invoke(target, "removeEventListener", …)`) can unregister it.
  // `target` is the EventTarget object itself or a string naming a
  // global (`"self"`, `"window"`, `"document"`, …).
  //
  // Payload convention: for `"message"` events the arrow receives
  // `event.data` — the structured-cloned handl value (`__pair__` /
  // `__either__` / `__bytes__` / `__tag__` records all survive the
  // clone); every other event receives the Event object itself. The
  // arrow is invoked as `arrow(payload)` — the single app arg-input —
  // and awaited. A rejection lands in `__lastError` (the `error` op's
  // convention) instead of escaping as an unhandled rejection.
  //
  // Reply convention (`"message"` events only): a resolved value that
  // is neither `undefined` nor `null` is posted back — to
  // `event.source` when the event carries one (MessagePort / window
  // replies), else to the target itself when it is a message peer
  // (a `Worker` — `terminate`, a worker global — `importScripts`, a
  // `MessagePort`/`BroadcastChannel` — `start`/`close`). `self` inside
  // a worker therefore replies to the parent and `worker.onmessage`
  // replies reach the worker; plain `window`/`document` targets never
  // auto-reply (a self-post would re-fire the same listener). The reply
  // must be structured-cloneable — arrows, promises and `__resource__`
  // handles cannot cross `postMessage`.
  on: (target, event, arrow) => {{
    const host = globalThis.js;
    if (host && typeof host.on === "function") {{
      return host.on(target, event, arrow);
    }}
    const t = (typeof target === "string") ? globalThis[target] : target;
    if (t == null || typeof t.addEventListener !== "function" || typeof arrow !== "function") {{
      __handl_rt.__lastError = new Error("unsafe or missing event target");
      return null;
    }}
    const name = String(event);
    const isMessage = name === "message";
    const isPeer = (v) => (v && typeof v.postMessage === "function" && (
      typeof v.terminate === "function" || typeof v.importScripts === "function"
      || typeof v.start === "function" || typeof v.close === "function"));
    const reply = (e, out) => {{
      if (!isMessage || out === undefined || out === null) return;
      const src = (e && typeof e === "object") ? e.source : null;
      const sink = (src && typeof src.postMessage === "function") ? src
        : (isPeer(t) ? t : null);
      if (sink === null) return;
      try {{
        sink.postMessage(out);
      }} catch (e1) {{
        // `window.postMessage`-shaped sinks want a targetOrigin/options
        // second argument; retry once with the same-origin "/" before
        // recording the failure.
        try {{
          sink.postMessage(out, "/");
        }} catch (e2) {{
          __handl_rt.__lastError = e2;
        }}
      }}
    }};
    const listener = (e) => {{
      const payload = (isMessage && e && typeof e === "object" && "data" in e) ? e.data : e;
      Promise.resolve()
        .then(() => arrow(payload))
        .then((out) => reply(e, out))
        .catch((err) => {{ __handl_rt.__lastError = err; }});
    }};
    t.addEventListener(name, listener);
    return listener;
  }},
  off: (target, event, listener) => {{
    const host = globalThis.js;
    if (host && typeof host.off === "function") {{
      return host.off(target, event, listener);
    }}
    const t = (typeof target === "string") ? globalThis[target] : target;
    if (t == null || typeof t.removeEventListener !== "function") {{
      __handl_rt.__lastError = new Error("unsafe or missing event target");
      return null;
    }}
    t.removeEventListener(String(event), listener);
    return null;
  }},
  // Bound-arrow record application input (`{{__term__, __bound__}}`,
  // ADR-0120 Phase 1): `bound ++ call-args` — a lone call arg stays a
  // singleton, a tuple spreads; a non-array `bound` packs as one element
  // (the tree-walk `apply_app` convention).
  __appInput: (bound, args) =>
    [...(Array.isArray(bound) ? bound : [bound]),
     ...(Array.isArray(args) ? args : [args])],
  // The `result` stdlib package (`stdlib/handl-packages/result`): `ok`
  // /`err` are `tag.tag` constructors over the host tag system — the
  // same shape the native TagModule impl produces at eval time.
  result: {{
    ok: (v) => __handl_rt.tag.tag("ok", {{ value: v }}),
    err: (e) => __handl_rt.tag.tag("err", {{ value: e }}),
  }},
  // The `bytes` host contract (`stdlib/handl-packages/bytes` — 8 FFI
  // atoms, no pure-handl implementation exists). On the JS boundary a
  // `Bytes` buffer is `{{__bytes__: [u8, ...]}}` — the ADR-0083 scalar
  // encoding — so the atoms are honest array/codec ops here. `Bytes` is
  // the type-descriptor record imported by name (`from "bytes" import
  // {{ Bytes }}`); at eval the native module supplies the same value.
  bytes: {{
    Bytes: {{__tag__: "bytes", __registry__: "type", kind: "primitive"}},
    from_list: (xs) => ({{__bytes__: Array.from(xs || [], (n) => n & 255)}}),
    to_list: (b) => Array.from((b && b.__bytes__) || []),
    concat: (a, b) => ({{__bytes__: ((a && a.__bytes__) || []).concat((b && b.__bytes__) || [])}}),
    slice: (b, off, len) => ({{__bytes__: ((b && b.__bytes__) || []).slice(off, off + len)}}),
    len: (b) => (((b && b.__bytes__) || []).length),
    at: (b, i) => {{
      const a = (b && b.__bytes__) || [];
      if (!Number.isInteger(i) || i < 0 || i >= a.length) {{
        throw new Error("bytes.at: index out of bounds");
      }}
      return a[i];
    }},
    utf8_encode: (s) => ({{__bytes__: Array.from(new TextEncoder().encode(s))}}),
    utf8_decode: (b) => new TextDecoder().decode(Uint8Array.from((b && b.__bytes__) || [])),
  }},
  // The `math` host contract (`stdlib/handl-packages/utils/math.hnl` —
  // FFI atoms over f64/u64). The integer/bitwise surface goes through
  // BigInt so `shr`/`shl`/`bit_*` keep 64-bit semantics past JS's
  // 32-bit bitwise limit; the rest are `Math` pass-throughs.
  math: {{
    bit_and: (a, b) => Number(BigInt(a) & BigInt(b)),
    bit_or: (a, b) => Number(BigInt(a) | BigInt(b)),
    bit_xor: (a, b) => Number(BigInt(a) ^ BigInt(b)),
    bit_not: (a) => Number(~BigInt(a)),
    shl: (a, n) => Number(BigInt(a) << BigInt(n)),
    shr: (a, n) => Number(BigInt(a) >> BigInt(n)),
    u32: (a) => Number(BigInt.asUintN(32, BigInt(a))),
    rotl32: (a, n) => {{
      const x = a >>> 0;
      const r = n % 32;
      return (((x << r) | (x >>> (32 - r))) >>> 0);
    }},
    mod: (a, b) => (a % b),
    abs: (a) => Math.abs(a),
    min: (a, b) => Math.min(a, b),
    max: (a, b) => Math.max(a, b),
    floor: (a) => Math.floor(a),
    ceil: (a) => Math.ceil(a),
    round: (a) => Math.round(a),
    trunc: (a) => Math.trunc(a),
    pow: (a, b) => Math.pow(a, b),
    sqrt: (a) => Math.sqrt(a),
  }},
  // The `type` host contract's type-descriptor records — the same
  // `{{__tag__, __registry__: "type", kind: "primitive"}}` shapes
  // `kit-canonical/handl-packages/type` binds at eval — plus `of`, the
  // kind-tag introspection op (the eval `type_tag` vocabulary).
  type: {{
    Int: {{__tag__: "int", __registry__: "type", kind: "primitive"}},
    Nat: {{__tag__: "nat", __registry__: "type", kind: "primitive"}},
    Str: {{__tag__: "str", __registry__: "type", kind: "primitive"}},
    Bool: {{__tag__: "bool", __registry__: "type", kind: "primitive"}},
    Null: {{__tag__: "null", __registry__: "type", kind: "primitive"}},
    Float: {{__tag__: "float", __registry__: "type", kind: "primitive"}},
    Number: {{__tag__: "number", __registry__: "type", kind: "primitive"}},
    Bytes: {{__tag__: "bytes", __registry__: "type", kind: "primitive"}},
    of: (v) => (v === null || v === undefined ? "null"
      : Array.isArray(v) ? "array"
      : (typeof v === "object" && v.__bytes__ ? "bytes"
      : (typeof v === "function" ? "arrow"
      : (typeof v === "boolean" ? "bool"
      : (typeof v === "number" ? "number"
      : (typeof v === "string" ? "string" : "object")))))),
    // `type.to_string` — the value→string rendering the serializers
    // use (`type.to_string(bytes.len(bs))` → "8").
    to_string: (v) => (v === null || v === undefined ? "null"
      : (typeof v === "object" && v.__bytes__ ? String.fromCharCode(...v.__bytes__)
      : String(v))),
  }},
  // The `option` stdlib package (`lib-option`): `Some`/`None` are
  // `tag.tag` constructors — the same shapes `option.some`/`option.none`
  // produce at eval; `Option` is a type-descriptor constructor.
  option: {{
    Option: (of) => ({{__tag__: "option", __registry__: "type", kind: "generic", of}}),
    some: (v) => __handl_rt.tag.tag("Some", {{value: v}}),
    none: () => __handl_rt.tag.tag("None", null),
    is_some: (o) => (__handl_rt.tag.tag_of(o) === "Some"),
    is_none: (o) => (__handl_rt.tag.tag_of(o) === "None"),
    unwrap: (o) => {{
      if (__handl_rt.tag.tag_of(o) === "Some") return __handl_rt.tag.untag(o).value;
      throw new Error("option.unwrap: called on None");
    }},
    unwrap_or: (o, d) => (__handl_rt.tag.tag_of(o) === "Some"
      ? __handl_rt.tag.untag(o).value : d),
  }},
  // The `str` host contract (`stdlib/handl-packages/utils/str.hnl` —
  // FFI atoms). PARTIAL: only the members .hnl sources actually call
  // are mirrored. Indices are character (code-point) positions, matching
  // the Rust `chars()` semantics — JS UTF-16 units would miscount
  // astral characters. `find` returns the char index or -1.
  str: {{
    length: (s) => [...String(s)].length,
    is_empty: (s) => (String(s).length === 0),
    equals: (a, b) => (String(a) === String(b)),
    concat: (a, b) => (String(a) + String(b)),
    slice: (s, start, end) => [...String(s)].slice(start, end).join(""),
    char_at: (s, i) => ([...String(s)][i] ?? null),
    find: (s, sub) => {{
      const i = String(s).indexOf(String(sub));
      return i < 0 ? -1 : [...String(s).slice(0, i)].length;
    }},
    contains: (s, sub) => (String(s).indexOf(String(sub)) >= 0),
    starts_with: (s, p) => String(s).startsWith(String(p)),
    ends_with: (s, p) => String(s).endsWith(String(p)),
    split: (s, sep) => String(s).split(String(sep)),
    join: (xs, sep) => (xs || []).join(String(sep)),
    trim: (s) => String(s).trim(),
    trim_start: (s) => String(s).trimStart(),
    trim_end: (s) => String(s).trimEnd(),
    lower: (s) => String(s).toLowerCase(),
    upper: (s) => String(s).toUpperCase(),
    replace: (s, from, to) => String(s).split(String(from)).join(String(to)),
    repeat: (s, n) => String(s).repeat(n),
  }},
  // The `char` host contract (`stdlib/handl-packages/char/char.hnl`) —
  // PARTIAL: `code_point_at`/`code_point`/`from_code_point` only (the
  // surface `http_rfc` uses). `code_point_at` indexes by character
  // position (Handl `Char` semantics), like `str.char_at`.
  char: {{
    code_point_at: (s, i) => {{
      const c = [...String(s)][i];
      if (c === undefined) {{ throw new Error("char.code_point_at: index out of bounds"); }}
      return c.codePointAt(0);
    }},
    code_point: (s) => {{
      const c = [...String(s)][0];
      if (c === undefined) {{ throw new Error("char.code_point: empty string"); }}
      return c.codePointAt(0);
    }},
    from_code_point: (n) => String.fromCodePoint(n),
  }},
  // The `hash_map` wire name (`stdlib/crates/plugin-lib-stdlib` —
  // `collections.hash_map` atoms): a plain JS object as the map;
  // `set`/`get_or`/`has` are the ops .hnl sources actually call.
  hash_map: {{
    new: () => ({{}}),
    from: (o) => Object.assign({{}}, o),
    to_object: (m) => Object.assign({{}}, m),
    size: (m) => Object.keys(m || {{}}).length,
    get: (m, k, d) => ((m && typeof m === "object" && k in m) ? m[k] : (d ?? null)),
    get_or: (m, k, d) => ((m && typeof m === "object" && k in m) ? m[k] : d),
    set: (m, k, v) => Object.assign({{}}, m, {{[k]: v}}),
    has: (m, k) => !!(m && typeof m === "object" && Object.prototype.hasOwnProperty.call(m, k)),
    remove: (m, k) => {{ const o = Object.assign({{}}, m); delete o[k]; return o; }},
    keys: (m) => Object.keys(m || {{}}),
    values: (m) => Object.values(m || {{}}),
  }},
  // The `obj` package — plain-record helpers. PARTIAL: only
  // `keys`/`get`/`set` (the surface `http_rfc` uses) are mirrored.
  obj: {{
    keys: (m) => Object.keys(m || {{}}),
    get: (m, k, d) => ((m && typeof m === "object" && k in m) ? m[k] : d),
    set: (m, k, v) => Object.assign({{}}, m, {{[k]: v}}),
  }},
  // The `fetch-js` package (`handl-packages/fetch-js`): host `fetch`
  // over `globalThis.fetch`, settled through the same `{{ok}}/{{err}}`
  // bridge as `js.await` — the .hnl contract is host-provided here.
  "fetch-js": {{
    fetch: async (input, init) => {{
      try {{
        return {{ ok: await globalThis.fetch(input, init) }};
      }} catch (e) {{
        return {{ err: e instanceof Error ? e : new Error(String(e)) }};
      }}
    }},
    fetch_text: async (input, init) => {{
      const r = await __handl_rt["fetch-js"].fetch(input, init);
      if (!r || typeof r !== "object" || !("ok" in r)) return r;
      try {{
        return {{ ok: await r.ok.text() }};
      }} catch (e) {{
        return {{ err: e instanceof Error ? e : new Error(String(e)) }};
      }}
    }},
    fetch_json: async (input, init) => {{
      const r = await __handl_rt["fetch-js"].fetch(input, init);
      if (!r || typeof r !== "object" || !("ok" in r)) return r;
      try {{
        return {{ ok: await r.ok.json() }};
      }} catch (e) {{
        return {{ err: e instanceof Error ? e : new Error(String(e)) }};
      }}
    }},
    fetch_bytes: async (input, init) => {{
      const r = await __handl_rt["fetch-js"].fetch(input, init);
      if (!r || typeof r !== "object" || !("ok" in r)) return r;
      try {{
        return {{ ok: await r.ok.bytes() }};
      }} catch (e) {{
        return {{ err: e instanceof Error ? e : new Error(String(e)) }};
      }}
    }},
  }},
}};
// `await` — the Promise bridge every Promise-returning contract op
// (`js.await`) funnels through. Never rejects: resolution yields
// `tag.tag("ok", {{value: v}})`, rejection yields
// `tag.tag("err", {{value: <JsError record>}})` — the same `Result`
// shape `result.ok`/`result.err` build, so compiled callers can read
// the channel with `tag.tag_of`/`tag.untag` exactly as at eval time.
__handl_rt.await = (p) =>
  Promise.resolve(__handl_rt.__promiseUnbox(p)).then(
    (v) => __handl_rt.result.ok(v),
    (e) => __handl_rt.result.err(e instanceof Error ? e : new Error(String(e)))
  );
"#
    )
}
