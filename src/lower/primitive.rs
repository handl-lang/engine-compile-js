//! Lower the primitive `Term` combinators (`Arr`, `Const`, `Op`, `Seq`) to JS.

use handl_plugin_compile::CompileError;
use hnl_engine::{ArrSource, Term};
use serde_json::Value;

use super::lower_applied as lower_sub;
use super::{
    BoundRef, JsAst, JsBinOp, JsUnaryOp, allow_inlining, arrow_fn, dispatch_all_ops, input_ident,
    lower_value, module_bound, op_lowering, rt_call, rt_member, unsafe_inline_foreign,
};
use crate::config::OpLowering;

/// Lower one of the four primitive terms to a JS expression.
pub fn lower(term: &Term) -> Result<JsAst, CompileError> {
    match term {
        Term::Arr { value, .. } => {
            // A quoted arrow (`Arr` holding a serialized `Term` — the
            // ADR-0120 Phase 1 quoting form) is a *function value* on this
            // target: all arrow values are JS async arrows, so `arr(q)`
            // is the constant arrow returning that function.
            if let Ok(inner) = serde_json::from_value::<Term>(value.clone()) {
                let arrow = super::lower(&inner)?;
                return Ok(arrow_fn(arrow));
            }
            let body = lower_value(value)?;
            Ok(arrow_fn(body))
        }
        Term::Const { value } => lower_value(value),
        Term::Seq { axis, fst, snd } => {
            check_sequential(axis, fst)?;
            let body = lower_seq_body(fst, snd, &input_ident())?;
            Ok(arrow_fn(body))
        }
        Term::Op {
            name,
            args,
            source,
            lit,
            dim,
            ..
        } => lower_op(
            name,
            args,
            source.clone(),
            lit.as_ref(),
            dim_label(dim).as_deref(),
        ),
        _ => Err(CompileError::Lower(format!(
            "primitive lowerer received non-primitive term: {term:?}"
        ))),
    }
}

/// The op's named-dimension label for handler dispatch — `dim.dim_label()`
/// covers the literal `Const "S"`/`Arr "S"` forms; compound dim terms
/// (which eval would evaluate to a label) resolve to `None` here, the
/// "unnamed dimension — every handler eligible" case.
fn dim_label(dim: &Term) -> Option<String> {
    dim.dim_label()
}

/// Op names eligible for `JsConfig::dispatch_all_ops` routing through
/// `__handl_rt.dispatch`: pure functions of their evaluated argument
/// terms — no `lit` payload, no incoming-value read. Everything else
/// (lit-carrying ops, `param`-reading ops, data constructors) keeps its
/// dedicated emit even under the flag.
const DISPATCHABLE_OPS: &[&str] = &[
    "add", "sub", "mul", "div", "mod", "neg", "not", "lt", "lte", "gt", "gte", "eq", "neq", "and",
    "or", "pow", "concat",
];

/// Whether a bare `Op{name, []}` resolves as a lazy *reference* — a
/// module-bound name or a non-builtin op name (the
/// [`lower_bound_op_body`]/`__opRef` outcomes) — rather than one of the
/// dedicated input-reading op arms (`id`, `tag`, `elem`, `add`, …).
/// Mirrors arrow-lower's `is_lazy_op_ref` minus its `id`/`elem`/`flow`
/// exclusions: those names have dedicated arms here that already read
/// `param`, so they are never lazy references on this target.
pub fn is_lazy_op_ref(name: &str) -> bool {
    super::module_bound(name).is_some() || !BUILTIN_OPS.contains(&name)
}

/// `__handl_rt.dispatch("name", [args], dim, incoming)` — eval's
/// `dispatch_op` fallback for `Native` ops: covering handler impls
/// (grades-filtered stack scan, then the incoming value's own `impls`)
/// apply first; uncovered ops fall back to the `__handl_rt.<name>`
/// member, then "unsupported op".
fn dispatch_call(
    name: &str,
    lowered_args: Vec<JsAst>,
    dim_label: Option<&str>,
    incoming: &JsAst,
) -> JsAst {
    rt_call(
        "dispatch",
        vec![
            JsAst::Str(name.to_string()),
            JsAst::Array(lowered_args),
            dim_label
                .map(|d| JsAst::Str(d.to_string()))
                .unwrap_or(JsAst::Null),
            incoming.clone(),
        ],
    )
}

/// Op names the calculus/lowerer reserves — a module binding under one
/// of these names still resolves as the builtin in argument position.
const BUILTIN_OPS: &[&str] = &[
    "id",
    "tag",
    "pair_tag",
    "loop_outcome",
    "arr_len",
    "arr_drop",
    "match_fail",
    "is_obj",
    "has_field",
    "tag_fields",
    "obj_except",
    "obj_key",
    "record_merge",
    "flow",
    "ctor_name",
    "ctor_fields",
    "paren",
    "rest",
    "elem",
    "product",
    "array",
    "record",
    "field_access",
    "eq",
    "neq",
    "and",
    "or",
    "not",
    "neg",
    "pow",
    "mod",
    "lt",
    "lte",
    "gt",
    "gte",
    "add",
    "sub",
    "mul",
    "div",
    "concat",
    "json",
    "proj_env",
    "await",
    "assert",
    "runtime_assert",
    "export",
    "import",
    "extend_env",
    "apply",
    "app",
    "resolve",
    "eval_template",
    "match_shell",
    "loop_step",
    "object",
    "fanout",
    "sum",
    "choice",
    "left",
    "right",
    "first",
    "second",
    "assign",
    "desugar_error",
    "proj_child",
    "proj_child_or",
    "proj_rest",
    "compare",
];

/// Lower the application of a primitive term to an input value.
pub fn lower_applied(term: &Term, input: &JsAst) -> Result<JsAst, CompileError> {
    match term {
        Term::Arr { value, .. } => {
            // `arr(q)` applied to any input yields `q`; a `q` that is a
            // serialized `Term` is a quoted arrow — its JS value is the
            // async arrow itself (the `Term::App` unquote convention).
            if let Ok(inner) = serde_json::from_value::<Term>(value.clone()) {
                return super::lower(&inner);
            }
            lower_value(value)
        }
        Term::Const { value } => lower_value(value),
        // Applying an `Op` arrow to `input` evaluates the op body with
        // `input` as the incoming value — emit the body against `input`
        // directly (awaited: op bodies may themselves contain `await`).
        Term::Op {
            name,
            args,
            source,
            lit,
            dim,
            ..
        } => Ok(JsAst::Await(Box::new(lower_op_body(
            name,
            args,
            source.clone(),
            lit.as_ref(),
            dim_label(dim).as_deref(),
            input,
            false,
        )?))),
        Term::Seq { axis, fst, snd } => {
            check_sequential(axis, fst)?;
            lower_seq_body(fst, snd, input)
        }
        _ => Err(CompileError::Lower(format!(
            "primitive lowerer received non-primitive term: {term:?}"
        ))),
    }
}

/// ADR-0120 Phase 5: `Seq{axis: k}` is `∘ₖ`. The JS backend lowers only
/// the sequential regime — `∘₀` on 1-cells (`dim(fst) ≤ 1`). The
/// globular pasting regime (`k < dim(fst)` — including axis 0 on cell
/// operands) needs the `Cell2` value model, which generated JS does not
/// carry; those terms are rejected here rather than silently compiled
/// to sequencing.
fn check_sequential(axis: &Term, fst: &Term) -> Result<(), CompileError> {
    match axis.nat_literal() {
        Some(0) if fst.globular_dim() <= 1 => Ok(()),
        Some(k) if k >= fst.globular_dim() => Err(CompileError::Lower(format!(
            "Seq: axis-{k} composition requires cells of dimension > {k} (fst has dim {})",
            fst.globular_dim()
        ))),
        _ => Err(CompileError::Lower(
            "Seq: globular cell composition is not supported by the JS backend".to_string(),
        )),
    }
}

/// `fst >>> snd`: evaluate `fst`'s result once into a `const` when it
/// is not a trivially-reusable expression, then run `snd` on the name.
///
/// Without the binding, `snd`'s lowering clones the *expression* into
/// every input position it touches (branch `cond` + both arms, `product`
/// args, …), so a stage like `fetch(...)` or `pair_tag(product(...))`
/// would re-evaluate per use. The IIFE keeps statement context legal and
/// preserves the source's eager evaluation order — the `const` runs at
/// exactly the point the `Seq` stage runs.
fn lower_seq_body(fst: &Term, snd: &Term, input: &JsAst) -> Result<JsAst, CompileError> {
    let fst_js = lower_sub(fst, input)?;
    if matches!(
        fst_js,
        JsAst::Ident(_) | JsAst::Null | JsAst::Bool(_) | JsAst::Num(_) | JsAst::Str(_)
    ) {
        return lower_sub_scheme(snd, &fst_js);
    }
    let name = super::next_tmp();
    let body = lower_sub_scheme(snd, &JsAst::Ident(name.clone()))?;
    Ok(JsAst::Await(Box::new(JsAst::Call {
        callee: Box::new(JsAst::ArrowFn {
            params: Vec::new(),
            body: Box::new(JsAst::Block(vec![
                JsAst::ConstDecl {
                    name,
                    value: Box::new(fst_js),
                },
                JsAst::Return(Box::new(body)),
            ])),
        }),
        args: Vec::new(),
    })))
}

/// `lower_applied` with the *scheme* reading for `Term::Op`: a bare
/// `Op{name, []}` lazy reference is a body arrow here — `Seq.snd` is
/// the stage the pipeline feeds the `fst` result to, so the named
/// binding/op applies to it (`x >>> s1` runs `s1(x)`, not the `s1`
/// arrow value). Other term kinds delegate to [`lower_applied`] —
/// their own subterm positions keep the referent reading (a `Choice`
/// arm or `App` arg naming an arrow yields the arrow, matching eval).
pub fn lower_sub_scheme(term: &Term, input: &JsAst) -> Result<JsAst, CompileError> {
    match term {
        Term::Op { .. } => lower_applied_scheme(term, input),
        _ => lower_sub(term, input),
    }
}

/// `lower_applied` for a `Term::Op` with `scheme = true` — see
/// [`lower_op_body`]. Awaited like the value-reading path.
pub fn lower_applied_scheme(term: &Term, input: &JsAst) -> Result<JsAst, CompileError> {
    match term {
        Term::Op {
            name,
            args,
            source,
            lit,
            dim,
            ..
        } => Ok(JsAst::Await(Box::new(lower_op_body(
            name,
            args,
            source.clone(),
            lit.as_ref(),
            dim_label(dim).as_deref(),
            input,
            true,
        )?))),
        _ => lower_sub(term, input),
    }
}

fn lower_op(
    name: &str,
    args: &[Term],
    source: ArrSource,
    lit: Option<&Value>,
    dim_label: Option<&str>,
) -> Result<JsAst, CompileError> {
    let param = input_ident();
    // `lower` produces the term's *arrow*: a bare `Op{name, []}` lazy
    // reference is a body scheme — it applies the named binding to the
    // incoming (`in ↦ s1(in)`), not a constant arrow returning the
    // reference.
    let body = lower_op_body(name, args, source, lit, dim_label, &param, true)?;
    Ok(arrow_fn(body))
}

/// The body of an `Op` arrow evaluated against the incoming value
/// `param` — `lower_op` wraps it in `async __in => …`; `lower_applied`
/// passes the concrete input expression instead. `dim_label` is the
/// op's named-dimension label (`Term::Op::dim` resolved to a literal
/// string) forwarded to handler dispatch for `grades` coverage.
///
/// `scheme` selects the *application* reading of a bare `Op{name, []}`
/// lazy reference: in scheme position (`Seq.snd`, quoted-arrow bodies —
/// anywhere the term is consumed as an arrow applied to `param`) the
/// named binding/op is applied to the incoming via
/// `__handl_rt.__appOrValue`; in value position (op/app/record args —
/// anywhere the term is read as data) it stays the referent itself.
/// `lower_op` passes `true` (it builds the term's arrow); `lower_applied`
/// passes `false` (argument positions want the referent).
fn lower_op_body(
    name: &str,
    args: &[Term],
    source: ArrSource,
    lit: Option<&Value>,
    dim_label: Option<&str>,
    param: &JsAst,
    scheme: bool,
) -> Result<JsAst, CompileError> {
    // `await` is the `js.await` promise bridge (`handl-packages/js`): it
    // lowers to a real JS `await` inside the (async) arrow body, handing
    // the promise to the host event loop for settlement. This holds for
    // both `Native` and `Foreign` spellings of the op — `await` is not a
    // host function to inject, it is the target's own boundary.
    if name == "await" {
        return lower_await_body(args, param);
    }

    // Module-scope names (imports and top-level bindings populated by
    // `crate::module`) resolve before other op interpretations — but only
    // a bare `Op{name}` (a `var` reference) or a name the calculus does
    // not reserve: `a + b` and `add(x, y)` both produce `Op{add, [a, b]}`,
    // so a bound name colliding with a builtin op cannot be told apart
    // from the builtin use — preferring the builtin is the honest choice.
    if let Some(bound) = module_bound(name)
        && (args.is_empty() || !BUILTIN_OPS.contains(&name))
    {
        return lower_bound_op_body(&bound, args, source, lit, dim_label, param, scheme);
    }

    if let ArrSource::Foreign(module) = &source {
        // `perform`/`io.perform` are reserved builtin spellings — eval's
        // `is_perform_callee` intercepts them before host dispatch, so
        // they lower to `__handl_rt.perform` (in-flight dispatch +
        // residual `IO` mint) rather than emitting a `jsCall`. The
        // qualified spelling is `module.name` (a lone
        // `op_foreign("perform")` has module == name).
        let qualified = if module.as_str() == name {
            name.to_string()
        } else {
            format!("{module}.{name}")
        };
        if qualified == "perform" || qualified == "io.perform" {
            return lower_perform_body(args, param, scheme);
        }
        return lower_foreign_op_body(name, args, module, lit, dim_label, param);
    }

    // `JsConfig::dispatch_all_ops`: route the pure arg-op family —
    // the scalar operators plus `eq`/`neq`/`and`/`or`/`pow`/`concat` —
    // through `__handl_rt.dispatch` so covering `handle` frames can
    // intercept them (eval's `dispatch_op` consults handlers for
    // *every* `Native` op before `apply_native`). Ops that read the
    // incoming value (`id`, `elem`, `field_access`, …) or carry `lit`
    // payloads (`record`, `json`, `proj_env`, …) keep their dedicated
    // emit — the dispatch fallback cannot reproduce lit/incoming
    // semantics, so even under the flag those ops bypass handlers
    // (documented residual gap).
    if dispatch_all_ops() && matches!(source, ArrSource::Native) && DISPATCHABLE_OPS.contains(&name)
    {
        let lowered_args: Vec<JsAst> = args
            .iter()
            .map(|a| lower_sub(a, param))
            .collect::<Result<_, _>>()?;
        return Ok(dispatch_call(name, lowered_args, dim_label, param));
    }

    let binop = |op: JsBinOp| -> Result<JsAst, CompileError> {
        if args.len() != 2 {
            return Err(CompileError::InvalidInput(format!(
                "binary op `{name}` requires 2 arguments, got {}",
                args.len()
            )));
        }
        let left = lower_sub(&args[0], param)?;
        let right = lower_sub(&args[1], param)?;
        Ok(JsAst::BinOp {
            op,
            left: Box::new(left),
            right: Box::new(right),
        })
    };
    let applied_args = |args: &[Term]| -> Result<Vec<JsAst>, CompileError> {
        args.iter().map(|a| lower_sub(a, param)).collect()
    };
    let rt = rt_call;
    // `JsConfig::op_lowering`: `Prelude` routes the scalar operator
    // family through `__handl_rt.<name>` member calls (the uniform
    // prelude-call strategy) instead of raw JS operators; `Mixed`
    // keeps today's per-op inline emit. Arity checks match the inline
    // forms either way.
    let prelude_ops = op_lowering() == OpLowering::Prelude;
    let scalar_binop = |op: JsBinOp| -> Result<JsAst, CompileError> {
        if prelude_ops {
            if args.len() != 2 {
                return Err(CompileError::InvalidInput(format!(
                    "binary op `{name}` requires 2 arguments, got {}",
                    args.len()
                )));
            }
            return Ok(rt(name, applied_args(args)?));
        }
        binop(op)
    };
    let scalar_unop = |op: JsUnaryOp| -> Result<JsAst, CompileError> {
        if args.len() != 1 {
            return Err(CompileError::InvalidInput(format!(
                "unary op `{name}` requires 1 argument, got {}",
                args.len()
            )));
        }
        if prelude_ops {
            return Ok(rt(name, applied_args(args)?));
        }
        Ok(JsAst::UnaryOp {
            op,
            expr: Box::new(lower_sub(&args[0], param)?),
        })
    };

    let body = match name {
        // Ops that read the arrow's *incoming* value (not arg terms).
        "id" | "tag" | "pair_tag" | "loop_outcome" | "arr_len" | "paren" => {
            rt(name, vec![param.clone()])
        }
        "elem" => {
            let idx = lit.and_then(Value::as_u64).ok_or_else(|| {
                CompileError::InvalidInput("elem: missing index literal".to_string())
            })?;
            rt("elem", vec![param.clone(), JsAst::Raw(idx.to_string())])
        }
        "arr_drop" => {
            let n = lit.and_then(Value::as_u64).unwrap_or(0);
            rt("arr_drop", vec![param.clone(), JsAst::Raw(n.to_string())])
        }
        "match_fail" => rt("match_fail", vec![]),
        // `match` object-pattern ops (arrow-lowered `try_pattern`
        // mirrors). `is_obj`/`tag_fields` read the incoming value;
        // `has_field`/`obj_except` have two surfaces — a static `lit`
        // key/list on the input itself, or the dynamic-key form
        // `has_field(target, key)` / `obj_except(target, keys)` whose
        // args evaluate on the input (the `[v, in, keys]` extended
        // dispatch pair). `obj_key` coerces a computed key value to a
        // field name (`""` for non-strings, `try_object_pattern`'s
        // `as_str().unwrap_or("")`).
        "is_obj" | "tag_fields" => rt(name, vec![param.clone()]),
        // `ctor_name`/`ctor_fields` — ctor-object pattern introspection
        // (ADR-0152 §2, emitted by `compile_pattern`'s ctor arms). Like
        // `is_obj`/`tag_fields` they read the incoming value — the
        // `Seq[pv, Op]` form carries no args — and `native_ops` also
        // honors a first arg, mirrored here.
        "ctor_name" | "ctor_fields" => {
            let v = if args.is_empty() {
                param.clone()
            } else {
                lower_sub(&args[0], param)?
            };
            rt(name, vec![v])
        }
        // `assert`/`runtime_assert` — AST-level assertions whose
        // payload children never reach the `Term`
        // (`Op{assert, []}`/`Op{runtime_assert, []}` carry no args), so
        // they must still *call* the shim (which throws the
        // eval-faithful "unsupported" diagnostic) rather than emit a
        // bare member reference like other 0-arg ops.
        "assert" | "runtime_assert" => rt(name, vec![]),
        "obj_key" => {
            let v = if args.is_empty() {
                param.clone()
            } else {
                lower_sub(&args[0], param)?
            };
            rt("obj_key", vec![v])
        }
        "has_field" => {
            let key = match lit {
                Some(Value::String(s)) => JsAst::Str(s.clone()),
                _ if args.len() >= 2 => lower_sub(&args[1], param)?,
                _ => {
                    return Err(CompileError::InvalidInput(
                        "has_field: missing key literal".to_string(),
                    ));
                }
            };
            let target = if args.is_empty() {
                param.clone()
            } else {
                lower_sub(&args[0], param)?
            };
            rt("has_field", vec![target, key])
        }
        "obj_except" => {
            let keys = match lit {
                Some(Value::Array(ks)) => JsAst::Array(
                    ks.iter()
                        .map(|k| JsAst::Str(k.as_str().unwrap_or("").to_string()))
                        .collect(),
                ),
                _ if args.len() >= 2 => lower_sub(&args[1], param)?,
                _ => {
                    return Err(CompileError::InvalidInput(
                        "obj_except: missing keys literal".to_string(),
                    ));
                }
            };
            let target = if args.is_empty() {
                param.clone()
            } else {
                lower_sub(&args[0], param)?
            };
            rt("obj_except", vec![target, keys])
        }
        // Data constructors over applied arg terms.
        "product" => rt("product", applied_args(args)?),
        "array" => JsAst::Array(applied_args(args)?),
        "record" => {
            // `{__term__, __bound__}` — a bound-arrow record (ADR-0120
            // Phase 1): a frozen partial application that, applied to
            // `args`, runs `__term__` on `__bound__ ++ args`. On this
            // target arrow values are JS async arrows, so the record
            // lowers to one: `__bound__` is evaluated eagerly at
            // construction (enclosing `param`), the call packs
            // `bound ++ call-args` into the inner arrow's `__in` —
            // exactly the term_interpreter's application rule.
            if let Some(bound_arrow) = lower_bound_arrow_record(args, lit, param)? {
                bound_arrow
            } else if let Some(keys) = lit.and_then(Value::as_array) {
                let vals = applied_args(args)?;
                let mut props = Vec::with_capacity(vals.len());
                for (i, v) in vals.into_iter().enumerate() {
                    let key = keys.get(i).and_then(Value::as_str).ok_or_else(|| {
                        CompileError::InvalidInput("record: key is not a string".to_string())
                    })?;
                    props.push((key.to_string(), v));
                }
                JsAst::Object(props)
            } else {
                rt("record", applied_args(args)?)
            }
        }
        "field_access" => {
            // `lit` is a string for `r.field` and a number for `a[i]`
            // indexing. A computed index (`a[expr]`) arrives as a second
            // arg with no lit — lower it as the key expression.
            let key = match lit {
                Some(Value::String(s)) => JsAst::Str(s.clone()),
                Some(v @ (Value::Number(_) | Value::Bool(_))) => JsAst::Raw(v.to_string()),
                _ if args.len() >= 2 => lower_sub(&args[1], param)?,
                _ => {
                    return Err(CompileError::InvalidInput(
                        "field_access: missing field literal".to_string(),
                    ));
                }
            };
            let target = if args.is_empty() {
                param.clone()
            } else {
                lower_sub(&args[0], param)?
            };
            JsAst::Index {
                obj: Box::new(target),
                key: Box::new(key),
            }
        }
        // Comparisons / logic / math. The scalar family honors
        // `JsConfig::op_lowering` (`Mixed` → raw JS operators,
        // `Prelude` → `__handl_rt.<name>` member calls).
        "eq" | "neq" | "and" | "or" | "pow" => rt(name, applied_args(args)?),
        "lt" => scalar_binop(JsBinOp::Lt)?,
        "lte" => scalar_binop(JsBinOp::Le)?,
        "gt" => scalar_binop(JsBinOp::Gt)?,
        "gte" => scalar_binop(JsBinOp::Ge)?,
        "mod" => scalar_binop(JsBinOp::Mod)?,
        "neg" => scalar_unop(JsUnaryOp::Neg)?,
        "not" => scalar_unop(JsUnaryOp::Not)?,
        "json" => {
            let template = lit.and_then(Value::as_str).ok_or_else(|| {
                CompileError::InvalidInput("json: missing template literal".to_string())
            })?;
            rt(
                "json",
                vec![
                    JsAst::Str(template.to_string()),
                    JsAst::Array(applied_args(args)?),
                ],
            )
        }
        "add" => scalar_binop(JsBinOp::Add)?,
        "sub" => scalar_binop(JsBinOp::Sub)?,
        "mul" => scalar_binop(JsBinOp::Mul)?,
        "div" => scalar_binop(JsBinOp::Div)?,
        "concat" => {
            if args.len() != 2 {
                return Err(CompileError::InvalidInput(format!(
                    "concat requires 2 arguments, got {}",
                    args.len()
                )));
            }
            let left = lower_sub(&args[0], param)?;
            let right = lower_sub(&args[1], param)?;
            rt_call("concat", vec![left, right])
        }
        "proj_env" => {
            let env = if args.is_empty() {
                param.clone()
            } else {
                lower_sub(&args[0], param)?
            };
            let field = lit.and_then(|v| v.as_str()).ok_or_else(|| {
                CompileError::InvalidInput("proj_env missing lit name".to_string())
            })?;
            rt_call("projEnv", vec![env, JsAst::Str(field.to_string())])
        }
        // `perform`/`io.perform` — the io module's `PerformFn` builtin
        // (ADR-0153 §4 residual `IO` mint + ADR-0158 in-flight
        // dispatch). Emits `__handl_rt.perform(op, arg)`, the runtime
        // mirror of `native_ops::perform_dispatch`; multi-shot `resume`
        // stays the `__applyHandler` honest throw.
        "perform" | "io.perform" => lower_perform_body(args, param, scheme)?,
        _ => {
            // Effect/proof op spellings that must never reach
            // `__handl_rt.<name>` or `__opRef`: `handle`/`handle_dyn`
            // belong to `Term::Handle` (the runtime `handle` member has
            // a different signature), `rewrite`/`path`/`equiv` have no
            // eval semantics to dispatch to (`Term::Rewrite` errors in
            // `proof.rs`; `path`/`equiv` are `Term::Opaque` data).
            if matches!(name, "handle" | "handle_dyn" | "rewrite" | "path" | "equiv") {
                return Err(CompileError::Lower(format!(
                    "op `{name}` is not supported by the JS backend \
                     (effect/proof operations have no runtime semantics here)"
                )));
            }
            if args.is_empty() {
                // A 0-arg op term is the op referenced *as a value* (e.g.
                // `var tag` for the ambient `tag` host package). Route
                // through `__opRef`: it resolves the name to the runtime
                // member when the op/package exists and throws
                // "unsupported op" like eval when it does not — a bare
                // `__handl_rt.<name>` member would silently evaluate to
                // `undefined` for unknown names (e.g. an unbound pattern
                // variable left over from a `{x}` shorthand object
                // pattern, which binds nothing). In *scheme* position the
                // reference is a body arrow — apply the resolved
                // referent to the incoming (`__appOrValue` keeps a
                // non-callable referent honest).
                let referent = rt_call("__opRef", vec![JsAst::Str(name.to_string())]);
                if scheme {
                    return Ok(JsAst::Await(Box::new(rt_call(
                        "__appOrValue",
                        vec![JsAst::Await(Box::new(referent)), param.clone()],
                    ))));
                }
                referent
            } else {
                let lowered_args: Vec<JsAst> = args
                    .iter()
                    .map(|a| lower_sub(a, param))
                    .collect::<Result<_, _>>()?;
                if matches!(source, ArrSource::Native) {
                    // Unknown/native op *call*: route through
                    // `__handl_rt.dispatch` so a covering `handle` frame
                    // can intercept it (eval's `dispatch_op` consults
                    // the handler stack for `Native`/`Foreign` ops —
                    // `Foreign` was diverted above, and `Judgment`/
                    // `DynamicForeign` keep the direct member call);
                    // uncovered ops fall back to the `__handl_rt.<name>`
                    // member inside `dispatch`, then "unsupported op".
                    // `args` evaluates once into the array `dispatch`
                    // unpacks; `param` is the incoming value for the
                    // second (`impls`-field) handler channel.
                    dispatch_call(name, lowered_args, dim_label, param)
                } else {
                    JsAst::Call {
                        callee: Box::new(rt_member(name)),
                        args: lowered_args,
                    }
                }
            }
        }
    };

    Ok(body)
}

/// Emit an `Op { name, args }` body whose name resolves to a
/// module-scope binding ([`BoundRef`]) — an import or top-level `const`.
/// `scheme` is `lower_op_body`'s application-vs-referent switch: a
/// bare `Op{name, []}` in scheme position applies the bound referent
/// to `param` via `__handl_rt.__appOrValue` (arrow bindings run,
/// non-callable values read back); in value position it stays the
/// referent itself — except [`BoundRef::Shim`], whose js-contract
/// members are host *functions*: a 0-arg value-position op is the
/// `js_error()` call spelling and invokes the member (I-001).
fn lower_bound_op_body(
    bound: &BoundRef,
    args: &[Term],
    source: ArrSource,
    lit: Option<&Value>,
    dim_label: Option<&str>,
    param: &JsAst,
    scheme: bool,
) -> Result<JsAst, CompileError> {
    if let BoundRef::Op(real) = bound {
        return lower_op_body(real, args, source, lit, dim_label, param, scheme);
    }
    let applied = args
        .iter()
        .map(|a| lower_sub(a, param))
        .collect::<Result<Vec<_>, _>>()?;
    // The referent expression for a bare `Op{name, []}` — what value
    // position emits today. In scheme position it feeds
    // `__appOrValue(referent, param)` instead.
    let app_or_value = |referent: JsAst| -> JsAst {
        if !scheme {
            return referent;
        }
        JsAst::Await(Box::new(rt_call(
            "__appOrValue",
            vec![referent, param.clone()],
        )))
    };
    let body = match bound {
        // `Arrow` is a top-level `const` name, `DepArrow` a member path
        // into an inlined dep module (`__handl_rt[spec].name`) — both are
        // `async __in => …` arrows applied with the packed arg input.
        BoundRef::Arrow(js_name) | BoundRef::DepArrow(js_name) => {
            let callee = match bound {
                BoundRef::DepArrow(_) => JsAst::Raw(js_name.clone()),
                _ => JsAst::Ident(js_name.clone()),
            };
            if applied.is_empty() {
                // Value position: the arrow yields the binding itself;
                // scheme position: apply it to the incoming.
                app_or_value(callee)
            } else {
                // `app` arg-input: one argument stays bare, several pack
                // into the tuple array the callee's `__in` expects.
                // `__apply` reinstates `__carried__` handler frames a
                // `handle` attached to the arrow value (deep dispatch).
                let mut applied = applied;
                let arg_input = if applied.len() == 1 {
                    applied.pop().unwrap()
                } else {
                    JsAst::Array(applied)
                };
                JsAst::Await(Box::new(rt_call("__apply", vec![callee, arg_input])))
            }
        }
        BoundRef::Ident(js_name) => {
            if applied.is_empty() {
                app_or_value(JsAst::Ident(js_name.clone()))
            } else {
                JsAst::Call {
                    callee: Box::new(JsAst::Ident(js_name.clone())),
                    args: applied,
                }
            }
        }
        BoundRef::Shim(member) => {
            let callee = rt_member(member);
            if applied.is_empty() {
                if scheme {
                    // Scheme position (`x >>> js_error`): the referent is
                    // the `__handl_rt` member itself (a host function),
                    // applied to the incoming via `__appOrValue`.
                    app_or_value(callee)
                } else {
                    // Value position: `js_error()` is a *call* — the js
                    // contract shims are host functions, never Handl
                    // arrow values, so the 0-arg `Op{name, []}` invokes
                    // the member (`__handl_rt.error()`), matching eval
                    // where the foreign op runs with zero args. Arrow-
                    // lower collapses `f()` and bare `f` to the same
                    // term; the call reading is the one real code uses
                    // (`e1 := js_error()`), and the bare member would
                    // read as the function object — `eq(·, null)` never
                    // holds, `__apply` would feed it a Handl `__in`
                    // instead of the spread-args convention (I-001).
                    JsAst::Call {
                        callee: Box::new(callee),
                        args: Vec::new(),
                    }
                }
            } else {
                JsAst::Call {
                    callee: Box::new(callee),
                    args: applied,
                }
            }
        }
        // `from "io" import perform [as p]` binds the
        // `__handl_rt.io.perform` member path (`module.rs`'s host-module
        // import arm). A *call* routes to `__handl_rt.perform` so the
        // op-name operand keeps the bare-identifier → name-string rule
        // (a spread `__handl_rt.io.perform(…)` call would feed
        // `__opRef("puts")` — eval's "unsupported op" throw — instead of
        // `"puts"`); a bare reference stays the member path, which the
        // prelude's `io` namespace resolves.
        BoundRef::Member(path) if path == "__handl_rt.io.perform" && !args.is_empty() => {
            return lower_perform_call(args, param);
        }
        // `Member` is a host member path (comma/spread args), `Ns` a dep
        // module's namespace record — both are plain values; a direct
        // `Op`-spelled call applies the member spread convention.
        BoundRef::Member(path) | BoundRef::Ns(path) => {
            if applied.is_empty() {
                app_or_value(JsAst::Raw(path.clone()))
            } else {
                JsAst::Call {
                    callee: Box::new(JsAst::Raw(path.clone())),
                    args: applied,
                }
            }
        }
        BoundRef::Op(_) => unreachable!("handled above"),
    };
    Ok(body)
}

/// Emit a `{__term__, __bound__}` bound-arrow record (`Op{record}` with
/// that literal key pair) as a JS arrow value:
///
/// ```js
/// (await (async () => {
///   const __b = <bound>;
///   return async __args => (await (<term>)(__handl_rt.__appInput(__b, __args)));
/// })())
/// ```
///
/// `__b` is evaluated once at construction against the enclosing input —
/// the record-freeze point — and the call packs `bound ++ call-args` into
/// the inner arrow's `__in` (the `term_interpreter` application rule: a
/// lone arg stays bare, a tuple spreads). `__term__` lowers through the
/// ordinary value path: a quoted `Arr` is already a JS arrow, and a
/// nested bound-arrow record is its own wrapper — both are callable.
/// Returns `None` (fall back to the plain-record emit) for any other
/// shape.
fn lower_bound_arrow_record(
    args: &[Term],
    lit: Option<&Value>,
    param: &JsAst,
) -> Result<Option<JsAst>, CompileError> {
    let is_bound_record = lit.and_then(Value::as_array).is_some_and(|keys| {
        keys.len() == 2
            && keys[0].as_str() == Some("__term__")
            && keys[1].as_str() == Some("__bound__")
    });
    if !is_bound_record || args.len() != 2 {
        return Ok(None);
    }
    let term_expr = lower_sub(&args[0], param)?;
    let bound_expr = lower_sub(&args[1], param)?;
    Ok(Some(JsAst::Await(Box::new(JsAst::Call {
        callee: Box::new(JsAst::ArrowFn {
            params: Vec::new(),
            body: Box::new(JsAst::Block(vec![
                JsAst::ConstDecl {
                    name: "__b".to_string(),
                    value: Box::new(bound_expr),
                },
                JsAst::Return(Box::new(JsAst::ArrowFn {
                    params: vec!["__args".to_string()],
                    // `__apply` reinstates `__carried__` handler frames
                    // on the bound arrow value.
                    body: Box::new(JsAst::Await(Box::new(rt_call(
                        "__apply",
                        vec![
                            term_expr,
                            rt_call(
                                "__appInput",
                                vec![
                                    JsAst::Ident("__b".to_string()),
                                    JsAst::Ident("__args".to_string()),
                                ],
                            ),
                        ],
                    )))),
                })),
            ])),
        }),
        args: Vec::new(),
    }))))
}

/// `perform(op, arg)` / `io.perform(op, arg)` — the `io` module's
/// `PerformFn` builtin. Eval intercepts the callee name at the
/// host-function seam (`native_ops::is_perform_callee` →
/// `perform_dispatch`): a covered op applies its handler impl
/// in-flight (with `acc` threading), an uncovered op falls through to
/// `PerformFn`'s residual `IO<{op: arg}>` former that an enclosing
/// `handle`'s `__interpretIO` interprets. `__handl_rt.perform` is the
/// runtime mirror of exactly that pair, so the emit is a plain member
/// call — *not* `__handl_rt.dispatch` (a handler covering the literal
/// op name `"perform"` is not consulted on this path in eval either).
///
/// A bare `Op{perform, []}` is the PerformFn *value*: value position
/// emits the `__handl_rt.perform` member, scheme position applies it
/// to the incoming like any 0-arg op referent.
fn lower_perform_body(args: &[Term], param: &JsAst, scheme: bool) -> Result<JsAst, CompileError> {
    if args.is_empty() {
        let member = rt_member("perform");
        if !scheme {
            return Ok(member);
        }
        return Ok(JsAst::Await(Box::new(rt_call(
            "__appOrValue",
            vec![member, param.clone()],
        ))));
    }
    lower_perform_call(args, param)
}

/// `__handl_rt.perform(<args>)` for a non-empty argument list — shared
/// by the `Op` arm above, the `Term::App` `io.perform` qualified-callee
/// path in `control.rs`, and `BoundRef::Member("__handl_rt.io.perform")`
/// (`from "io" import perform`). The call itself is not awaited: every
/// emitted arrow is async, so the returned promise flattens at the
/// enclosing `await` exactly like `dispatch` calls do.
pub(crate) fn lower_perform_call(args: &[Term], param: &JsAst) -> Result<JsAst, CompileError> {
    let mut lowered: Vec<JsAst> = Vec::with_capacity(args.len());
    for (i, a) in args.iter().enumerate() {
        // `perform` takes 1–2 arguments (op, arg?); extras pass through
        // so the runtime reports eval's arity error rather than the
        // lowerer silently dropping them.
        lowered.push(if i == 0 {
            perform_op_operand(a, param)?
        } else {
            lower_sub(a, param)?
        });
    }
    Ok(rt_call("perform", lowered))
}

/// The `perform` op-name operand. Eval's unbound-var rule hands the
/// bare identifier's *name* to `PerformFn` (`perform(puts, …)` arrives
/// as `"puts"` — `io.rs`'s documented operand spellings), so a bare
/// `Op{name, []}` that resolves to no module binding and no builtin op
/// lowers to that name string. Anything else — a string/`"puts"`
/// literal, a `sym(…)` value, a module-bound name, a computed term —
/// lowers normally and the runtime validates the string/`__sym__`
/// shape (`perform: op must be an op name … or a sym value`).
fn perform_op_operand(term: &Term, param: &JsAst) -> Result<JsAst, CompileError> {
    if let Term::Op { name, args, .. } = term
        && args.is_empty()
        && module_bound(name).is_none()
        && !BUILTIN_OPS.contains(&name.as_str())
    {
        return Ok(JsAst::Str(name.clone()));
    }
    lower_sub(term, param)
}

/// `await` — the `js.await` op. Takes exactly one argument (the promise)
/// and emits `(await __handl_rt.await(<arg>))`: the helper resolves to a
/// `Result` — `tag.tag("ok", {value: v})` or `tag.tag("err", {value: e})`
/// — never rejects — so a rejection is data, not an exception unwinding
/// out of the Handl program, and callers read it with `tag.tag_of` /
/// `tag.untag` exactly as they do at eval time.
fn lower_await_body(args: &[Term], param: &JsAst) -> Result<JsAst, CompileError> {
    if args.len() != 1 {
        return Err(CompileError::InvalidInput(format!(
            "await requires 1 argument (the promise), got {}",
            args.len()
        )));
    }
    let promise = lower_sub(&args[0], param)?;
    Ok(JsAst::Await(Box::new(rt_call("await", vec![promise]))))
}

fn lower_foreign_op_body(
    name: &str,
    args: &[Term],
    module: &str,
    lit: Option<&Value>,
    dim_label: Option<&str>,
    param: &JsAst,
) -> Result<JsAst, CompileError> {
    let lowered_args: Vec<JsAst> = args
        .iter()
        .map(|a| lower_sub(a, param))
        .collect::<Result<_, _>>()?;

    // Eval's `dispatch_op` covers `Foreign` sources too — the op `name`
    // is the handler-routing key. Route every foreign call through
    // `__handl_rt.dispatchForeign`, whose uncovered fallback is exactly
    // the call this function used to emit directly (`inline` selects
    // `globalThis[module][name]`, otherwise `jsCall`).
    let (descriptors, inline) = if !allow_inlining() {
        (foreign_descriptors(lit)?, false)
    // Experimental escape hatch (`JsConfig::unsafe_inline_foreign`):
    // skip the descriptor safety verdict — every foreign op inlines to
    // `globalThis[module][name](...)`. `no_inline_foreign` still wins
    // (checked above).
    } else if unsafe_inline_foreign() {
        (Value::Null, true)
    } else if let Some(lit) = lit {
        let desc = crate::security::validate_foreign_descriptor(lit)?;
        if crate::security::is_safe_for_inlining(&desc) {
            (Value::Null, true)
        } else {
            (desc.to_json(), false)
        }
    } else {
        (Value::Null, false)
    };
    let raw = serde_json::to_string(&descriptors).map_err(|e| CompileError::Emit(e.to_string()))?;
    Ok(rt_call(
        "dispatchForeign",
        vec![
            JsAst::Str(name.to_string()),
            JsAst::Array(lowered_args),
            dim_label
                .map(|d| JsAst::Str(d.to_string()))
                .unwrap_or(JsAst::Null),
            param.clone(),
            JsAst::Str(module.to_string()),
            JsAst::Raw(raw),
            JsAst::Bool(inline),
        ],
    ))
}

/// The validated foreign descriptor as JSON (`null` when absent) — the
/// `descriptors` argument `jsCall`/`dispatchForeign` carry.
fn foreign_descriptors(lit: Option<&Value>) -> Result<Value, CompileError> {
    match lit {
        Some(v) => Ok(crate::security::validate_foreign_descriptor(v)?.to_json()),
        None => Ok(Value::Null),
    }
}
