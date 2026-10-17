//! Runtime prelude for primitive Handl operations in JavaScript.

use crate::config::JsOutputFormat;

/// Return the runtime prelude source for the requested output format.
///
/// The runtime object binds through `globalThis` (`||=`) rather than a
/// bare `const`/`let` redeclaration: several compiled module units
/// concatenated into one file — a consumer plus its `__handl_rt.<pkg>`
/// imports — then share a single `__handl_rt` instead of failing on a
/// duplicate declaration.
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
  product: (a, b) => [a, b],
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
    if (!host || typeof host[name] !== "function") {{
      throw new Error("unsafe or missing foreign call");
    }}
    return host[name](...args);
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
  call: (obj, method, args, descriptors, retKind, throws) => {{
    const host = globalThis.js;
    if (host && typeof host.call === "function") {{
      return host.call(obj, method, args, descriptors, retKind, throws);
    }}
    const target = globalThis[obj];
    if (!target || typeof target[method] !== "function") {{
      __handl_rt.__lastError = new Error("unsafe or missing foreign call");
      return null;
    }}
    try {{
      __handl_rt.__lastError = null;
      return target[method](...args);
    }} catch (e) {{
      __handl_rt.__lastError = e;
      if (throws) {{ throw e; }}
      return null;
    }}
  }},
  invoke: (receiver, method, args, descriptors, retKind, throws) => {{
    const host = globalThis.js;
    if (host && typeof host.invoke === "function") {{
      return host.invoke(receiver, method, args, descriptors, retKind, throws);
    }}
    if (receiver == null || typeof receiver[method] !== "function") {{
      __handl_rt.__lastError = new Error("unsafe or missing foreign call");
      return null;
    }}
    try {{
      __handl_rt.__lastError = null;
      return receiver[method](...args);
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
// `{{ok: v}}`, rejection yields `{{err: <JsError record>}}` — the
// Handl error channel stays data, matching the `js.call`/`invoke`
// `throws?` + `error()` convention.
__handl_rt.await = (p) =>
  Promise.resolve(p).then(
    (v) => ({{ ok: v }}),
    (e) => ({{ err: e instanceof Error ? e : new Error(String(e)) }})
  );
"#
    )
}
