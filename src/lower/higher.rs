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
        } => lower_handle(body, target, impl_),
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
        } => lower_handle_applied(body, target, impl_, input),
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

fn lower_handle(body: &Term, target: &str, impl_: &Term) -> Result<JsAst, CompileError> {
    Ok(arrow_fn(lower_handle_applied(
        body,
        target,
        impl_,
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

fn lower_handle_applied(
    body: &Term,
    target: &str,
    impl_: &Term,
    input: &JsAst,
) -> Result<JsAst, CompileError> {
    let body_fn = handle_scheme(body)?;
    let impl_fn = handle_scheme(impl_)?;
    // `impl_` is a *scheme* evaluated under the same incoming as the
    // body (eval order: impl value first, then push, then body). The
    // lowered impl is an async arrow for scheme terms; a constant term
    // lowers to its value directly — the thunk yields either as the
    // handler value `handle` pushes.
    let impl_value = match &impl_fn {
        JsAst::ArrowFn { .. } => JsAst::Call {
            callee: Box::new(impl_fn.clone()),
            args: vec![input.clone()],
        },
        _ => impl_fn.clone(),
    };
    let impl_thunk = JsAst::ArrowFn {
        params: Vec::new(),
        body: Box::new(impl_value),
    };
    // `await __handl_rt.handle(bodyFn, "target", implThunk, input)` —
    // the runtime pushes the handler, runs `body` under it (ops route
    // through `__handl_rt.dispatch`), runs the `IO`-former post-pass and
    // `return` clause, pops in `finally`, and attaches the carried
    // handler to arrow results.
    Ok(JsAst::Await(Box::new(rt_call(
        "handle",
        vec![
            body_fn,
            JsAst::Str(target.to_string()),
            impl_thunk,
            input.clone(),
        ],
    ))))
}
