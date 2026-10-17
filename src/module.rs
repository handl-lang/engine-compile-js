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

use std::collections::HashMap;

use handl_kit_arrow_lower::lower_ast;
use handl_language::{Ast, AstNode, AstRef, Language};
use handl_plugin_compile::{CompileError, CompilerInput};
use handl_plugin_engine_lex_pattern::lex;
use handl_plugin_engine_parse::{ParserEngine, ParserRequest};
use handl_plugin_engine_parse_pratt::PrattEngine;
use handl_plugin_engine_transform_rewrite::RewriteEngine;
use handl_plugin_ext_diag::split_statements;
use plugin_engine::Term;
use serde_json::Value;

use crate::lower::{self, BoundRef, JsAst};

/// The `js` contract package (`handl-packages/js`): bare-name imports
/// bind to `__handl_rt` shims / the real `await` op.
const JS_PACKAGE: &str = "js";

/// Node kinds that are compile-time declarations with no runtime value —
/// they emit a comment, not a statement. `data` is listed too: its
/// desugared `block` binds constructors in a private env the per-statement
/// lowering cannot lift to module scope yet (stub, see report).
const DECL_ONLY_KINDS: &[&str] = &[
    "cap", "handle", "trait", "impl", "type", "law", "data", "sort", "grade",
    "rewrite", "infer",
];

/// Try to compile `input` as a whole `.hnl` module.
///
/// Returns `Ok(None)` when the input is not a parseable module — e.g. the
/// synthetic `CompilerInput`s tests build with an empty `Language` — so
/// the caller falls back to the single-term emit path.
pub(crate) fn try_compile_module(input: &CompilerInput) -> Result<Option<Vec<JsAst>>, CompileError> {
    let lang = &input.language;
    if lang.lexer_rules.is_empty() || lang.grammar.is_empty() {
        return Ok(None);
    }
    let chunks = split_statements(&input.source);
    if chunks.is_empty() {
        return Ok(Some(Vec::new()));
    }

    let mut ctx = ModuleCtx {
        bound: HashMap::new(),
        stmts: Vec::new(),
    };
    // Ambient host packages visible without an import (the canonical
    // module chain): `var tag` resolves to the `__handl_rt.tag` package
    // object, not a call.
    ctx.bound.insert(
        "tag".to_string(),
        BoundRef::Member("__handl_rt.tag".to_string()),
    );
    let last = chunks.len() - 1;
    for (i, chunk) in chunks.iter().enumerate() {
        let (ast, root) = parse_statement(lang, &chunk.text)
            .map_err(|e| CompileError::InvalidInput(format!("statement {}: {e}", i + 1)))?;
        let node = ast
            .get(root)
            .ok_or_else(|| CompileError::InvalidInput("statement root missing".to_string()))?;
        ctx.emit_statement(lang, &ast, root, node, i == last)?;
    }
    Ok(Some(ctx.stmts))
}

struct ModuleCtx {
    /// Module-scope name table for `lower::with_module_env`.
    bound: HashMap<String, BoundRef>,
    stmts: Vec<JsAst>,
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
    // Mirror `Runtime::parse`: the "desugar" transform pass runs between
    // parse and lowering so `for`/`do-while`/`opt-chain`/`data` reach the
    // lowerer in desugared form.
    let root = AstRef::new(ast.len() - 1);
    let out = RewriteEngine::new()
        .with_desugar_natives()
        .run_pass(lang, &ast, root, "desugar")
        .map_err(|e| format!("desugar: {e}"))?;
    let ast = out.ast;
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
                    let local = item
                        .get("as")
                        .and_then(Value::as_str)
                        .unwrap_or(name);
                    Some((name.to_string(), js_ident(local)))
                }
                _ => None,
            })
            .collect(),
        _ => Vec::new(),
    }
}

impl ModuleCtx {
    fn emit_statement(
        &mut self,
        lang: &Language,
        ast: &Ast,
        root: AstRef,
        node: &AstNode,
        is_last: bool,
    ) -> Result<(), CompileError> {
        match node.kind.as_str() {
            "stmt.import" => self.emit_import(node),
            "stmt.export" => self.emit_export(lang, ast, node),
            kind if DECL_ONLY_KINDS.contains(&kind) => {
                self.stmts.push(JsAst::Comment(format!(
                    "compile-js: `{kind}` declaration has no runtime emit (skipped)"
                )));
                Ok(())
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
    fn binding_parts(
        &self,
        lang: &Language,
        ast: &Ast,
        node: &AstNode,
    ) -> Option<(String, usize)> {
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
            let name = var_name_of(ast, target_idx)?;
            let value_idx = child_at(node, lang, "value", 1)?;
            return Some((name, value_idx));
        }
        if node.kind == "bind" {
            if let Some(name) = node.payload.get("name").and_then(Value::as_str) {
                if let Some(&value_idx) = children_of(node).first() {
                    return Some((name.to_string(), value_idx));
                }
            }
        }
        None
    }

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
        self.bound
            .insert(name.to_string(), BoundRef::Arrow(js_name.clone()));
        let value = self.lower_value(lang, ast, root, value_idx)?;
        let decl = JsAst::ConstDecl {
            name: js_name,
            value: Box::new(value),
        };
        self.stmts.push(if export {
            JsAst::ExportDecl(Box::new(decl))
        } else {
            decl
        });
        Ok(())
    }

    /// Lower a statement's value child to the JS initializer: a quoted
    /// arrow (`Arr` holding a serialized `Term`) unquotes to the body's
    /// `async __in => ...`; any other term is applied to `null` and
    /// awaited (top-level `await` is legal in ESM).
    fn lower_value(
        &mut self,
        lang: &Language,
        ast: &Ast,
        _root: AstRef,
        value_idx: usize,
    ) -> Result<JsAst, CompileError> {
        let term = self.lower_term(lang, ast, AstRef::new(value_idx))?;
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
        lang: &Language,
        ast: &Ast,
        root: AstRef,
        is_last: bool,
    ) -> Result<(), CompileError> {
        let term = self.lower_term(lang, ast, root)?;
        let applied = self.with_env(|| lower::lower_applied(&term, &JsAst::Null))?;
        if is_last {
            // A quoted arrow exports the function itself rather than
            // running it against `null`.
            if let Term::Arr { value, .. } = &term {
                if let Ok(inner) = serde_json::from_value::<Term>(value.clone()) {
                    let arrow = self.with_env(|| lower::lower(&inner))?;
                    self.stmts.push(JsAst::ExportDefault(Box::new(arrow)));
                    return Ok(());
                }
            }
            self.stmts.push(JsAst::ExportDefault(Box::new(applied)));
        } else {
            self.stmts.push(applied);
        }
        Ok(())
    }

    fn emit_export(&mut self, lang: &Language, ast: &Ast, node: &AstNode) -> Result<(), CompileError> {
        let Some(&child_idx) = children_of(node).first() else {
            return Err(CompileError::InvalidInput(
                "export: missing declaration child".to_string(),
            ));
        };
        let child = ast
            .get(AstRef::new(child_idx))
            .ok_or_else(|| CompileError::InvalidInput("export: child out of bounds".to_string()))?;
        if let Some((name, value_idx)) = self.binding_parts(lang, ast, child) {
            return self.emit_binding(lang, ast, AstRef::new(child_idx), child, &name, value_idx, true);
        }
        // `export name` / `export name: T` — re-export the bound name.
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
                let js = js_ident(&n);
                self.bound.insert(n, BoundRef::Arrow(js.clone()));
                self.stmts
                    .push(JsAst::ExportNames(vec![(js.clone(), js)]));
                Ok(())
            }
            None => Err(CompileError::InvalidInput(
                "export: cannot resolve the exported name".to_string(),
            )),
        }
    }

    fn emit_import(&mut self, node: &AstNode) -> Result<(), CompileError> {
        let spec = node
            .payload
            .get("path")
            .and_then(Value::as_str)
            .ok_or_else(|| CompileError::InvalidInput("import: missing path".to_string()))?
            .to_string();
        let alias = node
            .payload
            .get("alias")
            .and_then(Value::as_str)
            .map(|a| js_ident(a));
        let names = import_names(node);

        if is_bare_module_name(&spec) {
            // Host contract package — names resolve via `__handl_rt` /
            // the real `await` op; no ES import is emitted (a bare
            // specifier would not resolve in Node anyway).
            if spec == JS_PACKAGE {
                for (imported, local) in &names {
                    let bound = match imported.as_str() {
                        "await" => BoundRef::Op("await".to_string()),
                        other => BoundRef::Shim(other.to_string()),
                    };
                    self.bound.insert(local.clone(), bound);
                }
                if let Some(m) = &alias {
                    self.bound.insert(m.clone(), BoundRef::Member("__handl_rt".to_string()));
                }
            } else {
                for (imported, local) in &names {
                    self.bound.insert(
                        local.clone(),
                        BoundRef::Member(rt_member_path(&[&spec, imported])),
                    );
                }
                if let Some(m) = &alias {
                    self.bound
                        .insert(m.clone(), BoundRef::Member(rt_member_path(&[&spec])));
                }
            }
            return Ok(());
        }

        // Path specifier → a real ES import (emitted verbatim; specifier
        // resolution is the embedder's concern).
        if spec.ends_with("*") || names.iter().any(|(i, _)| i == "*") {
            let ns = alias.clone().unwrap_or_else(|| "module".to_string());
            self.bound.insert(ns.clone(), BoundRef::Ident(ns.clone()));
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
            self.bound.insert(local.clone(), BoundRef::Arrow(local.clone()));
        }
        self.stmts.push(JsAst::Import {
            spec,
            named,
            namespace: None,
        });
        Ok(())
    }

    fn lower_term(&self, lang: &Language, ast: &Ast, root: AstRef) -> Result<Term, CompileError> {
        lower_ast(ast, lang, root).map_err(CompileError::Lower)
    }

    /// Run `f` with the module-bound name table visible to the lowerer.
    fn with_env<T>(&self, f: impl FnOnce() -> T) -> T {
        lower::with_module_env(self.bound.clone(), f)
    }
}
