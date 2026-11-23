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
    /// ES module — runtime prelude + the inlined `.hnl` dep graph —
    /// loadable via `new Worker(url_or_blob, { type: "module" })`.
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
    /// - the output format must be `esm` (inlined deps use top-level
    ///   `await`; a worker bundle is always an ES module — load it with
    ///   `{ type: "module" }`, classic-script `importScripts` workers
    ///   are not supported yet);
    /// - the input must go through the file-level module emit (a
    ///   `.hnl` module source — the single-term path has no module
    ///   surface to wire);
    /// - every surviving ES `import` must be self-contained-resolvable:
    ///   builtin schemes (`node:`, `bun:`, `deno:`) and absolute URLs
    ///   (`https://…`) are kept; relative or filesystem-absolute
    ///   specifiers cannot resolve inside a blob/object-URL worker and
    ///   are a compile error — resolve them to `.hnl` files via
    ///   [`JsConfig::modules`]/[`JsConfig::entry_imports`] so they inline
    ///   instead.
    pub worker_entry: bool,
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
