//! File-level module emit: walk a `.hnl` file's top-level statements and
//! emit real ES module surface (`import`/`export`/`const`), reusing the
//! term-level lowering for arrow bodies.
//!
//! The canonical Pratt parser parses **one expression** per `parse` call —
//! a `.hnl` file is a sequence of top-level statements, each its own AST
//! (the same statement-chunk model the bootstrap typechecker uses:
//! `plugin-ext-diag::split_statements` + per-chunk lex/parse). The term
//! the CLI hands us covers only the *first* statement, so the module
//! emit re-derives every statement from `input.source`.
//!
//! Cross-statement names resolve through a module-scope [`BoundRef`]
//! table (imports and earlier `const`/`export const` bindings), surfaced
//! to the lowerer via [`crate::lower::with_module_env`]: an unbound
//! `var name` lowers to `Op{name}` and [`lower_op`] resolves it.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use handl_kit_arrow_lower::lower_ast;
use handl_language::{Ast, AstNode, AstRef, Language, Span};
use handl_plugin_compile::{CompileError, CompilerInput};
use handl_plugin_ext_diag::split_statements;
use hnl_engine::Term;
use hnl_tool_lex_pattern::lex;
use hnl_tool_parse::{ParserEngine, ParserRequest};
use hnl_tool_parse_pratt::PrattEngine;
use serde_json::Value;

use crate::config::JsConfig;
use crate::emit::{JsEmitter, SecurityScanner, is_js_ident};
use crate::lower::{self, BoundRef, JsAst, js_string};

/// The `js` contract package (`handl-packages/js`): bare-name imports
/// bind to `__handl_rt` shims / the real `await` op.
const JS_PACKAGE: &str = "js";

/// Prelude-owned package names that never inline a resolved `.hnl`
/// file: `tag`'s file is a pure contract (`cap`/`op`/`handle` decls, no
/// runtime values), so emitting its (empty) record over
/// `__handl_rt["tag"]` would clobber the live shim. Real impl packages
/// (`fetch-js`, …) deliberately are *not* here — a resolved file
/// supersedes the shim.
const PRELUDE_OWNED: &[&str] = &["tag"];

/// Error prefix for circular imports — mirrors `ModuleLoader`'s
/// "circular import detected". `ensure_module`'s emit-failure fallback
/// re-raises errors carrying it instead of degrading to `{}`.
const CIRCULAR_IMPORT: &str = "circular import detected";

/// Node kinds that are compile-time declarations with no runtime value —
/// they emit a comment, not a statement. `data` is deliberately absent:
/// the desugar pass rewrites it to a `__splice__`d `block` of former /
/// constructor / recursor `assign`s (handled in `emit_splice_block`); a
/// `data` node that survives to emit is a hard error, not a silent skip.
const DECL_ONLY_KINDS: &[&str] = &[
    "cap", "handle", "trait", "impl", "type", "law", "sort", "grade", "rewrite", "infer",
];

/// Try to compile `input` as a whole `.hnl` module.
///
/// Returns `Ok(None)` when the input is not a parseable module — e.g. the
/// synthetic `CompilerInput`s tests build with an empty `Language` — so
/// the caller falls back to the single-term emit path.
///
/// `config.modules`/`config.entry_imports` carry the driver's resolved
/// `.hnl` import graph (the dep-table seam): each spec that resolved to
/// a file is emitted once as an inlined module bound under
/// `__handl_rt[<spec>]`; unresolved specs keep their `__handl_rt`
/// member-path / ES-import emit.
pub(crate) fn try_compile_module(
    input: &CompilerInput,
    config: &JsConfig,
) -> Result<Option<Vec<JsAst>>, CompileError> {
    // The lowering knobs (`op_lowering`, `loop_max_iterations`,
    // `no_inline_foreign`, `unsafe_inline_foreign`) live in the
    // thread-local `LowerCtx` — the module emit must run inside
    // `with_config` just like the single-term path does, or the
    // config would be silently ignored for `.hnl` sources.
    lower::with_config(config, || try_compile_module_inner(input, config))
}

fn try_compile_module_inner(
    input: &CompilerInput,
    config: &JsConfig,
) -> Result<Option<Vec<JsAst>>, CompileError> {
    let lang = &input.language;
    if lang.lexer_rules.is_empty() || lang.grammar.is_empty() {
        return Ok(None);
    }
    let chunks = split_statements(&input.source);
    if chunks.is_empty() {
        return Ok(Some(Vec::new()));
    }

    let mut bundle = Bundle::new(config);
    let entry_edges: HashMap<String, String> = config.entry_imports.iter().cloned().collect();
    let emit = bundle.emit_module_body(lang, &input.source, entry_edges, false)?;
    let mut out = bundle.out;
    out.extend(emit.stmts);
    Ok(Some(out))
}

/// A resolved dep module held in the bundle table.
struct DepModule<'a> {
    /// The module's source text.
    source: &'a str,
    /// Its resolved import edges: spec-as-written → canonical path.
    edges: HashMap<String, String>,
}

/// What [`Bundle::ensure_module`] hands back to the importer: the dep's
/// export `(name, expr)` pairs for splice imports plus its nullary-ctor
/// literals (for `match` ctor-value patterns in the importer).
struct DepSurface {
    /// Export record entries `(exported name, value expr)`.
    exports: Vec<(String, JsAst)>,
    /// Nullary-ctor literals the dep bound, keyed by export name.
    ctors: HashMap<String, Value>,
}

/// An emitted dep module held in `Bundle::emitted`.
struct EmittedModule {
    /// JS identifier evaluating to the namespace object.
    ns: String,
    /// The dep's export surface.
    surface: DepSurface,
}

/// The result of emitting one module's statement list.
struct BodyEmit {
    /// The module's emitted statements (bindings, effects, comments).
    stmts: Vec<JsAst>,
    /// Export record entries `(exported name, value expr)` — the
    /// `export`ed names, or every bound name when the module declares
    /// none (`module_prep` parity).
    exports: Vec<(String, JsAst)>,
    /// A trailing bare-expression value (`Some` = the module record is
    /// that value, not a name projection — `module_prep` parity).
    tail: Option<JsAst>,
    /// Nullary-constructor literals this module bound (`data` decls and
    /// ctor names imported from deps), keyed by binding name — carried
    /// into `Bundle::dep_ctors` so importers can seed `lang.globals`
    /// for `match`'s ctor-value patterns (`Zero:`), mirroring eval's
    /// scope-value resolution across the import edge.
    ctor_literals: HashMap<String, Value>,
}

/// Bundle-level emit state shared by the entry module and every inlined
/// dep: the driver's resolved module table (`JsConfig::modules`), the
/// emit-once / cycle guard, and the dep consts (`Bundle::out` ends up
/// DFS-postordered before the entry module's own statements).
struct Bundle<'a> {
    /// Canonical path → module record (source + spec→path edges).
    modules: HashMap<String, DepModule<'a>>,
    /// Emitted modules: canon path → (JS expr evaluating to the
    /// namespace object, export `(name, value-expr)` pairs — consumed by
    /// `import "spec"` / `from "spec" import *` splices —, ctor literals
    /// for `match` ctor-value patterns in importers).
    emitted: HashMap<String, EmittedModule>,
    /// Paths on the active emit stack — a hit is a circular import
    /// (mirrors `ModuleLoader`'s `loading` set, which errors the same
    /// way at eval time).
    in_progress: HashSet<String>,
    /// Specs already bound to a `__handl_rt[<spec>]` assignment.
    spec_seen: HashSet<String>,
    /// Dep-module consts + `__handl_rt[...]` bindings — prepended to the
    /// entry module's statements.
    out: Vec<JsAst>,
    /// `__mod_<stem>_<n>` uniquifier.
    counter: usize,
}

impl<'a> Bundle<'a> {
    fn new(config: &'a JsConfig) -> Self {
        let modules = config
            .modules
            .iter()
            .map(|m| {
                (
                    m.path.clone(),
                    DepModule {
                        source: m.source.as_str(),
                        edges: m.imports.iter().cloned().collect(),
                    },
                )
            })
            .collect();
        Bundle {
            modules,
            emitted: HashMap::new(),
            in_progress: HashSet::new(),
            spec_seen: HashSet::new(),
            out: Vec::new(),
            counter: 0,
        }
    }

    /// Emit one module's statements into a fresh `ModuleCtx` — used for
    /// the entry file (`dep: false`, real `export` surface) and for
    /// inlined deps (`dep: true`, exports collected into a record).
    fn emit_module_body(
        &mut self,
        lang: &Language,
        source: &str,
        edges: HashMap<String, String>,
        dep: bool,
    ) -> Result<BodyEmit, CompileError> {
        let mut ctx = ModuleCtx::new(lang, &edges, dep);
        let chunks = split_statements(source);
        let last = chunks.len().saturating_sub(1);
        for (i, chunk) in chunks.iter().enumerate() {
            if let Some(msg) = retired_export_data_error(&chunk.text) {
                return Err(CompileError::InvalidInput(format!(
                    "statement {}: {msg}",
                    i + 1
                )));
            }
            let (ast, root) = parse_statement(lang, &chunk.text)
                .map_err(|e| CompileError::InvalidInput(format!("statement {}: {e}", i + 1)))?;
            let node = ast
                .get(root)
                .ok_or_else(|| CompileError::InvalidInput("statement root missing".to_string()))?;
            ctx.emit_statement(self, lang, &ast, root, node, i == last, &chunk.text)?;
        }
        Ok(ctx.finish())
    }

    /// Emit (or reuse) the dep module at `path` and bind `spec` to its
    /// namespace:
    ///
    /// ```js
    /// const __mod_<stem>_<n> = (await (async () => { <body>; return { <exports> }; })());
    /// __handl_rt[<spec>] = __mod_<stem>_<n>;
    /// ```
    ///
    /// Returns the module's export `(name, expr)` pairs for splice
    /// imports plus its nullary-ctor literals (for `match` ctor-value
    /// patterns in the importer). A dep that fails to emit degrades to
    /// an empty namespace plus a marker comment — preserving the old
    /// dead-member-path semantics — rather than failing the whole
    /// compile; a circular import errors (the same error
    /// `ModuleLoader` reports at eval time).
    fn ensure_module(
        &mut self,
        lang: &Language,
        spec: &str,
        path: &str,
    ) -> Result<DepSurface, CompileError> {
        if let Some(emitted) = self.emitted.get(path) {
            let ns = emitted.ns.clone();
            let surface = DepSurface {
                exports: emitted.surface.exports.clone(),
                ctors: emitted.surface.ctors.clone(),
            };
            self.bind_spec(spec, &ns);
            return Ok(surface);
        }
        if self.in_progress.contains(path) {
            return Err(CompileError::InvalidInput(format!(
                "{CIRCULAR_IMPORT}: {spec} ({path})"
            )));
        }
        let Some(dep) = self.modules.get(path) else {
            return Err(CompileError::InvalidInput(format!(
                "import `{spec}` resolved to `{path}`, which is not in the \
                 compile's module set — a circular import back to the entry \
                 file is the usual cause"
            )));
        };
        let source = dep.source.to_string();
        let edges = dep.edges.clone();
        self.in_progress.insert(path.to_string());
        let body = self.emit_module_body(lang, &source, edges, true);
        self.in_progress.remove(path);
        let ident = self.fresh_ident(path);
        let (stmts, surface) = match body {
            Ok(emit) => {
                let mut stmts = emit.stmts;
                stmts.push(JsAst::Return(Box::new(
                    emit.tail
                        .unwrap_or_else(|| JsAst::Object(emit.exports.clone())),
                )));
                (
                    stmts,
                    DepSurface {
                        exports: emit.exports,
                        ctors: emit.ctor_literals,
                    },
                )
            }
            Err(e) => {
                // A circular import found inside the dep body is a hard
                // error — propagate it (`ModuleLoader` errors the same
                // way at eval time); other emit gaps degrade to an empty
                // namespace + a marker comment.
                if let CompileError::InvalidInput(m) = &e
                    && m.starts_with(CIRCULAR_IMPORT)
                {
                    return Err(e);
                }
                self.out.push(JsAst::Comment(format!(
                    "compile-js: dep `{spec}` ({path}) not inlined: {e}"
                )));
                (
                    Vec::new(),
                    DepSurface {
                        exports: Vec::new(),
                        ctors: HashMap::new(),
                    },
                )
            }
        };
        self.out.push(JsAst::ConstDecl {
            name: ident.clone(),
            value: Box::new(JsAst::Await(Box::new(JsAst::Call {
                callee: Box::new(JsAst::ArrowFn {
                    params: Vec::new(),
                    body: Box::new(JsAst::Block(stmts)),
                }),
                args: Vec::new(),
            }))),
        });
        self.emitted.insert(
            path.to_string(),
            EmittedModule {
                ns: ident.clone(),
                surface: DepSurface {
                    exports: surface.exports.clone(),
                    ctors: surface.ctors.clone(),
                },
            },
        );
        self.bind_spec(spec, &ident);
        Ok(surface)
    }

    /// `__handl_rt[<spec>] = <ns>;` — once per spec text (the same file
    /// reached under two spec strings aliases both to the one const).
    fn bind_spec(&mut self, spec: &str, ns: &str) {
        if self.spec_seen.insert(spec.to_string()) {
            self.out.push(JsAst::Assign {
                name: rt_member_path(&[spec]),
                value: Box::new(JsAst::Raw(ns.to_string())),
            });
        }
    }

    fn fresh_ident(&mut self, path: &str) -> String {
        let stem = Path::new(path)
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("pkg");
        let n = self.counter;
        self.counter += 1;
        format!("__mod_{}_{}", js_ident(stem), n)
    }
}

/// The JS expression a module-bound name contributes to a dep's export
/// record (or a re-export entry): identifiers for local consts, raw
/// member paths for package members, `__handl_rt` paths for shims/ops.
fn bound_expr(r: &BoundRef) -> JsAst {
    match r {
        BoundRef::Arrow(n) | BoundRef::Ident(n) => JsAst::Ident(n.clone()),
        BoundRef::Member(p) | BoundRef::DepArrow(p) | BoundRef::Ns(p) => JsAst::Raw(p.clone()),
        BoundRef::Shim(n) | BoundRef::Op(n) => JsAst::Raw(rt_member_path(&[n])),
    }
}

struct ModuleCtx<'e> {
    /// Module-scope name table for `lower::with_module_env`.
    bound: HashMap<String, BoundRef>,
    /// Insertion order of `bound` — the dep-mode fallback export record
    /// projects every bound name when the module declares no `export`s
    /// (`module_prep` parity).
    bound_order: Vec<String>,
    stmts: Vec<JsAst>,
    /// The base `Language` (roles/globals for parsing and lowering).
    lang: &'e Language,
    /// A lazily-cloned `Language` carrying ctor globals injected by
    /// `data` declarations (`F := { C : … }` lowers nullary constructors
    /// to literal values so `match`'s ctor-value patterns see them —
    /// `lower_ast`'s env is per-statement and cannot carry module
    /// bindings, but `lang.globals` is consulted by `var` resolution).
    /// `None` until the first `data` decl; `lang()` prefers it.
    augmented_lang: Option<Language>,
    /// Names bound by signature decls (`x : T`, `export x : T`) — `let`
    /// bindings initialised to `null` (module_prep's type-only erasure),
    /// reassignable when a later `:=` supplies the implementation.
    signature_names: HashSet<String>,
    /// `export <name>` / `export <name> : <sig>` decls whose target may
    /// be bound later in the file — resolved in `finish` once every
    /// binding is known (also fixes `export { f }` before `f := …`).
    pending_exports: Vec<String>,
    /// JS names already emitted as exports — guards `export { f }`
    /// duplicates when a pending export resolves to an `export const`.
    exported_names: HashSet<String>,
    /// The importing module's resolved edges (spec → canonical path).
    edges: &'e HashMap<String, String>,
    /// Dep mode: collect exports for the `__handl_rt` record and route
    /// the trailing expression to `return`, instead of emitting `export`
    /// declarations.
    dep: bool,
    /// Dep-mode export record entries: `(exported name, value expr)`.
    exports: Vec<(String, JsAst)>,
    /// Dep-mode trailing bare-expression value (the module record).
    tail: Option<JsAst>,
    /// Nullary-ctor literals bound in this module — own `data` decls
    /// plus ctor literals propagated through imports (re-exportable).
    /// Surfaced on `BodyEmit` so `Bundle::emitted` can hand them to
    /// importers' `lang.globals`.
    ctor_literals: HashMap<String, Value>,
    /// Constructor names minted by `data` declaration groups — bound
    /// for in-module use but *not* export-surface names: the family
    /// record carries them (`module_prep` parity — only the family
    /// name is a top-level export).
    decl_ctors: HashSet<String>,
}

impl<'e> ModuleCtx<'e> {
    fn new(lang: &'e Language, edges: &'e HashMap<String, String>, dep: bool) -> Self {
        let mut bound = HashMap::new();
        // Ambient host packages visible without an import (the canonical
        // module chain — wire names resolve in every eval context): `var
        // tag`/`var hash_map` resolve to the `__handl_rt.<pkg>` package
        // object, not a call, and their `pkg.f(a, b)` calls take the
        // host spread-args convention (`BoundRef::Member`).
        for name in ["tag", "hash_map"] {
            bound.insert(name.to_string(), BoundRef::Member(rt_member_path(&[name])));
        }
        ModuleCtx {
            bound,
            bound_order: Vec::new(),
            stmts: Vec::new(),
            lang,
            augmented_lang: None,
            signature_names: HashSet::new(),
            pending_exports: Vec::new(),
            exported_names: HashSet::new(),
            edges,
            dep,
            exports: Vec::new(),
            tail: None,
            ctor_literals: HashMap::new(),
            decl_ctors: HashSet::new(),
        }
    }

    /// The `Language` handed to `lower_ast` — the augmented clone once a
    /// `data` decl has injected ctor globals, else the base language.
    fn lang(&self) -> &Language {
        self.augmented_lang.as_ref().unwrap_or(self.lang)
    }

    /// Bind a module-scope name; in dep mode the name joins the
    /// fallback export record.
    fn bind(&mut self, name: String, r: BoundRef) {
        if self.dep && !self.bound.contains_key(&name) {
            self.bound_order.push(name.clone());
        }
        self.bound.insert(name, r);
    }

    fn finish(mut self) -> BodyEmit {
        // Resolve deferred `export <name>` decls now that every binding
        // is known — a name bound anywhere in the module exports its
        // final value; a never-bound name is a type-only signature
        // (`module_prep` erases those to `null` in the module record,
        // and ESM has no runtime binding to export).
        for name in std::mem::take(&mut self.pending_exports) {
            // Decl-group ctor names are bound but not export-surface
            // names — `export Zero` does not name a top-level binding
            // (`module_prep` parity).
            let bound = self
                .bound
                .get(&name)
                .filter(|_| !self.decl_ctors.contains(&name));
            match bound {
                Some(r) => {
                    let r = r.clone();
                    if self.dep {
                        self.exports.push((name.clone(), bound_expr(&r)));
                    } else {
                        self.emit_entry_export(&name, &r);
                    }
                }
                None => {
                    if self.dep {
                        self.exports.push((name.clone(), JsAst::Null));
                    } else {
                        self.stmts.push(JsAst::Comment(format!(
                            "compile-js: `export {name}` has no runtime binding \
                             (type-only/signature export — nothing emitted)"
                        )));
                    }
                }
            }
        }
        let exports = if self.exports.is_empty() && self.dep {
            // No `export` declarations: the module record projects every
            // top-level bound name (`module_prep` parity). Dotted names
            // (`F.rec` recursor binds) are not valid Handl export names —
            // `module_prep`'s `is_valid_ident` skips them too.
            self.bound_order
                .iter()
                .filter(|n| !n.contains('.') && !self.decl_ctors.contains(*n))
                .filter_map(|n| self.bound.get(n).map(|r| (n.clone(), bound_expr(r))))
                .collect()
        } else {
            self.exports
        };
        BodyEmit {
            stmts: self.stmts,
            exports,
            tail: self.tail,
            ctor_literals: self.ctor_literals,
        }
    }

    /// Emit the ESM surface for `export <name>` once `name` is known to
    /// be bound: `export { js }` when the binding already is a real JS
    /// name (`const`/import), otherwise a `const` alias of the bound
    /// expression first (dep member paths, `__handl_rt` shims).
    fn emit_entry_export(&mut self, name: &str, r: &BoundRef) {
        let js = match r {
            BoundRef::Arrow(js) | BoundRef::Ident(js) => js.clone(),
            _ => js_ident(name),
        };
        if !self.exported_names.insert(js.clone()) {
            return;
        }
        match r {
            BoundRef::Arrow(_) | BoundRef::Ident(_) => {
                self.stmts.push(JsAst::ExportNames(vec![(js.clone(), js)]));
            }
            _ => {
                // Alias the bound expression to a local `const` so the
                // `export { … }` has a real binding; the module's own
                // uses keep resolving through the original `BoundRef`
                // (call conventions differ per variant).
                self.stmts.push(JsAst::ConstDecl {
                    name: js.clone(),
                    value: Box::new(bound_expr(r)),
                });
                self.stmts.push(JsAst::ExportNames(vec![(js.clone(), js)]));
            }
        }
    }
}

/// Emit a module's statement list for a non-ESM output format
/// (`commonjs`/`iife`/`inline`).
///
/// The ESM surface (`import`/`export` declarations) has no meaning in a
/// classic script or a CommonJS file, so it is lowered here:
///
/// - `export const/let` → the plain declaration plus a `"name"` entry in
///   the returned namespace object;
/// - `export { a as b }` → a `"b": a` namespace entry (live-binding
///   parity is approximate: the object is built once, at the end, so it
///   reads each binding's final value — the same snapshot
///   `module.exports = {…}` gives);
/// - `export default e` → a `"default": e` namespace entry;
/// - top-level `await`s (inlined dep consts, applied trailing
///   expressions) are illegal outside an async context, so the whole
///   body runs inside `(async () => { …; return <ns>; })()`.
///
/// The produced value is a `Promise` of the namespace record
/// `{ name: value, …, "default": <default export> }` — the analogue of
/// `import * as ns` / `ns.default`. Per-format shape:
///
/// - `commonjs`: `module.exports = (async () => { … })();` — `require()`
///   yields the promise (`await require("./x.cjs")`). Surviving ES
///   `import`s lower to real `require()` calls — specifiers resolve
///   relative to the emitted file, the same contract ESM imports have —
///   so `node:` builtins and driver-resolved paths both work.
/// - `iife`: `(async function () { … })();` — the script evaluates to
///   the promise (e.g. `eval(src)` or `<script>` + global capture).
/// - `inline`: `prelude` + `(async () => { … })();` — a bare classic
///   script; its completion value (`eval`, `new Function` body) is the
///   promise.
///
/// `iife`/`inline` have no module loader at all: any surviving ES
/// `import` — a specifier the driver did not resolve to a `.hnl` file —
/// is a hard compile error rather than a silent global. `commonjs`
/// keeps them as `require()` calls. `prelude` is placed per format:
/// inside the wrapper for `iife` (self-contained), top-level for
/// `commonjs`/`inline` (matching each format's `wrap()` contract).
///
/// `security_scan` is `JsConfig::security_scan`: when `false` the
/// [`SecurityScanner`] pass over the emitted body and namespace
/// surface is skipped.
pub(crate) fn emit_module_surface(
    stmts: &[JsAst],
    format_name: &str,
    prelude: &str,
    security_scan: bool,
) -> Result<String, CompileError> {
    if !matches!(format_name, "commonjs" | "iife" | "inline") {
        return Err(CompileError::InvalidInput(format!(
            "emit_module_surface: unsupported format {format_name:?}"
        )));
    }
    let emitter = JsEmitter;
    let mut plain: Vec<JsAst> = Vec::new();
    let mut named: Vec<(String, JsAst)> = Vec::new();
    let mut default: Option<JsAst> = None;
    for stmt in stmts {
        match stmt {
            JsAst::Import {
                spec,
                named: imports,
                namespace,
            } => match format_name {
                "commonjs" => plain.push(require_import(spec, imports, namespace.as_deref())),
                _ => {
                    return Err(CompileError::InvalidInput(format!(
                        "{format_name}: `import … from {spec:?}` cannot be expressed — \
                         the format has no module loader; resolve it to a `.hnl` module \
                         via `JsConfig::modules`/`entry_imports` so it inlines"
                    )));
                }
            },
            JsAst::ExportDecl(inner) => {
                if let JsAst::ConstDecl { name, .. } | JsAst::Let { name, .. } = inner.as_ref() {
                    push_named(&mut named, name.clone(), JsAst::Ident(name.clone()));
                }
                plain.push(inner.as_ref().clone());
            }
            JsAst::ExportNames(pairs) => {
                for (local, exported) in pairs {
                    push_named(&mut named, exported.clone(), JsAst::Ident(local.clone()));
                }
            }
            JsAst::ExportDefault(expr) => default = Some(expr.as_ref().clone()),
            other => plain.push(other.clone()),
        }
    }
    if let Some(d) = default {
        push_named(&mut named, "default".to_string(), d);
    }
    let surface = JsAst::Object(named);
    let body = emitter.emit_module(&plain)?;
    let surface_js = emitter.emit_expr(&surface)?;
    if security_scan {
        SecurityScanner::new().scan(&body)?;
        SecurityScanner::new().scan(&surface_js)?;
    }
    let full = match format_name {
        "commonjs" => format!(
            "{prelude}module.exports = (async () => {{\n{body}  return {surface_js};\n}})();\n"
        ),
        "iife" => format!("(async function() {{\n{prelude}{body}  return {surface_js};\n}})();\n"),
        // `inline`: prelude + a bare async-IIFE expression statement —
        // the script's completion value is the namespace promise.
        _ => format!("{prelude}(async () => {{\n{body}  return {surface_js};\n}})();\n"),
    };
    Ok(full)
}

/// Record `(exported name, value expr)` in the namespace object,
/// replacing an earlier entry of the same name (last binding wins,
/// matching eval's record projection).
fn push_named(named: &mut Vec<(String, JsAst)>, name: String, value: JsAst) {
    if let Some(slot) = named.iter_mut().find(|(n, _)| *n == name) {
        slot.1 = value;
    } else {
        named.push((name, value));
    }
}

/// Lower an ES `import` declaration to a CommonJS `require` binding:
/// `import * as ns` → `const ns = require(spec)`, named imports →
/// `const { "a": x } = require(spec)`, a bare `import "spec"` →
/// `require(spec);` (side effects only).
fn require_import(spec: &str, named: &[(String, String)], namespace: Option<&str>) -> JsAst {
    let call = || JsAst::Call {
        callee: Box::new(JsAst::Ident("require".to_string())),
        args: vec![JsAst::Str(spec.to_string())],
    };
    if let Some(ns) = namespace {
        return JsAst::ConstDecl {
            name: ns.to_string(),
            value: Box::new(call()),
        };
    }
    if named.is_empty() {
        return call();
    }
    // Destructuring needs no `JsAst` pattern node — emit the decl as a
    // raw snippet (quoted keys keep non-identifier import names legal).
    let props = named
        .iter()
        .map(|(imported, local)| format!("{}: {local}", js_string(imported)))
        .collect::<Vec<_>>()
        .join(", ");
    JsAst::Raw(format!(
        "const {{ {props} }} = require({})",
        js_string(spec)
    ))
}

/// `export data F := …` is a retired declaration form — the `data`
/// keyword is gone from the surface; the canonical family export is
/// `export F := new Inductive { … }` (the family record carries the
/// constructors). With `data` demoted to an ordinary identifier the
/// parser would otherwise read `export data Nat := Zero | Succ(Nat)`
/// as "export the binding `data`" followed by an unrelated family
/// declaration — silently wrong. Detect the pattern textually,
/// mirroring `module_prep`'s `is_retired_data_decl_rest`, and fail
/// loudly. Returns the error message when a retired form is present.
fn retired_export_data_error(text: &str) -> Option<String> {
    let bytes = text.as_bytes();
    let mut depth: i32 = 0;
    let mut i = 0usize;
    while i < bytes.len() {
        let b = bytes[i];
        // String literal — skip to the closing quote (respecting escapes).
        if b == b'"' {
            i += 1;
            while i < bytes.len() {
                match bytes[i] {
                    b'\\' => i += 2,
                    b'"' => {
                        i += 1;
                        break;
                    }
                    _ => i += 1,
                }
            }
            continue;
        }
        // `--` line comment and `/-- … --/` block comment — skip.
        if b == b'-' && i + 1 < bytes.len() && bytes[i + 1] == b'-' {
            while i < bytes.len() && bytes[i] != b'\n' {
                i += 1;
            }
            continue;
        }
        if b == b'/' && i + 2 < bytes.len() && bytes[i + 1] == b'-' && bytes[i + 2] == b'-' {
            i += 3;
            while i + 2 < bytes.len()
                && !(bytes[i] == b'-' && bytes[i + 1] == b'-' && bytes[i + 2] == b'/')
            {
                i += 1;
            }
            i = (i + 3).min(bytes.len());
            continue;
        }
        // `export` keyword at bracket depth 0 — word boundaries on both
        // sides (`reexport`, `exportable`, `exportx` are not keywords).
        if depth == 0
            && b == b'e'
            && text[i..].starts_with("export")
            && (i == 0 || !is_ident_byte(bytes[i - 1]))
            && !matches!(bytes.get(i + 6), Some(&c) if is_ident_byte(c))
            && is_retired_data_decl_rest(&text[i + 6..])
        {
            return Some(
                "module prep: `export data` declarations are removed — \
                 declare the family as `export F := new Inductive { … }` \
                 (params: `export F<n : Nat, a : Type> := new Inductive { … }`); \
                 the exported family record carries its constructors"
                    .to_string(),
            );
        }
        match b {
            b'(' | b'{' | b'[' => depth += 1,
            b')' | b'}' | b']' => depth -= 1,
            _ => {}
        }
        i += 1;
    }
    None
}

fn is_ident_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

/// Whether `rest` (the text after `export`) opens a retired `data`
/// family declaration — the `data` keyword followed by the family
/// name. `export data_transfer := …` is an ordinary binding
/// (`data_transfer` is one identifier) and `export data := …` binds
/// the name `data`; neither is a declaration. Mirrors
/// `module_prep::is_retired_data_decl_rest`.
fn is_retired_data_decl_rest(rest: &str) -> bool {
    let rest = rest.trim_start();
    let Some(after) = rest.strip_prefix("data") else {
        return false;
    };
    // `data` must end at a word boundary …
    match after.chars().next() {
        Some(c) if c.is_alphanumeric() || c == '_' => return false,
        _ => {}
    }
    // … and be followed by the family name (an identifier).
    after
        .trim_start()
        .chars()
        .next()
        .is_some_and(|c| c.is_alphabetic() || c == '_')
}

/// Lex + parse + desugar one statement chunk. Returns the (possibly
/// rewritten) AST and its root.
fn parse_statement(lang: &Language, text: &str) -> Result<(Ast, AstRef), String> {
    let tokens = lex(lang, text).map_err(|e| format!("lex: {e}"))?;
    if tokens.as_slice().is_empty() {
        return Ok((Ast::new(), AstRef::new(0)));
    }
    let req = ParserRequest::new(tokens);
    let ast = PrattEngine::new()
        .parse(lang, &req)
        .map_err(|e| format!("parse: {e}"))?;
    if ast.is_empty() {
        return Ok((ast, AstRef::new(0)));
    }
    // Mirror `Runtime::parse`: the canonical `apply_desugar` driver runs
    // between parse and lowering — `normalize_family_records` (the
    // `F := { … }` family-record surface spelling becomes a `data` node;
    // `X := new Inductive { … }` lowers to `data` at parse time), the
    // `desugar` transform pass, and `splice_decl_blocks`
    // (a root-level `__splice__` block stays flagged — `emit_statement`
    // walks its children as module-scope bindings).
    let ast = handl_runtime::desugar_pass::apply_desugar(lang, &ast)
        .map_err(|e| format!("desugar: {e}"))?;
    if ast.is_empty() {
        return Ok((ast, AstRef::new(0)));
    }
    let ast = ast;
    let root = statement_root(&ast);
    Ok((ast, root))
}

/// Node indices `node`'s payload references as AST children: the
/// `children` role array plus `function`'s `body` field (desugar-minted
/// functions carry `{"body": idx, "params": […]}` instead of
/// `children`). Other numeric payload fields (`termination`, lit
/// `handle`s, …) are values, not arena links.
fn payload_child_refs(payload: &Value, out: &mut Vec<usize>) {
    if let Some(children) = payload.get("children").and_then(Value::as_array) {
        out.extend(
            children
                .iter()
                .filter_map(|v| v.as_u64().map(|n| n as usize)),
        );
    }
    if let Some(body) = payload.get("body").and_then(Value::as_u64) {
        out.push(body as usize);
    }
}

/// The statement's root node.
///
/// The Pratt convention "root is the last arena node" does not survive
/// every statement shape: parsers and `desugar` leave *orphaned residue*
/// in the arena — a `data` declaration's ctor `alt` and superseded
/// `__splice__` blocks trail the real root, while `from "…" import
/// { a as b }` emits `import.alias` pair nodes *before* `stmt.import`
/// (the names live in its `names` payload, not `children`).
/// `ast.len() - 1` and "biggest unreferenced subtree" both misfire:
/// residue can sit at either end and can out-reach the root when the
/// root keeps children in payload fields (`stmt.import.names`).
///
/// Recover the root in two passes over the *unreferenced* candidates:
///
/// 1. **Outermost span wins.** The true root's span covers the whole
///    statement; *spanned* residue sits strictly inside it (`import.alias`
///    @`a as b`, ctor `alt` @`Name : T`, the superseded `Nat := {…}`
///    assign a `data` decl desugars away). A candidate whose span is
///    strictly contained in another candidate's is eliminated. Spanless
///    candidates (desugar-minted nodes — including the live
///    `__splice__` root of a top-level `data` decl) cannot be contained
///    and survive this pass.
/// 2. **Rank survivors by `(reach, index)`**: subtree size over
///    `children`/`body` edges, ties toward the highest index — closest
///    to the last-node convention. Reach, not span, decides here: the
///    live root contains every desugared binding, while dead residue
///    reaches only its own slice — and a *spanned* candidate can still
///    be dead (the pre-desugar `Nat := {…}` assign keeps its span after
///    `__splice__` takes over).
///
/// Falls back to `len - 1` when no unreferenced node exists (should not
/// happen — every well-formed AST has exactly one root).
fn statement_root(ast: &Ast) -> AstRef {
    let n = ast.len();
    let mut edges: Vec<Vec<usize>> = vec![Vec::new(); n];
    let mut referenced = vec![false; n];
    for (i, node) in ast.nodes().iter().enumerate() {
        let mut refs = Vec::new();
        payload_child_refs(&node.payload, &mut refs);
        for r in refs {
            if r < n {
                referenced[r] = true;
                edges[i].push(r);
            }
        }
    }
    // (reach, index) per unreferenced candidate.
    let mut cands: Vec<(usize, usize)> = Vec::new();
    for (i, is_referenced) in referenced.iter().enumerate() {
        if *is_referenced {
            continue;
        }
        // Iterative DFS over `edges` — arenas are small (one statement),
        // and candidates are few (usually exactly one).
        let mut seen = vec![false; n];
        let mut stack = vec![i];
        let mut reach = 0usize;
        while let Some(cur) = stack.pop() {
            if cur >= n || seen[cur] {
                continue;
            }
            seen[cur] = true;
            reach += 1;
            stack.extend_from_slice(&edges[cur]);
        }
        cands.push((reach, i));
    }
    // Pass 1: drop candidates whose span is strictly inside another
    // candidate's span.
    let spans: Vec<Option<Span>> = (0..n)
        .map(|i| ast.get(AstRef::new(i)).and_then(|nd| nd.span))
        .collect();
    let cand_idx: Vec<usize> = cands.iter().map(|&(_, i)| i).collect();
    let survivors: Vec<usize> = cand_idx
        .iter()
        .copied()
        .filter(|&i| {
            let Some(si) = spans[i] else {
                return true; // spanless residue can't be proven contained
            };
            !cand_idx.iter().any(|&j| {
                j != i
                    && spans[j].is_some_and(|sj| {
                        sj.start <= si.start
                            && si.end <= sj.end
                            && (sj.start < si.start || si.end < sj.end)
                    })
            })
        })
        .collect();
    // Pass 2: rank survivors by reach, then index.
    let mut best: Option<(usize, usize)> = None; // (reach, index)
    for &i in &survivors {
        let (reach, _) = cands
            .iter()
            .find(|&&(_, ci)| ci == i)
            .copied()
            .unwrap_or((0, i));
        let better = match best {
            None => true,
            Some((b_reach, b_idx)) => reach > b_reach || (reach == b_reach && i > b_idx),
        };
        if better {
            best = Some((reach, i));
        }
    }
    AstRef::new(best.map(|(_, i)| i).unwrap_or(n - 1))
}

fn children_of(node: &AstNode) -> Vec<usize> {
    node.payload
        .get("children")
        .and_then(|c| c.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_u64().map(|n| n as usize))
                .collect()
        })
        .unwrap_or_default()
}

/// Resolve a child position by named role, falling back to positional.
fn child_at(node: &AstNode, lang: &Language, role: &str, pos: usize) -> Option<usize> {
    let idx = lang
        .child_role_index(&node.kind, role)
        .ok()
        .flatten()
        .unwrap_or(pos);
    children_of(node).get(idx).copied()
}

fn var_name_of(ast: &Ast, idx: usize) -> Option<String> {
    ast.get(AstRef::new(idx))
        .and_then(|n| n.payload.as_str().map(String::from))
}

/// The bound name of a `:=`/`assign` target: a `var` (string payload)
/// or a `type-annot` whose subject name is lifted into `payload.name`
/// (`export f: Int := …` parses as `assign(type-annot(f, Int), v)`).
/// Signature-wrapped targets — `export some<T>: sig := …` parses as
/// `assign(type-annot(call(some, T)…), …)` — descend through the
/// wrapper's subject/callee (first child) to the name var.
fn binding_name_of(ast: &Ast, idx: usize) -> Option<String> {
    let mut cur = idx;
    for _ in 0..8 {
        let n = ast.get(AstRef::new(cur))?;
        if let Some(name) = n.payload.as_str().map(String::from).or_else(|| {
            n.payload
                .get("name")
                .and_then(Value::as_str)
                .map(String::from)
        }) {
            return Some(name);
        }
        cur = *children_of(n).first()?;
    }
    None
}

/// The byte range of the first `'`- or `"`-quoted string in `s`
/// (module specifiers never contain quotes).
fn quoted_range(s: &str) -> Option<(usize, usize)> {
    let bytes = s.as_bytes();
    let start = bytes.iter().position(|&b| b == b'"' || b == b'\'')?;
    let quote = bytes[start];
    let end = s[start + 1..].find(quote as char).map(|i| start + 1 + i)?;
    Some((start, end + 1))
}

/// Whether `text` is a `from "<spec>" export <names>` re-export. The
/// `stmt.import` payload is identical for the `import`/`export` tail
/// keywords, so the distinction is read from the statement text — the
/// same place `module_prep::from_export_to_import` reads it.
fn is_from_export(text: &str) -> bool {
    let t = text.trim_start();
    let Some(rest) = t.strip_prefix("from") else {
        return false;
    };
    if !(rest.is_empty() || rest.starts_with(' ') || rest.starts_with('\t')) {
        return false;
    }
    let Some((_, end)) = quoted_range(rest) else {
        return false;
    };
    let tail = rest[end..].trim_start();
    tail == "export"
        || tail
            .strip_prefix("export")
            .is_some_and(|r| r.starts_with(' ') || r.starts_with('\t') || r.starts_with('{'))
}

/// Whether a module specifier is a bare host-module name (e.g. `"js"`,
/// `"tag"`) rather than a path — a single identifier/kebab segment.
fn is_bare_module_name(spec: &str) -> bool {
    !spec.is_empty()
        && spec
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
        && spec
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
}

/// A `__handl_rt` member path for package spec/member segments —
/// identifier segments use `.name`, anything else bracket access
/// (`fetch-js` under `.` would parse as subtraction).
fn rt_member_path(segments: &[&str]) -> String {
    let mut path = "__handl_rt".to_string();
    for seg in segments {
        if is_js_ident(seg) {
            path.push('.');
            path.push_str(seg);
        } else {
            path.push('[');
            path.push_str(&js_string(seg));
            path.push(']');
        }
    }
    path
}

/// A valid-JS-identifier spelling for a Handl binding name.
fn js_ident(name: &str) -> String {
    let out: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '$' {
                c
            } else {
                '_'
            }
        })
        .collect();
    if out.chars().next().is_some_and(|c| c.is_ascii_digit()) {
        format!("_{out}")
    } else {
        out
    }
}

/// Parse `(imported, local)` pairs out of `stmt.import`'s `names`
/// payload: `"a"` → `(a, a)`, `{"name": "b", "as": "x"}` → `(b, x)`.
fn import_names(node: &AstNode) -> Vec<(String, String)> {
    match node.payload.get("names") {
        Some(Value::String(s)) => vec![(s.clone(), js_ident(s))],
        Some(Value::Array(items)) => items
            .iter()
            .filter_map(|item| match item {
                Value::String(s) => Some((s.clone(), js_ident(s))),
                Value::Object(_) => {
                    let name = item.get("name")?.as_str()?;
                    let local = item.get("as").and_then(Value::as_str).unwrap_or(name);
                    Some((name.to_string(), js_ident(local)))
                }
                _ => None,
            })
            .collect(),
        _ => Vec::new(),
    }
}

impl ModuleCtx<'_> {
    // The statement walk carries the parse triple (`ast`/`root`/`node`)
    // plus emit context — grouping them would obscure the roles.
    #[allow(clippy::too_many_arguments)]
    fn emit_statement(
        &mut self,
        g: &mut Bundle,
        lang: &Language,
        ast: &Ast,
        root: AstRef,
        node: &AstNode,
        is_last: bool,
        text: &str,
    ) -> Result<(), CompileError> {
        match node.kind.as_str() {
            "stmt.import" => self.emit_import(g, lang, ast, node, text),
            "stmt.export" => self.emit_export(lang, ast, node),
            // A `data` node that survived the desugar pass (unparseable
            // payload — `desugar_data` declines via `pass_through`) has
            // no runtime lowering; fail loudly rather than emit a comment
            // and silently drop the declaration's constructors.
            "data" => Err(CompileError::InvalidInput(
                "`data` declaration could not be desugared — its payload is \
                 not a supported family/constructor shape"
                    .to_string(),
            )),
            kind if DECL_ONLY_KINDS.contains(&kind) => {
                self.stmts.push(JsAst::Comment(format!(
                    "compile-js: `{kind}` declaration has no runtime emit (skipped)"
                )));
                Ok(())
            }
            // `x : T` — a bare type-annot statement is a signature
            // declaration (module_prep binds it to `null`).
            "type-annot" => self.emit_signature(lang, ast, root, node, is_last, false),
            // `F := new Inductive { … }` / `F := { … }` family decls
            // desugar to a declaration group of former/ctor/recursor
            // `assign`s. Two root shapes
            // reach emit: the `__splice__`-flagged `block` itself (no
            // enclosing statement list to splice into), or — because
            // `apply_desugar`'s `splice_decl_blocks` flattens flagged
            // blocks into *any* parent with a `statements` role — the
            // unflagged surface `block` the assigns were spliced into.
            _ if is_splice_block(node) => self.emit_splice_block(lang, ast, node).map(|_| ()),
            // A user `do { … }`/`{ … }` block that happens to contain a
            // `data` decl plus only `:=` statements is *not* a flattened
            // decl group: eval's `eval_block` pushes a scope, so the
            // constructors pop with the block. Flattening it would leak
            // the family into module scope — guard on the surface text
            // under the node's span (`do`/`{` vs `data`/family name).
            _ if self.is_flattened_decl_group(lang, ast, node) && !is_user_block(node, text) => {
                self.emit_splice_block(lang, ast, node).map(|_| ())
            }
            // A `stmt.export` marker inside a declaration group — the
            // shape the retired `export data F …` spelling produced —
            // has no supported form: the canonical family export is
            // `export F := new Inductive { … }`, whose `stmt.export`
            // wraps the `data` node directly (handled in `emit_export`).
            _ if node.kind == "block"
                && children_of(node).iter().any(|&c| {
                    ast.get(AstRef::new(c))
                        .is_some_and(|n| n.kind == "stmt.export")
                }) =>
            {
                Err(CompileError::InvalidInput(
                    "`export` inside a declaration group is not a supported \
                     form — declare and export the family as \
                     `export F := new Inductive { … }`"
                        .to_string(),
                ))
            }
            _ => {
                if let Some((name, value_idx)) = self.binding_parts(lang, ast, node) {
                    self.emit_binding(lang, ast, root, node, &name, value_idx, false)
                } else {
                    self.emit_expr_statement(lang, ast, root, is_last)
                }
            }
        }
    }

    /// Whether `kind` is a binding statement — face-driven like the
    /// arrow lowerer's `bind_stmt` (`faces` metadata first, canonical
    /// kind fallback for face-less shapes).
    fn is_binding(lang: &Language, kind: &str) -> bool {
        if lang.declares_faces(kind) {
            lang.shape_has_face(kind, "binding")
        } else {
            kind == "assign" || kind == "bind"
        }
    }

    /// `(name, value_child)` when `node` is a module binding — an
    /// `assign`-layout node (`target`/`value` roles), a `bind`
    /// (`payload.name` + child 0), or an `op` declaration (`payload.name`;
    /// its value is the node itself).
    fn binding_parts(&self, lang: &Language, ast: &Ast, node: &AstNode) -> Option<(String, usize)> {
        // `assign` layout only for declared-binding kinds — otherwise a
        // `call` node's `[callee, args]` children would be misread as
        // `target`/`value`.
        let is_assign_layout = lang
            .child_role_index(&node.kind, "target")
            .ok()
            .flatten()
            .is_some()
            || (!lang.declares_faces(&node.kind) && node.kind == "assign");
        if Self::is_binding(lang, &node.kind) && is_assign_layout {
            let target_idx = child_at(node, lang, "target", 0)?;
            let name = binding_name_of(ast, target_idx)?;
            let value_idx = child_at(node, lang, "value", 1)?;
            return Some((name, value_idx));
        }
        if node.kind == "bind"
            && let Some(name) = node.payload.get("name").and_then(Value::as_str)
            && let Some(&value_idx) = children_of(node).first()
        {
            return Some((name.to_string(), value_idx));
        }
        None
    }

    #[allow(clippy::too_many_arguments)]
    fn emit_binding(
        &mut self,
        lang: &Language,
        ast: &Ast,
        root: AstRef,
        _node: &AstNode,
        name: &str,
        value_idx: usize,
        export: bool,
    ) -> Result<(), CompileError> {
        let js_name = js_ident(name);
        // Bind the name *before* lowering the value so a self-recursive
        // body resolves to the JS `const` (a `const` initializer's own
        // name is in scope once the arrow runs).
        self.bind(name.to_string(), BoundRef::Arrow(js_name.clone()));
        let value = self.lower_value(lang, ast, root, value_idx)?;
        // A name previously bound by a signature decl (`x : T` → `let x =
        // null`) is reassigned, not redeclared — `const` would be a
        // duplicate-declaration SyntaxError and `let` is what the
        // signature binding emitted.
        let reassign = self.signature_names.contains(name);
        let decl = if reassign {
            JsAst::Assign {
                name: js_name.clone(),
                value: Box::new(value),
            }
        } else {
            JsAst::ConstDecl {
                name: js_name.clone(),
                value: Box::new(value),
            }
        };
        if self.dep {
            // Inlined module: plain binding, and the name joins the
            // export record when declared `export` (and not already
            // recorded by an earlier signature export).
            self.stmts.push(decl);
            if export && !self.exports.iter().any(|(n, _)| n == name) {
                self.exports.push((name.to_string(), JsAst::Ident(js_name)));
            }
        } else if export {
            if reassign {
                // `export x : T` already emitted `export let x = null` —
                // the assignment keeps the live export binding current.
                self.stmts.push(decl);
            } else {
                self.exported_names.insert(js_name.clone());
                self.stmts.push(JsAst::ExportDecl(Box::new(decl)));
            }
        } else {
            self.stmts.push(decl);
        }
        Ok(())
    }

    /// Lower a statement's value child to the JS initializer: a quoted
    /// arrow (`Arr` holding a serialized `Term`) unquotes to the body's
    /// `async __in => ...`; any other term is applied to `null` and
    /// awaited (top-level `await` is legal in ESM).
    fn lower_value(
        &mut self,
        _lang: &Language,
        ast: &Ast,
        _root: AstRef,
        value_idx: usize,
    ) -> Result<JsAst, CompileError> {
        let term = self.lower_term(ast, AstRef::new(value_idx))?;
        match &term {
            Term::Arr { value, .. } => {
                if let Ok(inner) = serde_json::from_value::<Term>(value.clone()) {
                    return self.with_env(|| lower::lower(&inner));
                }
                // Constant value arrow — apply it for the value.
                self.with_env(|| lower::lower_applied(&term, &JsAst::Null))
            }
            _ => self.with_env(|| lower::lower_applied(&term, &JsAst::Null)),
        }
    }

    /// A bare top-level expression statement: evaluated for effect by
    /// applying its arrow to `null`. The last statement also becomes the
    /// module's default export (`export default <result>`), and a
    /// trailing arrow *value* exports the arrow itself (a function
    /// module).
    fn emit_expr_statement(
        &mut self,
        _lang: &Language,
        ast: &Ast,
        root: AstRef,
        is_last: bool,
    ) -> Result<(), CompileError> {
        let term = self.lower_term(ast, root)?;
        let applied = self.with_env(|| lower::lower_applied(&term, &JsAst::Null))?;
        if is_last {
            // A quoted arrow exports the function itself rather than
            // running it against `null`.
            if let Term::Arr { value, .. } = &term
                && let Ok(inner) = serde_json::from_value::<Term>(value.clone())
            {
                let arrow = self.with_env(|| lower::lower(&inner))?;
                if self.dep {
                    self.tail = Some(arrow);
                } else {
                    self.stmts.push(JsAst::ExportDefault(Box::new(arrow)));
                }
                return Ok(());
            }
            if self.dep {
                // Dep mode: the trailing expression's value *is* the
                // module record (`module_prep` parity).
                self.tail = Some(applied);
            } else {
                self.stmts.push(JsAst::ExportDefault(Box::new(applied)));
            }
        } else {
            self.stmts.push(applied);
        }
        Ok(())
    }

    fn emit_export(
        &mut self,
        lang: &Language,
        ast: &Ast,
        node: &AstNode,
    ) -> Result<(), CompileError> {
        let Some(&child_idx) = children_of(node).first() else {
            return Err(CompileError::InvalidInput(
                "export: missing declaration child".to_string(),
            ));
        };
        let child = ast
            .get(AstRef::new(child_idx))
            .ok_or_else(|| CompileError::InvalidInput("export: child out of bounds".to_string()))?;
        // `export F := new Inductive { … }` lowers to a `stmt.export`
        // whose children *are* the desugared declaration group's
        // assigns (former/ctors/`F.rec`) — there is no wrapper
        // `block`/`assign` node, so the `binding_parts` probe below
        // would misread the first ctor assign as the exported binding.
        // Emit the whole group at module scope and export the family
        // name only (the family record carries its constructors —
        // `module_prep` parity).
        if self.is_decl_group_export(lang, ast, node) {
            let (names, family) = self.emit_decl_group(lang, ast, node)?;
            // The family name is the prefix of the dotted `F.rec`
            // target; fall back to the last non-dotted bound name
            // (`desugar_data` mints the family former last) when no
            // recursor assign survived.
            let family = family.or_else(|| names.last().cloned());
            if let Some(family) = family {
                self.export_names(vec![family]);
            }
            return Ok(());
        }
        if let Some((name, value_idx)) = self.binding_parts(lang, ast, child) {
            return self.emit_binding(
                lang,
                ast,
                AstRef::new(child_idx),
                child,
                &name,
                value_idx,
                true,
            );
        }
        // `export F := new Inductive { … }` (and the `F := { … }` /
        // `F(params) := c0 | c1` family-record spellings) — the
        // desugared declaration group exports the *family name only*:
        // the family record carries its constructors, so `F.Ctor`
        // reaches them importer-side (`module_prep` parity; `F.rec`
        // is dotted and cannot be a record field either way).
        if is_splice_block(child) || self.is_flattened_decl_group(lang, ast, child) {
            let (names, family) = self.emit_decl_group(lang, ast, child)?;
            let family = family.or_else(|| names.last().cloned());
            if let Some(family) = family {
                self.export_names(vec![family]);
            }
            return Ok(());
        }
        // `export x : T` — a signature export: binds `x` to `null` and
        // exports that binding (module_prep's type-only erasure).
        if child.kind == "type-annot" {
            return self.emit_signature(lang, ast, AstRef::new(child_idx), child, false, true);
        }
        // `export name` — re-export the bound name. Deferred to `finish`:
        // the name may be bound later in the file, and an unbound name is
        // a type-only signature export (`null` in dep records, a comment
        // in ESM — `export { f }` with no `f` binding is a SyntaxError).
        let name = match child.kind.as_str() {
            "var" => child.payload.as_str().map(String::from),
            _ => child
                .payload
                .get("name")
                .and_then(Value::as_str)
                .map(String::from)
                .or_else(|| {
                    children_of(child)
                        .first()
                        .and_then(|&i| var_name_of(ast, i))
                }),
        };
        match name {
            Some(n) => {
                self.pending_exports.push(n);
                Ok(())
            }
            None => Err(CompileError::InvalidInput(
                "export: cannot resolve the exported name".to_string(),
            )),
        }
    }

    /// `x : T` — a bare `type-annot` statement is a *signature*
    /// declaration, not an annotated expression: `module_prep` binds the
    /// name to `null` (type-only erasure), so emit `let x = null` and
    /// record the name in `signature_names` — a later `x := impl`
    /// reassigns it (`emit_binding`'s `Assign` arm) keeping any `export`
    /// live, matching eval's "last binding wins" record projection.
    ///
    /// A non-`var` subject (`(f x) : T`) is an annotated *expression* —
    /// lower it as an ordinary statement instead.
    #[allow(clippy::too_many_arguments)]
    fn emit_signature(
        &mut self,
        lang: &Language,
        ast: &Ast,
        root: AstRef,
        node: &AstNode,
        is_last: bool,
        export: bool,
    ) -> Result<(), CompileError> {
        let name = children_of(node)
            .first()
            .and_then(|&i| ast.get(AstRef::new(i)))
            .filter(|n| n.kind == "var")
            .and_then(|n| n.payload.as_str().map(String::from));
        let Some(name) = name else {
            return self.emit_expr_statement(lang, ast, root, is_last);
        };
        let js = js_ident(&name);
        if self.bound.contains_key(&name) {
            if self.signature_names.contains(&name) {
                // Re-signature of a signature binding — eval rebinds
                // `null`; mirror it so the live export tracks.
                self.stmts.push(JsAst::Assign {
                    name: js,
                    value: Box::new(JsAst::Null),
                });
            } else if export {
                // Signature over an implemented name — the impl stands;
                // `export x : T` still re-exports the bound value.
                self.pending_exports.push(name);
            } else {
                self.stmts.push(JsAst::Comment(format!(
                    "compile-js: signature re-declaration of `{name}` skipped \
                     (already bound)"
                )));
            }
            return Ok(());
        }
        self.bind(name.clone(), BoundRef::Arrow(js.clone()));
        self.signature_names.insert(name.clone());
        let decl = JsAst::Let {
            name: js.clone(),
            value: Box::new(JsAst::Null),
        };
        if self.dep {
            self.stmts.push(decl);
            if export {
                // The record reads the `let` at return time — a later
                // `x := impl` reassignment is what eval's `{x: x}` sees.
                self.exports.push((name, JsAst::Ident(js)));
            }
        } else {
            if export {
                self.exported_names.insert(js.clone());
                self.stmts.push(JsAst::ExportDecl(Box::new(decl)));
            } else {
                self.stmts.push(decl);
            }
        }
        Ok(())
    }

    /// Emit the statements of a desugared `data` declaration — the
    /// `__splice__`d `block` `desugar_data` mints (former, one `assign`
    /// per constructor, and the `F.rec` recursor). Each child binds at
    /// module scope exactly like a top-level statement; dotted targets
    /// (`F.rec`) become member assignments on the family binding.
    ///
    /// Returns the non-dotted bound names in order (the family/former
    /// first, then each constructor) — the export path projects only
    /// the family name (`module_prep` parity; the family record
    /// carries its constructors) — and injects nullary constructors
    /// into `lang.globals` so `match`'s ctor-value patterns (`Zero:`)
    /// see them as literals — `lower_ast`'s env is per-statement and
    /// cannot carry module bindings into `compile_pattern`'s `pat_env`.
    fn emit_splice_block(
        &mut self,
        lang: &Language,
        ast: &Ast,
        node: &AstNode,
    ) -> Result<Vec<String>, CompileError> {
        self.emit_decl_group(lang, ast, node).map(|(names, _)| names)
    }

    /// Emit the children of a desugared `data` declaration group — the
    /// `__splice__`-flagged `block` or the flattened surface `block`
    /// `splice_decl_blocks` leaves behind. Each child binds at module
    /// scope exactly like a top-level statement; dotted targets
    /// (`F.rec`) become flat consts bound under the dotted name (see
    /// the emission-order note below).
    ///
    /// Returns the non-dotted bound names in order plus the family
    /// name (the prefix of the dotted `F.rec` recursor target —
    /// `desugar_data` mints the ctor assigns first, the `F.rec`
    /// recursor next, and the family former *last*, so neither
    /// position nor the dotted target alone identifies it before the
    /// scan). Every non-dotted name except the family joins
    /// `decl_ctors` — bound for in-module use but excluded from the
    /// export surface, matching `module_prep`'s family-record export.
    /// Also injects nullary constructors into `lang.globals` so
    /// `match`'s ctor-value patterns (`Zero:`) see them as literals —
    /// `lower_ast`'s env is per-statement and cannot carry module
    /// bindings into `compile_pattern`'s `pat_env`.
    ///
    /// Emission order is not the minted order: `F.rec = …` cannot
    /// precede `const F` (TDZ), and the family record's `rec` field
    /// reads `F.rec` while `const F` is still initializing. Dotted
    /// member binds are therefore emitted as flat `const F_rec = …`
    /// bindings *before* the family former — eval binds `F.rec` as a
    /// plain scope name with no TDZ, and the family record's `rec`
    /// field plus in-module `F.rec` references resolve through
    /// `bound` to that const (the record's own `rec` property carries
    /// the same value for JS-side `F.rec` access).
    fn emit_decl_group(
        &mut self,
        lang: &Language,
        ast: &Ast,
        node: &AstNode,
    ) -> Result<(Vec<String>, Option<String>), CompileError> {
        let children = node
            .payload
            .get("children")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        // Pre-scan the binding targets: `(idx, name, value_idx,
        // dotted)` for binding children, `None` for anything else.
        let mut plan: Vec<(usize, Option<(String, usize, bool)>)> = Vec::new();
        let mut family: Option<String> = None;
        for entry in &children {
            let Some(idx) = entry.as_u64().map(|n| n as usize) else {
                continue;
            };
            let Some(child) = ast.get(AstRef::new(idx)) else {
                continue;
            };
            let info = self.binding_parts(lang, ast, child).map(|(name, value_idx)| {
                let dotted = name.contains('.');
                (name, value_idx, dotted)
            });
            if let Some((name, _, true)) = &info
                && family.is_none()
                && let Some(prefix) = name.split('.').next()
                && !prefix.is_empty()
            {
                family = Some(prefix.to_string());
            }
            plan.push((idx, info));
        }
        let is_family = |info: &Option<(String, usize, bool)>| {
            matches!(info, Some((n, _, false)) if Some(n.as_str()) == family.as_deref())
        };
        let mut names: Vec<String> = Vec::new();
        let mut ctor_globals: Vec<(String, Value)> = Vec::new();
        // Pass 1 — constructors (and defensive non-binding children)
        // in child order; the family former and dotted member binds
        // are deferred.
        for &(idx, ref info) in &plan {
            if is_family(info) || matches!(info, Some((_, _, true))) {
                continue;
            }
            let Some(child) = ast.get(AstRef::new(idx)) else {
                continue;
            };
            self.emit_decl_group_child(
                lang,
                ast,
                AstRef::new(idx),
                child,
                &mut names,
                &mut ctor_globals,
            )?;
        }
        // Pass 2 — dotted member binds (`F.rec`) as flat consts bound
        // under the dotted name, before `const F` evaluates.
        for &(idx, ref info) in &plan {
            let Some((name, value_idx, true)) = info else {
                continue;
            };
            let child_ref = AstRef::new(idx);
            let js = js_ident(name);
            self.bind(name.clone(), BoundRef::Arrow(js.clone()));
            let value = self.lower_value(lang, ast, child_ref, *value_idx)?;
            self.stmts.push(JsAst::ConstDecl {
                name: js,
                value: Box::new(value),
            });
        }
        // Pass 3 — the family former record: its ctor/`rec` fields
        // resolve through `bound` now that every referent exists.
        for &(idx, ref info) in &plan {
            if !is_family(info) {
                continue;
            }
            let Some(child) = ast.get(AstRef::new(idx)) else {
                continue;
            };
            self.emit_decl_group_child(
                lang,
                ast,
                AstRef::new(idx),
                child,
                &mut names,
                &mut ctor_globals,
            )?;
        }
        self.inject_ctor_globals(ctor_globals);
        // Every non-dotted name except the family is a constructor —
        // bound but not an export-surface name.
        for n in names
            .iter()
            .filter(|n| Some(n.as_str()) != family.as_deref())
        {
            self.decl_ctors.insert(n.clone());
        }
        Ok((names, family))
    }

    /// Emit one non-dotted child of a desugared `data` declaration
    /// group — a former/ctor `assign` (or, defensively, an expression
    /// statement). Non-dotted binding names join `names`; nullary ctor
    /// literals join `ctor_globals`.
    fn emit_decl_group_child(
        &mut self,
        lang: &Language,
        ast: &Ast,
        child_ref: AstRef,
        child: &AstNode,
        names: &mut Vec<String>,
        ctor_globals: &mut Vec<(String, Value)>,
    ) -> Result<(), CompileError> {
        if let Some((name, value_idx)) = self.binding_parts(lang, ast, child) {
            debug_assert!(!name.contains('.'), "dotted decl-group binds are emitted by emit_decl_group");
            self.emit_binding(lang, ast, child_ref, child, &name, value_idx, false)?;
            if let Some(v) = ctor_global_value(ast, value_idx) {
                ctor_globals.push((name.clone(), v));
            }
            names.push(name);
        } else {
            self.emit_expr_statement(lang, ast, child_ref, false)?;
        }
        Ok(())
    }

    /// Export each bound name — dep mode records `(name, bound_expr)`
    /// in the module record, entry mode emits the ESM surface. Used by
    /// the `export F := new Inductive { … }` family export.
    fn export_names(&mut self, names: Vec<String>) {
        for name in names {
            let Some(r) = self.bound.get(&name).cloned() else {
                continue;
            };
            if self.dep {
                if !self.exports.iter().any(|(n, _)| *n == name) {
                    self.exports.push((name, bound_expr(&r)));
                }
            } else {
                self.emit_entry_export(&name, &r);
            }
        }
    }

    /// Whether a `stmt.export` node's children are a desugared `data`
    /// declaration group — the `export F := new Inductive { … }`
    /// surface lowers to `stmt.export` wrapping the former/ctor/`F.rec`
    /// assigns *directly* (no `block`/`assign` wrapper). Every child
    /// must be a binding and at least one target must be dotted
    /// (`F.rec` — dotted names are not bindable surface syntax, so an
    /// ordinary `export` cannot produce them).
    fn is_decl_group_export(&self, lang: &Language, ast: &Ast, node: &AstNode) -> bool {
        let children = children_of(node);
        if children.len() < 2 {
            return false;
        }
        let mut saw_dotted = false;
        for &c in &children {
            let Some(child) = ast.get(AstRef::new(c)) else {
                return false;
            };
            match self.binding_parts(lang, ast, child) {
                Some((name, _)) => saw_dotted |= name.contains('.'),
                None => return false,
            }
        }
        saw_dotted
    }

    /// Whether `node` is the *flattened* form of a `data` declaration
    /// group: `apply_desugar`'s `splice_decl_blocks` splices the
    /// `__splice__`-flagged block `desugar_data` mints into any parent
    /// carrying a `statements` role — including the surface `block`
    /// a family declaration parses to — leaving an ordinary unflagged
    /// `block`
    /// whose children are the former/ctor/recursor `assign`s. The
    /// reliable marker is the dotted `F.rec` target `desugar_data`
    /// always mints last; a user block cannot produce one (dotted names
    /// are not bindable surface syntax), and requiring *every* child to
    /// be a binding keeps ordinary block expressions out.
    fn is_flattened_decl_group(&self, lang: &Language, ast: &Ast, node: &AstNode) -> bool {
        if node.kind != "block" || is_splice_block(node) {
            return false;
        }
        let children = children_of(node);
        if children.len() < 2 {
            return false;
        }
        let mut saw_dotted = false;
        for &c in &children {
            let Some(child) = ast.get(AstRef::new(c)) else {
                return false;
            };
            match self.binding_parts(lang, ast, child) {
                Some((name, _)) => saw_dotted |= name.contains('.'),
                None => return false,
            }
        }
        saw_dotted
    }

    /// Register nullary ctor values as `lang.globals` entries — see
    /// `emit_splice_block`. Lazily clones the base `Language` on first
    /// use so modules without `data` decls pay nothing. Also records
    /// each literal in `ctor_literals` so dep modules can hand them to
    /// importers (and re-export them transitively).
    fn inject_ctor_globals(&mut self, ctors: Vec<(String, Value)>) {
        if ctors.is_empty() {
            return;
        }
        let lang = self.augmented_lang.get_or_insert_with(|| self.lang.clone());
        for (name, value) in ctors {
            self.ctor_literals.insert(name.clone(), value.clone());
            lang.globals.insert(name, value);
        }
    }

    /// Splice a resolved dep's export names into scope —
    /// `import "spec"` / `from "spec" import *` bind the module record's
    /// fields as arrow-convention member paths. Nullary-ctor literals
    /// the dep bound ride along into `lang.globals` so `match`'s
    /// ctor-value patterns (`Zero:`) see imported constructors as the
    /// same literals eval's scope resolution would hand them.
    fn splice_exports(
        &mut self,
        spec: &str,
        exports: &[(String, JsAst)],
        ctors: &HashMap<String, Value>,
    ) {
        let mut injected = Vec::new();
        for (name, _) in exports {
            let local = js_ident(name);
            self.bind(
                local.clone(),
                BoundRef::DepArrow(rt_member_path(&[spec, name])),
            );
            if let Some(v) = ctors.get(name) {
                injected.push((local, v.clone()));
            }
        }
        // Family-record exports (`export F := new Inductive { … }`)
        // keep ctor names off the export surface, but the family
        // record still carries the ctor literals (`F.Zero`) — eval
        // resolves a `Zero:` pattern through the importer's scope
        // binding to that same literal, so seed `lang.globals` for
        // every ctor literal the dep bound, not just export-named
        // ones.
        for (name, v) in ctors {
            let local = js_ident(name);
            if !injected.iter().any(|(n, _)| *n == local) {
                injected.push((local, v.clone()));
            }
        }
        self.inject_ctor_globals(injected);
    }

    /// Seed `lang.globals` with a resolved dep's nullary-ctor literals
    /// for the `(exported name, local bound name)` pairs this import
    /// binds — plus every ctor literal the dep bound under its own
    /// name. Family-record exports (`export F := new Inductive { … }`)
    /// keep ctor names off the export surface, but the family record
    /// still carries them (`F.Zero`), so the literals stay reachable
    /// importer-side: eval resolves a `Zero:` pattern through the
    /// importer's scope binding (`Zero := Nat.Zero`) to the same
    /// literal `eq`-match compares against.
    fn inject_dep_ctors(
        &mut self,
        exports: &[(String, JsAst)],
        ctors: &HashMap<String, Value>,
        pairs: &[(String, String)],
    ) {
        let mut injected = Vec::new();
        for (imported, local) in pairs {
            if !exports.iter().any(|(n, _)| n == imported) {
                continue;
            }
            if let Some(v) = ctors.get(imported) {
                injected.push((local.clone(), v.clone()));
            }
        }
        for (name, v) in ctors {
            let local = js_ident(name);
            if !injected.iter().any(|(n, _)| *n == local) {
                injected.push((local, v.clone()));
            }
        }
        self.inject_ctor_globals(injected);
    }

    fn emit_import(
        &mut self,
        g: &mut Bundle,
        lang: &Language,
        ast: &Ast,
        node: &AstNode,
        text: &str,
    ) -> Result<(), CompileError> {
        // `from "<spec>" …` carries `payload.path` (payload_extract);
        // bare `import "<spec>"` keeps the spec as its string child (the
        // `import` prefix rule has no extract step).
        let names = import_names(node);
        let spec = node
            .payload
            .get("path")
            .and_then(Value::as_str)
            .map(String::from)
            .or_else(|| children_of(node).first().and_then(|&i| var_name_of(ast, i)))
            .ok_or_else(|| CompileError::InvalidInput("import: missing path".to_string()))?;
        let alias = node
            .payload
            .get("alias")
            .and_then(Value::as_str)
            .map(js_ident);
        // `payload.alias` is context-dependent — eval's `import` op
        // (`native_ops`) reads it differently per `names` shape:
        //
        // - braced `names` array (`{ a, b as x }`): `alias` is dead
        //   payload — eval's array branch never reads it, so `as m`
        //   after a list binds nothing here either;
        // - `import * as m` (or a names-less import): `alias` is the
        //   *namespace* binding — `m` sees the whole module record;
        // - `from "s" import name as local` (single non-`*` name):
        //   `alias` is the *local rename* — eval binds only `local` →
        //   the `name` export and leaves `name` itself unbound.
        let names_is_list = matches!(node.payload.get("names"), Some(Value::Array(_)));
        let single_import = match names.as_slice() {
            [(imported, _)] if !names_is_list && imported != "*" => Some(imported.clone()),
            _ => None,
        };
        let renamed = if single_import.is_some() {
            alias.clone()
        } else {
            None
        };
        let ns_alias = if names_is_list || renamed.is_some() {
            None
        } else {
            alias.clone()
        };
        // The local name a `(imported, local)` pair binds: the `as`
        // rename when this is the single-name form, else the pair's
        // own local (already `js_ident`ed by `import_names`).
        let local_of = |imported: &str, local: &str| -> String {
            match (&renamed, &single_import) {
                (Some(a), Some(s)) if s == imported => a.clone(),
                _ => local.to_string(),
            }
        };
        // `from "<spec>" export a, b` — a re-export. `import`/`export`
        // share the `stmt.import` node (the trailing keyword isn't in
        // the payload), so the tail is read from the statement text —
        // the same place `module_prep::from_export_to_import` reads it.
        let reexport = is_from_export(text);

        // The `js` contract package binds `__handl_rt` shims / the real
        // `await` op — never inlined (its file is decl-only) even when a
        // dep table resolves `js` to a path.
        if spec == JS_PACKAGE {
            for (imported, local) in names.iter().filter(|(i, _)| i != "*") {
                let bound = match imported.as_str() {
                    "await" => BoundRef::Op("await".to_string()),
                    other => BoundRef::Shim(other.to_string()),
                };
                self.bind(local_of(imported, local), bound);
            }
            if let Some(m) = &ns_alias {
                self.bind(m.clone(), BoundRef::Member("__handl_rt".to_string()));
            }
            return Ok(());
        }

        // A spec the driver resolved to a `.hnl` file: emit the dep once
        // (`const __mod_<n>` bound under `__handl_rt[<spec>]`), then bind
        // names as arrow-convention member paths — the imports become
        // real values instead of dead stubs. Prelude-owned specs
        // (`tag`) keep their shims.
        if !PRELUDE_OWNED.contains(&spec.as_str())
            && let Some(dep_path) = self.edges.get(&spec).cloned()
        {
            let DepSurface { exports, ctors } = g.ensure_module(lang, &spec, &dep_path)?;
            if reexport {
                let pairs: Vec<(String, String)> = names
                    .iter()
                    .filter(|(i, _)| i != "*")
                    .map(|(i, _)| (i.clone(), js_ident(i)))
                    .collect();
                self.inject_dep_ctors(&exports, &ctors, &pairs);
                for (imported, _) in names.iter().filter(|(i, _)| i != "*") {
                    let path = rt_member_path(&[&spec, imported]);
                    self.bind(js_ident(imported), BoundRef::DepArrow(path.clone()));
                    if self.dep {
                        self.exports.push((imported.clone(), JsAst::Raw(path)));
                    }
                }
                return Ok(());
            }
            if let Some(m) = &ns_alias {
                self.bind(m.clone(), BoundRef::Ns(rt_member_path(&[&spec])));
            }
            let star = spec.ends_with('*') || names.iter().any(|(i, _)| i == "*");
            if star {
                if ns_alias.is_none() {
                    self.splice_exports(&spec, &exports, &ctors);
                }
            } else {
                let pairs: Vec<(String, String)> = names
                    .iter()
                    .map(|(imported, local)| (imported.clone(), local_of(imported, local)))
                    .collect();
                self.inject_dep_ctors(&exports, &ctors, &pairs);
                for (imported, local) in &names {
                    self.bind(
                        local_of(imported, local),
                        BoundRef::DepArrow(rt_member_path(&[&spec, imported])),
                    );
                }
                // `import "spec"` — the splice form.
                if names.is_empty() && ns_alias.is_none() && !text.trim_start().starts_with("from")
                {
                    self.splice_exports(&spec, &exports, &ctors);
                }
            }
            return Ok(());
        }

        if reexport {
            // `from "<host>" export a` — member-path binds; in dep mode
            // the names also join the export record.
            for (imported, _) in names.iter().filter(|(i, _)| i != "*") {
                let path = rt_member_path(&[&spec, imported]);
                self.bind(js_ident(imported), BoundRef::Member(path.clone()));
                if self.dep {
                    self.exports.push((imported.clone(), JsAst::Raw(path)));
                }
            }
            return Ok(());
        }

        if is_bare_module_name(&spec) {
            // Host module (no backing file): names bind `__handl_rt`
            // member paths — prelude shims when present, dead members
            // otherwise.
            for (imported, local) in names.iter().filter(|(i, _)| i != "*") {
                self.bind(
                    local_of(imported, local),
                    BoundRef::Member(rt_member_path(&[&spec, imported])),
                );
            }
            if let Some(m) = &ns_alias {
                self.bind(m.clone(), BoundRef::Member(rt_member_path(&[&spec])));
            }
            return Ok(());
        }

        if self.dep {
            // An unresolved path spec inside an inlined module cannot
            // emit an ES `import` (illegal inside the dep's IIFE) — the
            // names keep member-path stub semantics.
            for (imported, local) in names.iter().filter(|(i, _)| i != "*") {
                self.bind(
                    local_of(imported, local),
                    BoundRef::Member(rt_member_path(&[&spec, imported])),
                );
            }
            if let Some(m) = &ns_alias {
                self.bind(m.clone(), BoundRef::Member(rt_member_path(&[&spec])));
            }
            return Ok(());
        }

        // Path specifier → a real ES import (emitted verbatim; specifier
        // resolution is the embedder's concern).
        if spec.ends_with("*") || names.iter().any(|(i, _)| i == "*") {
            let ns = ns_alias.clone().unwrap_or_else(|| "module".to_string());
            self.bind(ns.clone(), BoundRef::Ident(ns.clone()));
            self.stmts.push(JsAst::Import {
                spec,
                named: Vec::new(),
                namespace: Some(ns),
            });
            return Ok(());
        }
        let named = names
            .iter()
            .filter(|(imported, _)| imported != "*")
            .map(|(imported, local)| (imported.clone(), local_of(imported, local)))
            .collect::<Vec<_>>();
        for (_, local) in &named {
            self.bind(local.clone(), BoundRef::Arrow(local.clone()));
        }
        self.stmts.push(JsAst::Import {
            spec,
            named,
            namespace: None,
        });
        Ok(())
    }

    /// Lower an AST subtree to a `Term` — via `lower_ast` on the
    /// (possibly ctor-global-augmented) module `Language`.
    fn lower_term(&self, ast: &Ast, root: AstRef) -> Result<Term, CompileError> {
        lower_ast(ast, self.lang(), root).map_err(CompileError::Lower)
    }

    /// Run `f` with the module-bound name table visible to the lowerer.
    fn with_env<T>(&self, f: impl FnOnce() -> T) -> T {
        lower::with_module_env(self.bound.clone(), f)
    }
}

/// Whether `node` is a desugar-minted *declaration group* — the
/// `__splice__`-flagged `block` `desugar_data` (and the `.hnl` desugar
/// overlay) produces for `F := new Inductive { … }` / `F := { … }`:
/// former/constructor/recursor
/// `assign`s that must bind in the enclosing statement list, not inside
/// a nested scope (mirrors `handl-runtime`'s `splice_decl_blocks`, which
/// this per-statement walk cannot run — it re-parses each chunk).
fn is_splice_block(node: &AstNode) -> bool {
    node.kind == "block" && node.payload.get("__splice__").and_then(Value::as_bool) == Some(true)
}

/// Whether `node` is a user-written block (`do { … }`, `{ … }`) rather
/// than the surface `block` a `data` declaration desugars into. Both
/// are `block`-kinded with an identical payload shape, so the tell is
/// the statement text under the node's span: a decl group starts with
/// `data`/the family name, a user block with `do` or `{`. A missing
/// span (desugar-minted nodes carry none) or out-of-range span is *not*
/// a user block — the flatten path stays the default.
fn is_user_block(node: &AstNode, text: &str) -> bool {
    let Some(span) = node.span else {
        return false;
    };
    let Some(src) = text.get(span.start..) else {
        return false;
    };
    let t = src.trim_start();
    t.starts_with('{')
        || (t.starts_with("do")
            && t[2..]
                .chars()
                .next()
                .is_none_or(|c| !(c.is_ascii_alphanumeric() || c == '_')))
}

/// Evaluate an AST subtree as a JSON literal — the restricted shape a
/// nullary data constructor's value takes (`object`/`entry`/`array` over
/// `lit`/`value` leaves, including the `{"__sym__": …}`/`{"__ctor__": …}`
/// marker records, which are plain objects on this target). `None` on
/// anything non-literal (vars, calls, field accesses) so the caller can
/// skip the `globals` injection rather than mint a wrong value.
fn ast_literal_json(ast: &Ast, idx: usize) -> Option<Value> {
    let node = ast.get(AstRef::new(idx))?;
    match node.kind.as_str() {
        "lit" | "value" => Some(node.payload.clone()),
        "array" => {
            let items = children_of(node)
                .iter()
                .map(|&c| ast_literal_json(ast, c))
                .collect::<Option<Vec<_>>>()?;
            Some(Value::Array(items))
        }
        "object" => {
            let mut map = serde_json::Map::new();
            for &e in &children_of(node) {
                let entry = ast.get(AstRef::new(e))?;
                if entry.kind != "entry" {
                    return None;
                }
                let ek = children_of(entry);
                let (Some(&k), Some(&v)) = (ek.first(), ek.get(1)) else {
                    return None;
                };
                let key_node = ast.get(AstRef::new(k))?;
                let key = match key_node.kind.as_str() {
                    "var" | "lit" | "value" => key_node.payload.as_str()?.to_string(),
                    _ => return None,
                };
                map.insert(key, ast_literal_json(ast, v)?);
            }
            Some(Value::Object(map))
        }
        _ => None,
    }
}

/// The `lang.globals` value for a nullary constructor binding — `Some`
/// only when the assign's value is a literal ctor object (an `object`
/// carrying `__ctor__`; the family *former* carries `__former__` and is
/// skipped). Arity-carrying constructors lower to `function` nodes and
/// never reach this — their `match` patterns dispatch on `ctor_name`,
/// which needs no env entry.
fn ctor_global_value(ast: &Ast, value_idx: usize) -> Option<Value> {
    let v = ast_literal_json(ast, value_idx)?;
    v.as_object()?.contains_key("__ctor__").then_some(v)
}
