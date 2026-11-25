//! Lower the control-flow `Term` combinators (`First`, `Choice`, `Loop`, `Iter`, `App`) to JS.

use handl_plugin_compile::CompileError;
use hnl_engine::Term;
use serde_json::{Number, Value};

use crate::lower::{
    JsAst, JsBinOp, JsUnaryOp, arrow_fn, input_ident, js_string, loop_max_iterations, rt_call,
};

fn let_decl(name: &str, value: JsAst) -> JsAst {
    JsAst::Let {
        name: name.to_string(),
        value: Box::new(value),
    }
}

fn assign(name: &str, value: JsAst) -> JsAst {
    JsAst::Assign {
        name: name.to_string(),
        value: Box::new(value),
    }
}

/// Lower a control combinator to a top-level JS arrow.
pub fn lower(term: &Term) -> Result<JsAst, CompileError> {
    match term {
        Term::First { inner } => lower_first(inner),
        Term::Choice { l, r } => lower_choice(l, r),
        Term::Loop { body } => lower_loop(body),
        Term::Iter {
            fuel,
            predicate,
            step,
            initial,
        } => lower_iter(fuel, predicate, step, initial),
        Term::App { callee, args } => lower_app(callee, args),
        _ => Err(CompileError::Lower(format!(
            "control lowerer received non-control term: {term:?}"
        ))),
    }
}

/// Lower a control combinator applied to a concrete input expression.
pub fn lower_applied(term: &Term, input: &JsAst) -> Result<JsAst, CompileError> {
    match term {
        Term::First { inner } => lower_first_applied(inner, input),
        Term::Choice { l, r } => lower_choice_applied(l, r, input),
        Term::Loop { body } => lower_loop_applied(body, input),
        Term::Iter {
            fuel,
            predicate,
            step,
            initial,
        } => lower_iter_applied(fuel, predicate, step, initial, input),
        Term::App { callee, args } => lower_app_applied(callee, args, input),
        _ => Err(CompileError::Lower(format!(
            "control lowerer received non-control term: {term:?}"
        ))),
    }
}

/// `First { inner }` — run `inner` on the first component of a pair.
pub fn lower_first(inner: &Term) -> Result<JsAst, CompileError> {
    let param = input_ident();
    Ok(arrow_fn(lower_first_applied(inner, &param)?))
}

fn lower_first_applied(inner: &Term, input: &JsAst) -> Result<JsAst, CompileError> {
    let fst = rt_call("fst", vec![input.clone()]);
    let snd = rt_call("snd", vec![input.clone()]);
    let inner_out = super::lower_applied(inner, &fst)?;
    Ok(rt_call("mkPair", vec![inner_out, snd]))
}

/// `Choice { l, r }` — route on `Either`.
pub fn lower_choice(l: &Term, r: &Term) -> Result<JsAst, CompileError> {
    let param = input_ident();
    Ok(arrow_fn(lower_choice_applied(l, r, &param)?))
}

fn lower_choice_applied(l: &Term, r: &Term, input: &JsAst) -> Result<JsAst, CompileError> {
    let cond = rt_call("isLeft", vec![input.clone()]);
    let left_val = rt_call("leftValue", vec![input.clone()]);
    let right_val = rt_call("rightValue", vec![input.clone()]);
    let left = super::lower_applied(l, &left_val)?;
    let right = super::lower_applied(r, &right_val)?;
    // The branch IIFE may contain `await` expressions (e.g. `js.await`),
    // so it is async and its call is awaited.
    let iife = JsAst::ArrowFn {
        params: vec![],
        body: Box::new(JsAst::Block(vec![JsAst::If {
            cond: Box::new(cond),
            then_branch: Box::new(JsAst::Block(vec![JsAst::Return(Box::new(left))])),
            else_branch: Some(Box::new(JsAst::Block(vec![JsAst::Return(Box::new(right))]))),
        }])),
    };
    Ok(JsAst::Await(Box::new(JsAst::Call {
        callee: Box::new(iife),
        args: vec![],
    })))
}

/// `Loop { body }` — bounded feedback loop.
pub fn lower_loop(body: &Term) -> Result<JsAst, CompileError> {
    let param = input_ident();
    Ok(arrow_fn(lower_loop_applied(body, &param)?))
}

fn lower_loop_applied(body: &Term, input: &JsAst) -> Result<JsAst, CompileError> {
    let body_fn = super::lower(body)?;
    let saved = "__saved";
    let state = "__state";
    let cnt = "__cnt";
    let pair = "__pair";
    let out = "__out";
    // `JsConfig::loop_max_iterations` — the feedback bound the loop
    // emits as `__cnt < N` (default 100 000).
    let max_iterations = JsAst::Num(Number::from(loop_max_iterations()));

    let block = JsAst::Block(vec![
        let_decl(saved, input.clone()),
        let_decl(state, JsAst::Null),
        let_decl(cnt, JsAst::Num(Number::from(0))),
        JsAst::While {
            cond: Box::new(JsAst::BinOp {
                op: JsBinOp::Lt,
                left: Box::new(JsAst::Ident(cnt.to_string())),
                right: Box::new(max_iterations),
            }),
            body: Box::new(JsAst::Block(vec![
                assign(
                    cnt,
                    JsAst::BinOp {
                        op: JsBinOp::Add,
                        left: Box::new(JsAst::Ident(cnt.to_string())),
                        right: Box::new(JsAst::Num(Number::from(1))),
                    },
                ),
                let_decl(
                    pair,
                    rt_call(
                        "mkPair",
                        vec![
                            JsAst::Ident(saved.to_string()),
                            JsAst::Ident(state.to_string()),
                        ],
                    ),
                ),
                let_decl(
                    out,
                    JsAst::Await(Box::new(JsAst::Call {
                        callee: Box::new(body_fn),
                        args: vec![JsAst::Ident(pair.to_string())],
                    })),
                ),
                JsAst::If {
                    cond: Box::new(rt_call("isLeft", vec![JsAst::Ident(out.to_string())])),
                    then_branch: Box::new(JsAst::Block(vec![JsAst::Return(Box::new(rt_call(
                        "leftValue",
                        vec![JsAst::Ident(out.to_string())],
                    )))])),
                    else_branch: Some(Box::new(JsAst::If {
                        cond: Box::new(rt_call("isRight", vec![JsAst::Ident(out.to_string())])),
                        then_branch: Box::new(JsAst::Block(vec![assign(
                            state,
                            rt_call("rightValue", vec![JsAst::Ident(out.to_string())]),
                        )])),
                        else_branch: Some(Box::new(JsAst::If {
                            cond: Box::new(rt_call("isPair", vec![JsAst::Ident(out.to_string())])),
                            then_branch: Box::new(JsAst::Block(vec![JsAst::Return(Box::new(
                                rt_call("fst", vec![JsAst::Ident(out.to_string())]),
                            ))])),
                            else_branch: None,
                        })),
                    })),
                },
            ])),
        },
        JsAst::Return(Box::new(JsAst::Null)),
    ]);

    // The block holds `let`/`while` statements — legal only as a
    // function body. Applied in expression position it must run inside
    // an async IIFE (the loop body's `await`s stay legal).
    Ok(JsAst::Await(Box::new(JsAst::Call {
        callee: Box::new(JsAst::ArrowFn {
            params: Vec::new(),
            body: Box::new(block),
        }),
        args: Vec::new(),
    })))
}

/// `Iter { fuel, predicate, step, initial }` — bounded iteration.
pub fn lower_iter(
    fuel: &Term,
    predicate: &Term,
    step: &Term,
    initial: &Term,
) -> Result<JsAst, CompileError> {
    let param = input_ident();
    Ok(arrow_fn(lower_iter_applied(
        fuel, predicate, step, initial, &param,
    )?))
}

fn lower_iter_applied(
    fuel: &Term,
    predicate: &Term,
    step: &Term,
    initial: &Term,
    input: &JsAst,
) -> Result<JsAst, CompileError> {
    let fuel_val = super::lower_applied(fuel, input)?;
    let pred_fn = super::lower(predicate)?;
    let step_fn = super::lower(step)?;
    let init_val = super::lower_applied(initial, input)?;

    let state = "__state";
    let cnt = "__cnt";

    let block = JsAst::Block(vec![
        let_decl(state, init_val),
        let_decl(cnt, JsAst::Num(Number::from(0))),
        JsAst::While {
            cond: Box::new(JsAst::BinOp {
                op: JsBinOp::Lt,
                left: Box::new(JsAst::Ident(cnt.to_string())),
                right: Box::new(fuel_val),
            }),
            body: Box::new(JsAst::Block(vec![
                JsAst::If {
                    cond: Box::new(JsAst::UnaryOp {
                        op: JsUnaryOp::Not,
                        expr: Box::new(JsAst::Await(Box::new(JsAst::Call {
                            callee: Box::new(pred_fn),
                            args: vec![JsAst::Ident(state.to_string())],
                        }))),
                    }),
                    then_branch: Box::new(JsAst::Block(vec![JsAst::Return(Box::new(
                        JsAst::Ident(state.to_string()),
                    ))])),
                    else_branch: None,
                },
                assign(
                    state,
                    JsAst::Await(Box::new(JsAst::Call {
                        callee: Box::new(step_fn),
                        args: vec![JsAst::Ident(state.to_string())],
                    })),
                ),
                assign(
                    cnt,
                    JsAst::BinOp {
                        op: JsBinOp::Add,
                        left: Box::new(JsAst::Ident(cnt.to_string())),
                        right: Box::new(JsAst::Num(Number::from(1))),
                    },
                ),
            ])),
        },
        JsAst::Return(Box::new(JsAst::Ident(state.to_string()))),
    ]);

    // Same as `lower_loop_applied`: the statement block runs inside an
    // async IIFE so it is a valid expression-position value.
    Ok(JsAst::Await(Box::new(JsAst::Call {
        callee: Box::new(JsAst::ArrowFn {
            params: Vec::new(),
            body: Box::new(block),
        }),
        args: Vec::new(),
    })))
}

/// `App { callee, args }` — higher-order arrow application.
pub fn lower_app(callee: &Term, args: &[Term]) -> Result<JsAst, CompileError> {
    let param = input_ident();
    Ok(arrow_fn(lower_app_applied(callee, args, &param)?))
}

/// The full member path of a `field_access` callee chain rooted at a
/// host-namespace binding (`BoundRef::Member`), or `None` when the
/// callee isn't a host member path. `ns.f(a, b)` on a host package
/// applies the whole argument tuple — the eval `call` convention — so
/// the nested `App` chain flattens into one `ns["f"](a, b)`; the curried
/// `f(a)(b)` emit is reserved for arrow values (dep `Ns` namespaces
/// included).
fn host_member_callee(callee: &Term) -> Option<String> {
    let Term::Op {
        name, args, lit, ..
    } = callee
    else {
        return None;
    };
    if name != "field_access" {
        return None;
    }
    let field = lit.as_ref().and_then(Value::as_str)?;
    let recv = args.first().and_then(host_member_receiver)?;
    Some(format!("{recv}[{}]", js_string(field)))
}

/// The `__handl_rt` path a `field_access` receiver term resolves to —
/// an `Op{name}` bound to a host `Member` path, or a deeper
/// `field_access` chain over one. `Ns` (dep) receivers return `None`:
/// their members are compiled arrows, not host functions.
fn host_member_receiver(recv: &Term) -> Option<String> {
    let Term::Op {
        name, args, lit, ..
    } = recv
    else {
        return None;
    };
    if name == "field_access" {
        let field = lit.as_ref().and_then(Value::as_str)?;
        let base = args.first().and_then(host_member_receiver)?;
        return Some(format!("{base}[{}]", js_string(field)));
    }
    match super::module_bound(name) {
        Some(super::BoundRef::Member(path)) => Some(path),
        _ => None,
    }
}

/// `io.perform(op, arg)` — eval's `is_perform_callee` intercepts the
/// qualified callee at the host-function seam, so the qualified
/// spelling gets the same `__handl_rt.perform` emit as the flat
/// `perform` op (in-flight dispatch + residual `IO` mint). The
/// arrow-lowered surface form is an `App` chain whose innermost callee
/// is `field_access(<io>, "perform")`; `<io>` resolves to the io
/// module namespace when it is an unbound `io` var (eval's
/// module-name split reaches `io.perform` through `ModuleLookup` the
/// same way) or a name bound to the `__handl_rt.io` member path
/// (`import io` / `from "io" import * as io`).
fn is_io_receiver(recv: &Term) -> bool {
    let Term::Op { name, args, .. } = recv else {
        return false;
    };
    if !args.is_empty() {
        return false;
    }
    match super::module_bound(name) {
        Some(super::BoundRef::Member(path)) => path == "__handl_rt.io",
        Some(_) => false,
        None => name == "io",
    }
}

/// Whether `callee` is the `io.perform` member access —
/// `field_access(<io>, "perform")` per [`is_io_receiver`].
fn perform_member_callee(callee: &Term) -> bool {
    let Term::Op {
        name, args, lit, ..
    } = callee
    else {
        return false;
    };
    name == "field_access"
        && args.len() == 1
        && lit.as_ref().and_then(Value::as_str) == Some("perform")
        && is_io_receiver(&args[0])
}

/// Peel a left-nested `App` chain over an `io.perform` callee into the
/// concatenated argument list (innermost args first — the same order
/// [`flatten_host_member_app`] produces). `Some` only when the
/// innermost callee is [`perform_member_callee`].
fn flatten_perform_app<'t>(callee: &'t Term, args: &'t [Term]) -> Option<Vec<&'t Term>> {
    match callee {
        Term::App {
            callee: inner,
            args: inner_args,
        } => {
            let mut all = flatten_perform_app(inner, inner_args)?;
            all.extend(args.iter());
            Some(all)
        }
        _ => perform_member_callee(callee).then(|| args.iter().collect()),
    }
}

fn lower_app_applied(callee: &Term, args: &[Term], input: &JsAst) -> Result<JsAst, CompileError> {
    // `io.perform(op, arg)` — the qualified PerformFn spelling. Route
    // to `__handl_rt.perform` with the flat spelling's op-name operand
    // rule (a bare `puts` arg is the name string, not an `__opRef`
    // dispatch object). Awaited like every other async-arrow call.
    if let Some(all) = flatten_perform_app(callee, args) {
        let terms: Vec<Term> = all.iter().map(|t| (*t).clone()).collect();
        return Ok(JsAst::Await(Box::new(
            super::primitive::lower_perform_call(&terms, input)?,
        )));
    }
    // Flatten `App(App(f, a), b)` chains whose innermost callee is a
    // `field_access` over a host `Member` namespace: eval applies a
    // host function to the whole argument tuple, so `math.bit_or(a, b)`
    // emits `__handl_rt.math["bit_or"](a, b)` — not `…(a)(b)`, which is
    // the curried convention for arrow values.
    if let Some((path, first_args)) = flatten_host_member_app(callee, args) {
        let arg_exprs: Vec<JsAst> = first_args
            .iter()
            .map(|a| super::lower_applied(a, input))
            .collect::<Result<_, _>>()?;
        return Ok(JsAst::Call {
            callee: Box::new(JsAst::Raw(path)),
            args: arg_exprs,
        });
    }
    // A quoted-arrow callee (`Arr` whose payload is a serialized `Term` —
    // the `Term::App` contract, ADR-0120 Phase 1) is unquoted and lowered
    // to its JS arrow; the interpreter runs the inner term on the
    // arg-input, we emit the arrow itself.
    let callee_expr = match callee {
        Term::Arr { value, .. } => match serde_json::from_value::<Term>(value.clone()) {
            Ok(inner) => super::lower(&inner)?,
            Err(_) => super::lower_applied(callee, input)?,
        },
        _ => super::lower_applied(callee, input)?,
    };
    let arg_exprs: Vec<JsAst> = args
        .iter()
        .map(|a| super::lower_applied(a, input))
        .collect::<Result<_, _>>()?;
    // The `app` arg-input (term_interpreter): a lone argument is fed
    // bare, several pack into one array — the callee's `__in`.
    let mut arg_exprs = arg_exprs;
    let arg_input = if arg_exprs.len() == 1 {
        arg_exprs.pop().unwrap()
    } else {
        JsAst::Array(arg_exprs)
    };
    // Applying an arrow yields a Promise on this target (all emitted
    // arrows are async), so the call is awaited. `__apply` reinstates
    // `__carried__` handler frames a `handle` attached to the callee
    // value (deep dispatch, `attach_carried_handler` subset).
    Ok(JsAst::Await(Box::new(rt_call(
        "__apply",
        vec![callee_expr, arg_input],
    ))))
}

/// Peel a left-nested `App` chain — `App(App(base, inner), outer)` —
/// into the innermost callee and the concatenated argument list
/// (innermost args first, matching eval's left-to-right application
/// order). `Some` only when the base resolves to a host member path.
fn flatten_host_member_app<'t>(
    callee: &'t Term,
    args: &'t [Term],
) -> Option<(String, Vec<&'t Term>)> {
    match callee {
        Term::App {
            callee: inner,
            args: inner_args,
        } => {
            let (path, mut all) = flatten_host_member_app(inner, inner_args)?;
            all.extend(args.iter());
            Some((path, all))
        }
        _ => host_member_callee(callee).map(|path| (path, args.iter().collect())),
    }
}
