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
use handl_language::{Ast, AstNode, AstRef, Language};
use handl_plugin_compile::{CompileError, CompilerInput};
use handl_plugin_ext_diag::split_statements;
use hnl_engine::Term;
use hnl_engine_lex_pattern::lex;
use hnl_engine_parse::{ParserEngine, ParserRequest};
use hnl_engine_parse_pratt::PrattEngine;
use serde_json::Value;

use crate::config::JsConfig;
use crate::emit::{JsEmitter, SecurityScanner};
use crate::lower::{self, BoundRef, JsAst};

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
    /// `import "spec"` / `from "spec" import *` splices).
    emitted: HashMap<String, (String, Vec<(String, JsAst)>)>,
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
    /// imports. A dep that fails to emit degrades to an empty namespace
    /// plus a marker comment — preserving the old dead-member-path
    /// semantics — rather than failing the whole compile; a circular
    /// import errors (the same error `ModuleLoader` reports at eval
    /// time).
    fn ensure_module(
        &mut self,
        lang: &Language,
        spec: &str,
        path: &str,
    ) -> Result<Vec<(String, JsAst)>, CompileError> {
        if let Some((ns, exports)) = self.emitted.get(path) {
            let (ns, exports) = (ns.clone(), exports.clone());
            self.bind_spec(spec, &ns);
            return Ok(exports);
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
        let (stmts, exports) = match body {
            Ok(emit) => {
                let mut stmts = emit.stmts;
                stmts.push(JsAst::Return(Box::new(
                    emit.tail
                        .unwrap_or_else(|| JsAst::Object(emit.exports.clone())),
                )));
                (stmts, emit.exports)
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
                (Vec::new(), Vec::new())
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
        self.emitted
            .insert(path.to_string(), (ident.clone(), exports.clone()));
        self.bind_spec(spec, &ident);
        Ok(exports)
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
            match self.bound.get(&name) {
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
                .filter(|n| !n.contains('.'))
                .filter_map(|n| self.bound.get(n).map(|r| (n.clone(), bound_expr(r))))
                .collect()
        } else {
            self.exports
        };
        BodyEmit {
            stmts: self.stmts,
            exports,
            tail: self.tail,
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
pub(crate) fn emit_module_surface(
    stmts: &[JsAst],
    format_name: &str,
    prelude: &str,
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
    SecurityScanner::new().scan(&body)?;
    SecurityScanner::new().scan(&surface_js)?;
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
        .map(|(imported, local)| {
            let key = serde_json::to_string(&Value::String(imported.clone()))
                .unwrap_or_else(|_| "\"_\"".to_string());
            format!("{key}: {local}")
        })
        .collect::<Vec<_>>()
        .join(", ");
    let spec_lit = serde_json::to_string(&Value::String(spec.to_string()))
        .unwrap_or_else(|_| "\"_\"".to_string());
    JsAst::Raw(format!("const {{ {props} }} = require({spec_lit})"))
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
    // between parse and lowering — `normalize_family_records` (both
    // `data F :: …` and the `F := { … }` surface spellings become `data`
    // nodes), the `desugar` transform pass, and `splice_decl_blocks`
    // (a root-level `__splice__` block stays flagged — `emit_statement`
    // walks its children as module-scope bindings).
    let ast = handl_runtime::desugar_pass::apply_desugar(lang, &ast)
        .map_err(|e| format!("desugar: {e}"))?;
    if ast.is_empty() {
        return Ok((ast, AstRef::new(0)));
    }
    let len = ast.len();
    Ok((ast, AstRef::new(len - 1)))
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

/// Whether `s` is a valid JS identifier (so `.s` member access is legal).
fn is_js_ident(s: &str) -> bool {
    !s.is_empty()
        && s.chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic() || c == '_' || c == '$')
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '$')
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
            let quoted = serde_json::to_string(&Value::String(seg.to_string()))
                .unwrap_or_else(|_| "\"_\"".to_string());
            path.push('[');
            path.push_str(&quoted);
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
            // `data F := { … }` / `data F :: …` desugar to a declaration
            // group of former/ctor/recursor `assign`s. Two root shapes
            // reach emit: the `__splice__`-flagged `block` itself (no
            // enclosing statement list to splice into), or — because
            // `apply_desugar`'s `splice_decl_blocks` flattens flagged
            // blocks into *any* parent with a `statements` role — the
            // unflagged surface `block` the assigns were spliced into.
            _ if is_splice_block(node) => self.emit_splice_block(lang, ast, node).map(|_| ()),
            _ if self.is_flattened_decl_group(lang, ast, node) => {
                self.emit_splice_block(lang, ast, node).map(|_| ())
            }
            // `export data F :: …` parses as `export <var data>` spliced
            // into the family decl group — there is no `export data`
            // form; the block would lower to a nonsense `export(data)`
            // call. Fail loudly: export the family by name instead
            // (`data F :: …` then `export F`).
            _ if node.kind == "block"
                && children_of(node).iter().any(|&c| {
                    ast.get(AstRef::new(c))
                        .is_some_and(|n| n.kind == "stmt.export")
                }) =>
            {
                Err(CompileError::InvalidInput(
                    "`export data …` is not a supported declaration form — \
                     declare the family, then `export F` (or the ctor names)"
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
        // `export data F := { … }` / `export data F :: …` — the desugared
        // declaration group exports the family name (module_prep
        // projects the bound `F`, not the constructors).
        if is_splice_block(child) || self.is_flattened_decl_group(lang, ast, child) {
            let family = self.emit_splice_block(lang, ast, child)?;
            if let Some(name) = family
                && let Some(r) = self.bound.get(&name).cloned()
            {
                if self.dep {
                    self.exports.push((name, bound_expr(&r)));
                } else {
                    self.emit_entry_export(&name, &r);
                }
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
    /// Returns the first bound name (the family/former) for
    /// `export data` handling, and injects nullary constructors into
    /// `lang.globals` so `match`'s ctor-value patterns (`Zero:`) see
    /// them as literals — `lower_ast`'s env is per-statement and cannot
    /// carry module bindings into `compile_pattern`'s `pat_env`.
    fn emit_splice_block(
        &mut self,
        lang: &Language,
        ast: &Ast,
        node: &AstNode,
    ) -> Result<Option<String>, CompileError> {
        let children = node
            .payload
            .get("children")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let mut family: Option<String> = None;
        let mut ctor_globals: Vec<(String, Value)> = Vec::new();
        for entry in children {
            let Some(idx) = entry.as_u64().map(|n| n as usize) else {
                continue;
            };
            let Some(child) = ast.get(AstRef::new(idx)) else {
                continue;
            };
            let child_ref = AstRef::new(idx);
            if let Some((name, value_idx)) = self.binding_parts(lang, ast, child) {
                if family.is_none() {
                    family = Some(name.clone());
                }
                if name.contains('.') {
                    self.emit_member_binding(lang, ast, child_ref, &name, value_idx)?;
                } else {
                    self.emit_binding(lang, ast, child_ref, child, &name, value_idx, false)?;
                    if let Some(v) = ctor_global_value(ast, value_idx) {
                        ctor_globals.push((name, v));
                    }
                }
            } else {
                self.emit_expr_statement(lang, ast, child_ref, false)?;
            }
        }
        self.inject_ctor_globals(ctor_globals);
        Ok(family)
    }

    /// Whether `node` is the *flattened* form of a `data` declaration
    /// group: `apply_desugar`'s `splice_decl_blocks` splices the
    /// `__splice__`-flagged block `desugar_data` mints into any parent
    /// carrying a `statements` role — including the surface `block`
    /// `data F :: …` parses to — leaving an ordinary unflagged `block`
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

    /// `F.rec = <value>` — a dotted binding target minted by
    /// `desugar_data` for the family recursor. Emits a member assignment
    /// on the (already-bound) family name and binds `F.rec` to the
    /// member path so dep export records and `Op` refs resolve it.
    fn emit_member_binding(
        &mut self,
        lang: &Language,
        ast: &Ast,
        root: AstRef,
        name: &str,
        value_idx: usize,
    ) -> Result<(), CompileError> {
        let path = name.split('.').map(js_ident).collect::<Vec<_>>().join(".");
        self.bind(name.to_string(), BoundRef::Member(path.clone()));
        let value = self.lower_value(lang, ast, root, value_idx)?;
        self.stmts.push(JsAst::Assign {
            name: path,
            value: Box::new(value),
        });
        Ok(())
    }

    /// Register nullary ctor values as `lang.globals` entries — see
    /// `emit_splice_block`. Lazily clones the base `Language` on first
    /// use so modules without `data` decls pay nothing.
    fn inject_ctor_globals(&mut self, ctors: Vec<(String, Value)>) {
        if ctors.is_empty() {
            return;
        }
        let lang = self.augmented_lang.get_or_insert_with(|| self.lang.clone());
        for (name, value) in ctors {
            lang.globals.insert(name, value);
        }
    }

    /// Splice a resolved dep's export names into scope —
    /// `import "spec"` / `from "spec" import *` bind the module record's
    /// fields as arrow-convention member paths.
    fn splice_exports(&mut self, spec: &str, exports: &[(String, JsAst)]) {
        for (name, _) in exports {
            let local = js_ident(name);
            self.bind(local, BoundRef::DepArrow(rt_member_path(&[spec, name])));
        }
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
        let names = import_names(node);
        // `from "<spec>" export a, b` — a re-export. `import`/`export`
        // share the `stmt.import` node (the trailing keyword isn't in
        // the payload), so the tail is read from the statement text —
        // the same place `module_prep::from_export_to_import` reads it.
        let reexport = is_from_export(text);

        // The `js` contract package binds `__handl_rt` shims / the real
        // `await` op — never inlined (its file is decl-only) even when a
        // dep table resolves `js` to a path.
        if spec == JS_PACKAGE {
            for (imported, local) in &names {
                let bound = match imported.as_str() {
                    "await" => BoundRef::Op("await".to_string()),
                    other => BoundRef::Shim(other.to_string()),
                };
                self.bind(local.clone(), bound);
            }
            if let Some(m) = &alias {
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
            let exports = g.ensure_module(lang, &spec, &dep_path)?;
            if reexport {
                for (imported, _) in names.iter().filter(|(i, _)| i != "*") {
                    let path = rt_member_path(&[&spec, imported]);
                    self.bind(js_ident(imported), BoundRef::DepArrow(path.clone()));
                    if self.dep {
                        self.exports.push((imported.clone(), JsAst::Raw(path)));
                    }
                }
                return Ok(());
            }
            if let Some(m) = &alias {
                self.bind(m.clone(), BoundRef::Ns(rt_member_path(&[&spec])));
            }
            let star = spec.ends_with('*') || names.iter().any(|(i, _)| i == "*");
            if star {
                if alias.is_none() {
                    self.splice_exports(&spec, &exports);
                }
            } else {
                for (imported, local) in &names {
                    self.bind(
                        local.clone(),
                        BoundRef::DepArrow(rt_member_path(&[&spec, imported])),
                    );
                }
                // `import "spec"` — the splice form.
                if names.is_empty() && alias.is_none() && !text.trim_start().starts_with("from") {
                    self.splice_exports(&spec, &exports);
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
            for (imported, local) in &names {
                self.bind(
                    local.clone(),
                    BoundRef::Member(rt_member_path(&[&spec, imported])),
                );
            }
            if let Some(m) = &alias {
                self.bind(m.clone(), BoundRef::Member(rt_member_path(&[&spec])));
            }
            return Ok(());
        }

        if self.dep {
            // An unresolved path spec inside an inlined module cannot
            // emit an ES `import` (illegal inside the dep's IIFE) — the
            // names keep member-path stub semantics.
            for (imported, local) in &names {
                self.bind(
                    local.clone(),
                    BoundRef::Member(rt_member_path(&[&spec, imported])),
                );
            }
            if let Some(m) = &alias {
                self.bind(m.clone(), BoundRef::Member(rt_member_path(&[&spec])));
            }
            return Ok(());
        }

        // Path specifier → a real ES import (emitted verbatim; specifier
        // resolution is the embedder's concern).
        if spec.ends_with("*") || names.iter().any(|(i, _)| i == "*") {
            let ns = alias.clone().unwrap_or_else(|| "module".to_string());
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
            .map(|(imported, local)| (imported.clone(), local.clone()))
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
/// overlay) produces for `data F := { … }`: former/constructor/recursor
/// `assign`s that must bind in the enclosing statement list, not inside
/// a nested scope (mirrors `handl-runtime`'s `splice_decl_blocks`, which
/// this per-statement walk cannot run — it re-parses each chunk).
fn is_splice_block(node: &AstNode) -> bool {
    node.kind == "block" && node.payload.get("__splice__").and_then(Value::as_bool) == Some(true)
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
