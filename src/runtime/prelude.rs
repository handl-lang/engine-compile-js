//! Runtime prelude for primitive Handl operations in JavaScript.

use crate::config::JsOutputFormat;

/// Return the runtime prelude source for the requested output format.
pub fn runtime_prelude(format: &dyn JsOutputFormat) -> String {
    let decl = if format.name() == "esm" || format.name() == "iife" {
        "const"
    } else {
        "var"
    };
    format!(
        r#"{} __handl_rt = {{
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
  }}
}};
"#,
        decl
    )
}
