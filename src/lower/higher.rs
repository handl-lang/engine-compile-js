//! Lower the higher-order `Term` combinators (`Child`, `Handle`) to JS.

use handl_plugin_compile::CompileError;
use hnl_engine::Term;

use crate::JsAst;

const INPUT_NAME: &str = "__in";

fn input_ident() -> JsAst {
    JsAst::Ident(INPUT_NAME.to_string())
}

fn rt_call(name: &str, args: Vec<JsAst>) -> JsAst {
    JsAst::Call {
        callee: Box::new(JsAst::Member {
            obj: Box::new(JsAst::Ident("__handl_rt".to_string())),
            prop: name.to_string(),
        }),
        args,
    }
}

fn arrow_fn(body: JsAst) -> JsAst {
    JsAst::ArrowFn {
        params: vec![INPUT_NAME.to_string()],
        body: Box::new(body),
    }
}

/// Lower a higher-order combinator to a top-level JS arrow.
pub fn lower(term: &Term) -> Result<JsAst, CompileError> {
    match term {
        Term::Child { role } => lower_child(role),
        Term::Handle {
            body,
            target,
            impl_,
            acc,
        } => lower_handle(body, target, impl_, acc.as_deref()),
        _ => Err(CompileError::Lower(format!(
            "higher lowerer received non-higher term: {term:?}"
        ))),
    }
}

/// Lower a higher-order combinator applied to a concrete input expression.
pub fn lower_applied(term: &Term, input: &JsAst) -> Result<JsAst, CompileError> {
    match term {
        Term::Child { role } => Ok(lower_child_applied(role, input)),
        Term::Handle {
            body,
            target,
            impl_,
            acc,
        } => lower_handle_applied(body, target, impl_, acc.as_deref(), input),
        _ => Err(CompileError::Lower(format!(
            "higher lowerer received non-higher term: {term:?}"
        ))),
    }
}

fn lower_child(role: &str) -> Result<JsAst, CompileError> {
    Ok(arrow_fn(lower_child_applied(role, &input_ident())))
}

fn lower_child_applied(role: &str, input: &JsAst) -> JsAst {
    rt_call("child", vec![input.clone(), JsAst::Str(role.to_string())])
}

fn lower_handle(
    body: &Term,
    target: &str,
    impl_: &Term,
    acc: Option<&Term>,
) -> Result<JsAst, CompileError> {
    Ok(arrow_fn(lower_handle_applied(
        body,
        target,
        impl_,
        acc,
        &input_ident(),
    )?))
}

/// The arrow a `handle` body/impl scheme lowers to. A bare lazy op
/// reference (`Op{name, []}` — `handle arr Foo with h`'s `arr Foo`)
/// is the exception: eval yields the op-*reference* arrow value, and
/// `handle` calls `body(input)` expecting that arrow back (it attaches
/// the carried handler and returns it). The scheme reading would
/// *perform* the op instead, so the referent is emitted as a const
/// arrow `__in => <referent>` — the same shape `lower_op` produced
/// before lazy refs gained the apply reading.
fn handle_scheme(term: &Term) -> Result<JsAst, CompileError> {
    if let Term::Op {
        name, args, lit, ..
    } = term
        && args.is_empty()
        && lit.is_none()
        && super::primitive::is_lazy_op_ref(name)
    {
        let param = JsAst::Ident(INPUT_NAME.to_string());
        let referent = super::primitive::lower_applied(term, &param)?;
        return Ok(JsAst::ArrowFn {
            params: vec![INPUT_NAME.to_string()],
            body: Box::new(referent),
        });
    }
    crate::lower::lower(term)
}

/// A scheme term evaluated under `input`, wrapped in the `() => value`
/// thunk `__handl_rt.handle` invokes — the shared shape of the `impl`
/// and `acc` child emissions (both evaluate on the body's incoming).
fn scheme_thunk(term: &Term, input: &JsAst) -> Result<JsAst, CompileError> {
    let f = handle_scheme(term)?;
    let value = match &f {
        JsAst::ArrowFn { .. } => JsAst::Call {
            callee: Box::new(f.clone()),
            args: vec![input.clone()],
        },
        _ => f.clone(),
    };
    Ok(JsAst::ArrowFn {
        params: Vec::new(),
        body: Box::new(value),
    })
}

fn lower_handle_applied(
    body: &Term,
    target: &str,
    impl_: &Term,
    acc: Option<&Term>,
    input: &JsAst,
) -> Result<JsAst, CompileError> {
    let body_fn = handle_scheme(body)?;
    // `impl_` is a *scheme* evaluated under the same incoming as the
    // body (eval order: impl value first, then push, then body). The
    // lowered impl is an async arrow for scheme terms; a constant term
    // lowers to its value directly — the thunk yields either as the
    // handler value `handle` pushes.
    let impl_thunk = scheme_thunk(impl_, input)?;
    // ADR-0158 `acc`: the optional accumulator seed is evaluated on the
    // same incoming (eval order: impl, then `acc`, then push) and
    // passed as a thunk — `__handl_rt.handle` seeds the frame's
    // `__acc__` slot from it. `Term::Handle` only carries `acc` when
    // the `handle … with h acc s0` clause parsed; hand-built terms may
    // set it too.
    let acc_thunk = acc.map(|t| scheme_thunk(t, input)).transpose()?;
    // `await __handl_rt.handle(bodyFn, "target", implThunk, input,
    // accThunk?)` — the runtime pushes the handler (with the `__acc__`
    // slot when `acc` is present), runs `body` under it (ops route
    // through `__handl_rt.dispatch` as `(payload, s) ~> (result, s')`),
    // runs the `IO`-former post-pass and `return`/`acc` postlude, pops
    // in `finally`, and attaches the carried handler to arrow results.
    let mut args = vec![
        body_fn,
        JsAst::Str(target.to_string()),
        impl_thunk,
        input.clone(),
    ];
    if let Some(thunk) = acc_thunk {
        args.push(thunk);
    }
    Ok(JsAst::Await(Box::new(rt_call("handle", args))))
}
