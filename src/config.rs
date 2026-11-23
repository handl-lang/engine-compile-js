//! JS compiler target and output-format configuration.

use std::any::Any;
use std::fmt;

use handl_plugin_compile::CompilerConfig;

/// A JavaScript runtime target.
pub trait JsTarget: fmt::Debug + Send + Sync {
    /// The canonical target name.
    fn name(&self) -> &str;
}

/// The Deno JavaScript runtime.
#[derive(Debug, Clone, Copy, Default)]
pub struct Deno;

impl JsTarget for Deno {
    fn name(&self) -> &str {
        "deno"
    }
}

/// The Node.js JavaScript runtime.
#[derive(Debug, Clone, Copy, Default)]
pub struct Node;

impl JsTarget for Node {
    fn name(&self) -> &str {
        "node"
    }
}

/// The Bun JavaScript runtime.
#[derive(Debug, Clone, Copy, Default)]
pub struct Bun;

impl JsTarget for Bun {
    fn name(&self) -> &str {
        "bun"
    }
}

/// The Browser JavaScript runtime target.
#[derive(Debug, Clone, Copy, Default)]
pub struct Browser;

impl JsTarget for Browser {
    fn name(&self) -> &str {
        "browser"
    }
}

/// A JavaScript module output format.
pub trait JsOutputFormat: fmt::Debug + Send + Sync {
    /// The canonical format name.
    fn name(&self) -> &str;

    /// Wrap a generated expression body in the module boilerplate.
    fn wrap(&self, body: &str) -> String;
}

/// ECMAScript module output.
#[derive(Debug, Clone, Copy, Default)]
pub struct Esm;

impl JsOutputFormat for Esm {
    fn name(&self) -> &str {
        "esm"
    }

    fn wrap(&self, body: &str) -> String {
        format!("export default {};\n", body)
    }
}

/// CommonJS module output.
///
/// Single-term inputs wrap as `module.exports = <expr>;`. `.hnl`
/// module sources go through the file-level emit: the body runs inside
/// `module.exports = (async () => { …; return <namespace>; })();`, so
/// `require()` yields a `Promise` of the export namespace (`{ name:
/// value, …, default: <default export> }`), and surviving ES `import`s
/// lower to `require()` calls.
#[derive(Debug, Clone, Copy, Default)]
pub struct CommonJs;

impl JsOutputFormat for CommonJs {
    fn name(&self) -> &str {
        "commonjs"
    }

    fn wrap(&self, body: &str) -> String {
        format!("module.exports = {};\n", body)
    }
}

/// IIFE output, bundling the runtime prelude and returning the expression.
///
/// Single-term inputs produce `(function () { …; return <expr>; })();`.
/// `.hnl` module sources produce an *async* IIFE —
/// `(async function () { …; return <namespace>; })();` — because
/// inlined deps and applied expressions use `await`; the script
/// evaluates to a `Promise` of the export namespace, and any ES
/// `import` is a compile error (a classic script has no module loader).
#[derive(Debug, Clone, Copy, Default)]
pub struct Iife;

impl JsOutputFormat for Iife {
    fn name(&self) -> &str {
        "iife"
    }

    fn wrap(&self, body: &str) -> String {
        let prelude = crate::runtime::prelude::runtime_prelude(self);
        format!(
            "(function() {{\n{}{}  return {};\n}})();\n",
            prelude,
            if prelude.ends_with('\n') { "" } else { "\n" },
            body
        )
    }
}

/// Inline output, no module header; ideal for `<script>` or `eval`.
///
/// Single-term inputs emit `prelude + <expr>`. `.hnl` module sources
/// emit `prelude + (async () => { …; return <namespace>; })();` — a
/// bare classic script whose completion value (e.g. under `eval`) is a
/// `Promise` of the export namespace; ES `import`s are a compile error.
#[derive(Debug, Clone, Copy, Default)]
pub struct Inline;

impl JsOutputFormat for Inline {
    fn name(&self) -> &str {
        "inline"
    }

    fn wrap(&self, body: &str) -> String {
        let prelude = crate::runtime::prelude::runtime_prelude(self);
        format!("{}{}", prelude, body)
    }
}

/// Default iteration bound for `Term::Loop` feedback lowering —
/// mirrors the bound the JS backend has always emitted (`__cnt <
/// 100000`). Overridable via [`JsConfig::loop_max_iterations`].
pub const DEFAULT_LOOP_MAX_ITERATIONS: u64 = 100_000;

/// How the scalar operator family (`add`/`sub`/`mul`/`div`, the
/// comparisons, `mod`, `neg`, `not`) lowers to JavaScript — one of the
/// hardcoded lowering choices surfaced by the P-002 configurability
/// audit. The default preserves today's measured behavior; the
/// alternative is marked experimental pending the T-017 benchmark
/// harness.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum OpLowering {
    /// Per-op choice (today's behavior): cheap scalar ops emit raw JS
    /// operators (`a + b`, `a < b`, `!x`); ops with runtime semantics —
    /// `eq`/`neq` deep equality, `and`/`or` truthiness, `pow`, data
    /// constructors, effect ops — go through `__handl_rt` member calls
    /// or `dispatch`.
    #[default]
    Mixed,
    /// Route the scalar operator family through `__handl_rt` member
    /// calls as well — the uniform "prelude-call" strategy with no raw
    /// JS operators (`__handl_rt.add(a, b)`, `__handl_rt.lt(a, b)`, …).
    ///
    /// Experimental: requires the scalar-op members in the runtime
    /// prelude (always emitted) and is pending T-017 measurements
    /// before it can become a default candidate.
    Prelude,
}

/// A `.hnl` module the driver resolved for inlining into the JS bundle —
/// the ADR-0128 Phase-5 dep-table seam applied to `hnl compile js`.
///
/// `path` is the resolved canonical file path — the emit-once key.
/// `imports` are this module's own resolved import edges —
/// spec-as-written → resolved path — so a `from "./x"` inside the module
/// resolves against *its* directory, not the entry file's. Specs absent
/// from `imports` are host modules (`js`, `tag`, `str`, …): their names
/// keep binding `__handl_rt` member paths / shims.
#[derive(Debug)]
pub struct JsModule {
    /// Canonical path of the module file (emit-once key).
    pub path: String,
    /// The module's source text.
    pub source: String,
    /// Resolved import edges: spec-as-written → canonical path.
    pub imports: Vec<(String, String)>,
}

/// Configuration for the JavaScript compiler.
#[derive(Debug)]
pub struct JsConfig {
    /// The target runtime (Deno, Node, Bun, Browser, ...).
    pub target: Box<dyn JsTarget>,
    /// The module output format (ESM, CommonJS, IIFE, Inline).
    pub format: Box<dyn JsOutputFormat>,
    /// Whether debug information should be emitted.
    pub debug: bool,
    /// Keep the runtime `js` client for all foreign calls.
    ///
    /// When `true`, no foreign calls are inlined and all calls go through
    /// `__handl_rt.jsCall`.
    pub no_inline_foreign: bool,
    /// Whether a Source Map V3 should be emitted.
    pub source_map: bool,
    /// `.hnl` modules the driver resolved for bundling into the output:
    /// each is emitted once as `const __mod_<n> = <module>` and bound
    /// into `__handl_rt[<spec>]` namespaces, so package imports resolve
    /// to real values instead of dead member stubs. Empty for callers
    /// that do not perform import resolution (the compiler crate itself
    /// never touches the filesystem — resolution is the driver's job).
    pub modules: Vec<JsModule>,
    /// The entry file's resolved import edges — spec-as-written →
    /// canonical path — for `CompilerInput::source` (which carries no
    /// path of its own).
    pub entry_imports: Vec<(String, String)>,
    /// Compile the input as a **worker entry bundle**: one self-contained
    /// file — runtime prelude + the inlined `.hnl` dep graph — loadable
    /// via `new Worker(url_or_blob)` / `importScripts` / a Blob URL.
    ///
    /// The flag is a *bundle contract*, not a code convention: the
    /// worker's message wiring lives in the `.hnl` source itself,
    /// exactly as `lib-js`'s `js/worker` package spells it — the entry
    /// registers its handler at module top level with
    /// `js.on("self", "message", arrow)` (browser workers;
    /// `on_message_self` in `js/worker`) or `js.on(port, "message",
    /// arrow)` over a concrete `EventTarget` such as Node's
    /// `parentPort`. The `js.on` trampoline unwraps `MessageEvent.data`,
    /// awaits the compiled arrow, and posts a non-null result back to
    /// the parent automatically.
    ///
    /// Constraints enforced at compile time:
    ///
    /// - the output format must be `esm`, `iife`, or `inline` —
    ///   `esm` produces a module worker (load it with
    ///   `{ type: "module" }`); `iife`/`inline` produce a classic
    ///   worker script (no `type` option, `importScripts`-style).
    ///   `commonjs` is rejected: `module.exports` does not exist in a
    ///   worker global scope;
    /// - the input must go through the file-level module emit (a
    ///   `.hnl` module source — the single-term path has no module
    ///   surface to wire);
    /// - no surviving ES `import` may be unresolvable inside the
    ///   bundle: builtin schemes (`node:`, `bun:`, `deno:`) and
    ///   absolute URLs (`https://…`) are kept under `esm`; relative or
    ///   filesystem-absolute specifiers cannot resolve inside a
    ///   blob/object-URL worker and are a compile error — resolve them
    ///   to `.hnl` files via [`JsConfig::modules`]/
    ///   [`JsConfig::entry_imports`] so they inline instead. Under
    ///   `iife`/`inline` *every* ES `import` is a compile error (a
    ///   classic script has no module loader — not even `node:`
    ///   builtins), so worker entries in those formats must fully
    ///   inline their `.hnl` dep graph.
    pub worker_entry: bool,
    /// How the scalar operator family lowers — see [`OpLowering`].
    /// Defaults to [`OpLowering::Mixed`] (today's behavior).
    pub op_lowering: OpLowering,
    /// Iteration bound emitted for `Term::Loop` feedback lowering
    /// (`__cnt < N` in the generated `while`). Defaults to
    /// [`DEFAULT_LOOP_MAX_ITERATIONS`] (100 000 — the bound the backend
    /// has always emitted).
    pub loop_max_iterations: u64,
    /// Run the post-emit [`crate::SecurityScanner`] pass over the
    /// generated JavaScript (rejects `eval(`/`Function(` call patterns).
    /// Defaults to `true`. Setting `false` is an escape hatch for
    /// embedding generated code that legitimately contains those
    /// substrings (e.g. inside string literals); it weakens the
    /// ADR-0114 Phase-7 hardening, so prefer keeping it on.
    pub security_scan: bool,
    /// **Experimental** — bypass the foreign-call inline safety check.
    ///
    /// When `true` (and [`JsConfig::no_inline_foreign`] is `false`),
    /// every foreign op inlines to a direct
    /// `globalThis[<module>][<name>](...)` call regardless of the
    /// descriptor's [`crate::is_safe_for_inlining`] verdict — including
    /// names the safe list blocks (`setTimeout` with a string
    /// argument, non-identifier module spellings). `no_inline_foreign`
    /// still wins when both are set.
    ///
    /// Experimental: pending T-017 measurements; not part of the stable
    /// configuration surface.
    #[doc(hidden)]
    pub unsafe_inline_foreign: bool,
    /// Route the pure arg-op family (the scalar operators plus
    /// `eq`/`neq`/`and`/`or`/`pow`/`concat`) through
    /// `__handl_rt.dispatch` instead of inline JS operators / direct
    /// `__handl_rt` member calls.
    ///
    /// Eval's `dispatch_op` consults the dynamic-extent handler stack
    /// for *every* `Native` op before `apply_native` — a `handle` frame
    /// can therefore intercept `add`/`lt`/etc. The default lowering
    /// gives those ops dedicated emit that bypasses handler dispatch
    /// entirely; this flag restores interception coverage for the
    /// ops whose semantics are a pure function of their arguments.
    ///
    /// Cost: every covered op becomes an `await`ed async dispatch —
    /// measurably slower on arithmetic-heavy code, and still not full
    /// parity: ops that read the incoming value or carry `lit`
    /// payloads (`elem`, `field_access`, `record`, `json`, `proj_env`,
    /// …) keep their dedicated emit and bypass handlers even under the
    /// flag (the dispatch fallback cannot reproduce lit/incoming
    /// semantics). Defaults to `false`.
    pub dispatch_all_ops: bool,
}

impl JsConfig {
    /// Create a config for Deno + ESM.
    pub fn esm_deno() -> Self {
        Self {
            target: Box::new(Deno),
            format: Box::new(Esm),
            debug: false,
            no_inline_foreign: false,
            source_map: false,
            modules: Vec::new(),
            entry_imports: Vec::new(),
            worker_entry: false,
            op_lowering: OpLowering::Mixed,
            loop_max_iterations: DEFAULT_LOOP_MAX_ITERATIONS,
            security_scan: true,
            unsafe_inline_foreign: false,
            dispatch_all_ops: false,
        }
    }

    /// Create a config for Node + CommonJS.
    pub fn cjs_node() -> Self {
        Self {
            target: Box::new(Node),
            format: Box::new(CommonJs),
            debug: false,
            no_inline_foreign: false,
            source_map: false,
            modules: Vec::new(),
            entry_imports: Vec::new(),
            worker_entry: false,
            op_lowering: OpLowering::Mixed,
            loop_max_iterations: DEFAULT_LOOP_MAX_ITERATIONS,
            security_scan: true,
            unsafe_inline_foreign: false,
            dispatch_all_ops: false,
        }
    }
}

impl CompilerConfig for JsConfig {
    fn as_any(&self) -> &dyn Any {
        self
    }

    fn target(&self) -> &str {
        "js"
    }

    fn debug(&self) -> bool {
        self.debug
    }

    fn source_map(&self) -> bool {
        self.source_map
    }
}
