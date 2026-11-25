//! Lower the proof `Term` combinators to JS.
//!
//! Only `Rewrite` reaches this module: it is the only proof shape in the
//! closed `Term` vocabulary. `Path` / `Equiv` (with `Refine`, `CellId`,
//! `JElim`, `Whisker`, …) are *derived* operators that ADR-0119 §7 folded into
//! `Term::Opaque`, so they arrive as opaque ADR-0103 records at the appropriate
//! dimension rather than as variants — there is nothing left to lower here.
//! Dispatched from `lower::lower` / `lower::lower_applied`.

use handl_plugin_compile::CompileError;
use hnl_engine::{Term, term::RewriteWitness};

use crate::JsAst;

/// Lower a proof combinator to a top-level JS arrow.
pub fn lower(term: &Term) -> Result<JsAst, CompileError> {
    match term {
        Term::Rewrite { witness, .. } => Err(rewrite_unsupported(witness)),
        _ => Err(CompileError::Lower(format!(
            "proof lowerer received non-proof term: {term:?}"
        ))),
    }
}

/// Lower a proof combinator applied to a concrete input expression.
pub fn lower_applied(term: &Term, _input: &JsAst) -> Result<JsAst, CompileError> {
    match term {
        Term::Rewrite { witness, .. } => Err(rewrite_unsupported(witness)),
        _ => Err(CompileError::Lower(format!(
            "proof lowerer received non-proof term: {term:?}"
        ))),
    }
}

/// `Term::Rewrite` has no eval semantics to port — every evaluator
/// (`term_interpreter`, tree-walk, `nbe`) returns `NotImplemented` — so
/// the honest lowering is a compile error, not a fabricated result.
fn rewrite_unsupported(witness: &RewriteWitness) -> CompileError {
    CompileError::Lower(format!(
        "rewrite (~>) with {} witness is not supported by the JS backend \
         (eval returns NotImplemented for Term::Rewrite)",
        witness_name(witness)
    ))
}

fn witness_name(witness: &RewriteWitness) -> String {
    match witness {
        RewriteWitness::Named(s) => s.clone(),
        RewriteWitness::Compute => "compute".to_string(),
        RewriteWitness::HandlerHint => "handler-hint".to_string(),
        RewriteWitness::Cast => "cast".to_string(),
    }
}
