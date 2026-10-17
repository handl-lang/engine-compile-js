//! Lower the primitive `Term` combinators (`Arr`, `Const`, `Op`, `Seq`) to JS.

use handl_plugin_compile::CompileError;
use plugin_engine::{ArrSource, Term};
use serde_json::Value;

use super::lower_applied as lower_sub;
use super::{BoundRef, JsAst, JsBinOp, JsUnaryOp, allow_inlining, lower_value, module_bound};

const INPUT_NAME: &str = "__in";

/// Lower one of the four primitive terms to a JS expression.
pub fn lower(term: &Term) -> Result<JsAst, CompileError> {
    match term {
        Term::Arr { value, .. } => {
            let body = lower_value(value)?;
            Ok(JsAst::ArrowFn {
                params: vec![INPUT_NAME.to_string()],
                body: Box::new(body),
            })
        }
        Term::Const { value } => lower_value(value),
        Term::Seq { axis, fst, snd } => {
            check_sequential(axis, fst)?;
            let param = JsAst::Ident(INPUT_NAME.to_string());
            let body = lower_sub(snd, &lower_sub(fst, &param)?)?;
            Ok(JsAst::ArrowFn {
                params: vec![INPUT_NAME.to_string()],
                body: Box::new(body),
            })
        }
        Term::Op {
            name,
            args,
            source,
            lit,
            ..
        } => lower_op(name, args, source.clone(), lit.as_ref()),
        _ => Err(CompileError::Lower(format!(
            "primitive lowerer received non-primitive term: {term:?}"
        ))),
    }
}

/// Op names the calculus/lowerer reserves — a module binding under one
/// of these names still resolves as the builtin in argument position.
const BUILTIN_OPS: &[&str] = &[
    "id", "tag", "pair_tag", "loop_outcome", "arr_len", "arr_drop", "match_fail",
    "paren", "rest", "elem", "product", "array", "record", "field_access",
    "eq", "neq", "and", "or", "not", "neg", "pow", "mod", "lt", "lte", "gt",
    "gte", "add", "sub", "mul", "div", "concat", "json", "proj_env", "await",
    "assert", "runtime_assert", "export", "import", "extend_env", "apply",
    "app", "resolve", "eval_template", "match_shell", "loop_step", "object",
    "fanout", "sum", "choice", "left", "right", "first", "second", "assign",
    "desugar_error", "proj_child", "proj_child_or", "proj_rest", "compare",
];

/// Lower the application of a primitive term to an input value.
pub fn lower_applied(term: &Term, _input: &JsAst) -> Result<JsAst, CompileError> {
    match term {
        Term::Arr { value, .. } | Term::Const { value } => lower_value(value),
        // Applying an `Op` arrow to `input` evaluates the op body with
        // `input` as the incoming value — emit the body against `input`
        // directly (awaited: op bodies may themselves contain `await`).
        Term::Op {
            name,
            args,
            source,
            lit,
            ..
        } => Ok(JsAst::Await(Box::new(lower_op_body(
            name,
            args,
            source.clone(),
            lit.as_ref(),
            _input,
        )?))),
        Term::Seq { axis, fst, snd } => {
            check_sequential(axis, fst)?;
            lower_sub(snd, &lower_sub(fst, _input)?)
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

fn lower_op(
    name: &str,
    args: &[Term],
    source: ArrSource,
    lit: Option<&Value>,
) -> Result<JsAst, CompileError> {
    let param = JsAst::Ident(INPUT_NAME.to_string());
    let body = lower_op_body(name, args, source, lit, &param)?;
    Ok(JsAst::ArrowFn {
        params: vec![INPUT_NAME.to_string()],
        body: Box::new(body),
    })
}

/// The body of an `Op` arrow evaluated against the incoming value
/// `param` — `lower_op` wraps it in `async __in => …`; `lower_applied`
/// passes the concrete input expression instead.
fn lower_op_body(
    name: &str,
    args: &[Term],
    source: ArrSource,
    lit: Option<&Value>,
    param: &JsAst,
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
    if let Some(bound) = module_bound(name) {
        if args.is_empty() || !BUILTIN_OPS.contains(&name) {
            return lower_bound_op_body(&bound, args, source, lit, param);
        }
    }

    if let ArrSource::Foreign(module) = &source {
        return lower_foreign_op_body(name, args, module, lit, param);
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
    let rt = |name: &str, args: Vec<JsAst>| JsAst::Call {
        callee: Box::new(JsAst::Member {
            obj: Box::new(JsAst::Ident("__handl_rt".to_string())),
            prop: name.to_string(),
        }),
        args,
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
        // Data constructors over applied arg terms.
        "product" => rt("product", applied_args(args)?),
        "array" => JsAst::Array(applied_args(args)?),
        "record" => {
            if let Some(keys) = lit.and_then(Value::as_array) {
                let vals = applied_args(args)?;
                let mut props = Vec::with_capacity(vals.len());
                for (i, v) in vals.into_iter().enumerate() {
                    let key = keys
                        .get(i)
                        .and_then(Value::as_str)
                        .ok_or_else(|| {
                            CompileError::InvalidInput(
                                "record: key is not a string".to_string(),
                            )
                        })?;
                    props.push((key.to_string(), v));
                }
                JsAst::Object(props)
            } else {
                rt("record", applied_args(args)?)
            }
        }
        "field_access" => {
            let field = lit.and_then(Value::as_str).ok_or_else(|| {
                CompileError::InvalidInput("field_access: missing field literal".to_string())
            })?;
            let target = if args.is_empty() {
                param.clone()
            } else {
                lower_sub(&args[0], param)?
            };
            JsAst::Index {
                obj: Box::new(target),
                key: Box::new(JsAst::Str(field.to_string())),
            }
        }
        // Comparisons / logic / math.
        "eq" | "neq" | "and" | "or" | "pow" => rt(name, applied_args(args)?),
        "lt" => binop(JsBinOp::Lt)?,
        "lte" => binop(JsBinOp::Le)?,
        "gt" => binop(JsBinOp::Gt)?,
        "gte" => binop(JsBinOp::Ge)?,
        "mod" => binop(JsBinOp::Mod)?,
        "neg" => {
            if args.len() != 1 {
                return Err(CompileError::InvalidInput(format!(
                    "unary op `neg` requires 1 argument, got {}",
                    args.len()
                )));
            }
            JsAst::UnaryOp {
                op: JsUnaryOp::Neg,
                expr: Box::new(lower_sub(&args[0], param)?),
            }
        }
        "not" => {
            if args.len() != 1 {
                return Err(CompileError::InvalidInput(format!(
                    "unary op `not` requires 1 argument, got {}",
                    args.len()
                )));
            }
            JsAst::UnaryOp {
                op: JsUnaryOp::Not,
                expr: Box::new(lower_sub(&args[0], param)?),
            }
        }
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
        "add" => binop(JsBinOp::Add)?,
        "sub" => binop(JsBinOp::Sub)?,
        "mul" => binop(JsBinOp::Mul)?,
        "div" => binop(JsBinOp::Div)?,
        "concat" => {
            if args.len() != 2 {
                return Err(CompileError::InvalidInput(format!(
                    "concat requires 2 arguments, got {}",
                    args.len()
                )));
            }
            let left = lower_sub(&args[0], param)?;
            let right = lower_sub(&args[1], param)?;
            JsAst::Call {
                callee: Box::new(JsAst::Member {
                    obj: Box::new(JsAst::Ident("__handl_rt".to_string())),
                    prop: "concat".to_string(),
                }),
                args: vec![left, right],
            }
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
            JsAst::Call {
                callee: Box::new(JsAst::Member {
                    obj: Box::new(JsAst::Ident("__handl_rt".to_string())),
                    prop: "projEnv".to_string(),
                }),
                args: vec![env, JsAst::Str(field.to_string())],
            }
        }
        _ => {
            let callee = JsAst::Member {
                obj: Box::new(JsAst::Ident("__handl_rt".to_string())),
                prop: name.to_string(),
            };
            if args.is_empty() {
                // A 0-arg op term is the op referenced *as a value* (e.g.
                // `var tag` for the ambient `tag` host package): emit the
                // member, not a call.
                callee
            } else {
                let lowered_args: Vec<JsAst> = args
                    .iter()
                    .map(|a| lower_sub(a, param))
                    .collect::<Result<_, _>>()?;
                JsAst::Call {
                    callee: Box::new(callee),
                    args: lowered_args,
                }
            }
        }
    };

    Ok(body)
}

/// Emit an `Op { name, args }` body whose name resolves to a
/// module-scope binding ([`BoundRef`]) — an import or top-level `const`.
fn lower_bound_op_body(
    bound: &BoundRef,
    args: &[Term],
    source: ArrSource,
    lit: Option<&Value>,
    param: &JsAst,
) -> Result<JsAst, CompileError> {
    if let BoundRef::Op(real) = bound {
        return lower_op_body(real, args, source, lit, param);
    }
    let applied = args
        .iter()
        .map(|a| lower_sub(a, param))
        .collect::<Result<Vec<_>, _>>()?;
    let body = match bound {
        BoundRef::Arrow(js_name) => {
            if applied.is_empty() {
                // Value position: the arrow yields the binding itself.
                JsAst::Ident(js_name.clone())
            } else {
                // `app` arg-input: one argument stays bare, several pack
                // into the tuple array the callee's `__in` expects.
                let mut applied = applied;
                let arg_input = if applied.len() == 1 {
                    applied.pop().unwrap()
                } else {
                    JsAst::Array(applied)
                };
                JsAst::Await(Box::new(JsAst::Call {
                    callee: Box::new(JsAst::Ident(js_name.clone())),
                    args: vec![arg_input],
                }))
            }
        }
        BoundRef::Ident(js_name) => {
            if applied.is_empty() {
                JsAst::Ident(js_name.clone())
            } else {
                JsAst::Call {
                    callee: Box::new(JsAst::Ident(js_name.clone())),
                    args: applied,
                }
            }
        }
        BoundRef::Shim(member) => JsAst::Call {
            callee: Box::new(JsAst::Member {
                obj: Box::new(JsAst::Ident("__handl_rt".to_string())),
                prop: member.clone(),
            }),
            args: applied,
        },
        BoundRef::Member(path) => {
            if applied.is_empty() {
                JsAst::Raw(path.clone())
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

/// `await` — the `js.await` op. Takes exactly one argument (the promise)
/// and emits `(await __handl_rt.await(<arg>))`: the helper resolves to
/// `{ok: v}` or `{err: <JsError>}` — never rejects — so a rejection is a
/// data record, not an exception unwinding out of the Handl program.
fn lower_await_body(args: &[Term], param: &JsAst) -> Result<JsAst, CompileError> {
    if args.len() != 1 {
        return Err(CompileError::InvalidInput(format!(
            "await requires 1 argument (the promise), got {}",
            args.len()
        )));
    }
    let promise = lower_sub(&args[0], param)?;
    Ok(JsAst::Await(Box::new(JsAst::Call {
        callee: Box::new(JsAst::Member {
            obj: Box::new(JsAst::Ident("__handl_rt".to_string())),
            prop: "await".to_string(),
        }),
        args: vec![promise],
    })))
}

fn lower_foreign_op_body(
    name: &str,
    args: &[Term],
    module: &str,
    lit: Option<&Value>,
    param: &JsAst,
) -> Result<JsAst, CompileError> {
    let lowered_args: Vec<JsAst> = args
        .iter()
        .map(|a| lower_sub(a, param))
        .collect::<Result<_, _>>()?;

    if !allow_inlining() {
        return emit_js_call(module, name, &lowered_args, lit);
    }

    if let Some(lit) = lit {
        let desc = crate::security::validate_foreign_descriptor(lit)?;
        if crate::security::is_safe_for_inlining(&desc) {
            return Ok(inline_foreign_call(module, name, lowered_args));
        }
    }

    emit_js_call(module, name, &lowered_args, lit)
}

fn inline_foreign_call(module: &str, name: &str, args: Vec<JsAst>) -> JsAst {
    JsAst::Call {
        callee: Box::new(JsAst::Index {
            obj: Box::new(JsAst::Index {
                obj: Box::new(JsAst::Ident("globalThis".to_string())),
                key: Box::new(JsAst::Str(module.to_string())),
            }),
            key: Box::new(JsAst::Str(name.to_string())),
        }),
        args,
    }
}

fn emit_js_call(
    module: &str,
    name: &str,
    args: &[JsAst],
    maybe_desc: Option<&Value>,
) -> Result<JsAst, CompileError> {
    let descriptors = match maybe_desc {
        Some(v) => crate::security::validate_foreign_descriptor(v)?.to_json(),
        None => Value::Null,
    };
    let raw = serde_json::to_string(&descriptors).map_err(|e| CompileError::Emit(e.to_string()))?;
    Ok(JsAst::Call {
        callee: Box::new(JsAst::Member {
            obj: Box::new(JsAst::Ident("__handl_rt".to_string())),
            prop: "jsCall".to_string(),
        }),
        args: vec![
            JsAst::Str(module.to_string()),
            JsAst::Str(name.to_string()),
            JsAst::Array(args.to_vec()),
            JsAst::Raw(raw),
        ],
    })
}
