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
            // A quoted arrow (`Arr` holding a serialized `Term` — the
            // ADR-0120 Phase 1 quoting form) is a *function value* on this
            // target: all arrow values are JS async arrows, so `arr(q)`
            // is the constant arrow returning that function.
            if let Ok(inner) = serde_json::from_value::<Term>(value.clone()) {
                let arrow = super::lower(&inner)?;
                return Ok(JsAst::ArrowFn {
                    params: vec![INPUT_NAME.to_string()],
                    body: Box::new(arrow),
                });
            }
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
            let body = lower_seq_body(fst, snd, &param)?;
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
    "is_obj", "has_field", "tag_fields", "obj_except", "obj_key",
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
            lower_seq_body(fst, snd, _input)
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
        return lower_sub(snd, &fst_js);
    }
    let name = super::next_tmp();
    let body = lower_sub(snd, &JsAst::Ident(name.clone()))?;
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
            // `lit` is a string for `r.field` and a number for `a[i]`
            // indexing. A computed index (`a[expr]`) arrives as a second
            // arg with no lit — lower it as the key expression.
            let key = match lit {
                Some(Value::String(s)) => JsAst::Str(s.clone()),
                Some(v @ (Value::Number(_) | Value::Bool(_))) => {
                    JsAst::Raw(v.to_string())
                }
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
        // `Arrow` is a top-level `const` name, `DepArrow` a member path
        // into an inlined dep module (`__handl_rt[spec].name`) — both are
        // `async __in => …` arrows applied with the packed arg input.
        BoundRef::Arrow(js_name) | BoundRef::DepArrow(js_name) => {
            let callee = match bound {
                BoundRef::DepArrow(_) => JsAst::Raw(js_name.clone()),
                _ => JsAst::Ident(js_name.clone()),
            };
            if applied.is_empty() {
                // Value position: the arrow yields the binding itself.
                callee
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
                    callee: Box::new(callee),
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
        // `Member` is a host member path (comma/spread args), `Ns` a dep
        // module's namespace record — both are plain values; a direct
        // `Op`-spelled call applies the member spread convention.
        BoundRef::Member(path) | BoundRef::Ns(path) => {
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
    let is_bound_record = lit
        .and_then(Value::as_array)
        .is_some_and(|keys| {
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
                    body: Box::new(JsAst::Await(Box::new(JsAst::Call {
                        callee: Box::new(term_expr),
                        args: vec![JsAst::Call {
                            callee: Box::new(JsAst::Member {
                                obj: Box::new(JsAst::Ident("__handl_rt".to_string())),
                                prop: "__appInput".to_string(),
                            }),
                            args: vec![
                                JsAst::Ident("__b".to_string()),
                                JsAst::Ident("__args".to_string()),
                            ],
                        }],
                    }))),
                })),
            ])),
        }),
        args: Vec::new(),
    }))))
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
