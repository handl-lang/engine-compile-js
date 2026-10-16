//! Lower `plugin_engine::Term` into a target JavaScript AST.

mod control;
mod higher;
mod primitive;
mod proof;

use handl_plugin_compile::{CompileError, ValueAbi};
use plugin_engine::Term;
use serde_json::{Number, Value};

use crate::config::JsConfig;

thread_local! {
    static LOWER_CTX: std::cell::RefCell<LowerCtx> =
        std::cell::RefCell::new(LowerCtx::default());
}

/// How a module-scope name (import or top-level binding) resolves inside
/// a lowered `Op { name, .. }` term.
#[derive(Debug, Clone)]
pub(crate) enum BoundRef {
    /// A `const`/`export const`/ES-imported name: the binding is an async
    /// arrow taking the `app` arg-input (lone arg or `[args]` tuple).
    /// Value position emits the bare identifier; a call emits
    /// `(await name(arg_input))`.
    Arrow(String),
    /// A plain identifier reference (e.g. a namespace import `* as m`):
    /// value position emits the identifier; a call applies it with
    /// spread args (host-function convention).
    Ident(String),
    /// A `__handl_rt` member function — spread args, synchronous call.
    /// Used for the `js` contract-package shims (`call`, `invoke`,
    /// `error`); promise results are settled by the separate `await` op.
    Shim(String),
    /// A member path on `__handl_rt` (e.g. `__handl_rt.tag.tag_of` for a
    /// `from "tag" import { tag_of }` name): value position emits the
    /// path; a call applies it with spread args (host-function
    /// convention).
    Member(String),
    /// Route to a different native op name — `from "js" import { await
    /// as js_await }` maps `js_await` back to the real `await` op so the
    /// emitted code keeps the literal-`await` bridge.
    Op(String),
}

#[derive(Debug, Default)]
struct LowerCtx {
    no_inline_foreign: bool,
    /// Module-scope names resolvable inside arrow bodies — imports and
    /// top-level bindings, populated by [`crate::module`] for the
    /// file-level emit. Empty for single-term inputs.
    bound: std::collections::HashMap<String, BoundRef>,
}

/// Run `f` with the lowering context derived from `config`.
pub(crate) fn with_config<T>(config: &JsConfig, f: impl FnOnce() -> T) -> T {
    let prev = LOWER_CTX.with(|c| c.borrow().no_inline_foreign);
    LOWER_CTX.with(|c| {
        let mut ctx = LowerCtx::default();
        ctx.no_inline_foreign = config.no_inline_foreign;
        *c.borrow_mut() = ctx;
    });
    let ret = f();
    LOWER_CTX.with(|c| {
        c.borrow_mut().no_inline_foreign = prev;
        c.borrow_mut().bound.clear();
    });
    ret
}

/// Run `f` with `bound` visible as the module-scope name table.
pub(crate) fn with_module_env<T>(
    bound: std::collections::HashMap<String, BoundRef>,
    f: impl FnOnce() -> T,
) -> T {
    let prev = LOWER_CTX.with(|c| std::mem::take(&mut c.borrow_mut().bound));
    LOWER_CTX.with(|c| c.borrow_mut().bound = bound);
    let ret = f();
    LOWER_CTX.with(|c| c.borrow_mut().bound = prev);
    ret
}

/// Look up `name` in the module-scope binding table.
pub(crate) fn module_bound(name: &str) -> Option<BoundRef> {
    LOWER_CTX.with(|c| c.borrow().bound.get(name).cloned())
}

/// Whether foreign calls are allowed to be inlined in the current lowering run.
pub(crate) fn allow_inlining() -> bool {
    !LOWER_CTX.with(|c| c.borrow().no_inline_foreign)
}

/// A serializable JavaScript expression/statement AST.
#[derive(Debug, Clone, PartialEq)]
pub enum JsAst {
    /// The `null` literal.
    Null,
    /// A boolean literal.
    Bool(bool),
    /// A JSON number literal.
    Num(Number),
    /// A string literal.
    Str(String),
    /// An array literal.
    Array(Vec<JsAst>),
    /// An object literal.
    Object(Vec<(String, JsAst)>),
    /// An identifier.
    Ident(String),
    /// Member access (`a.b`).
    Member {
        /// The object being accessed.
        obj: Box<JsAst>,
        /// The property name.
        prop: String,
    },
    /// Index access (`a[b]`).
    Index {
        /// The object being indexed.
        obj: Box<JsAst>,
        /// The key/index expression.
        key: Box<JsAst>,
    },
    /// A function call.
    Call {
        /// The callee expression.
        callee: Box<JsAst>,
        /// The arguments.
        args: Vec<JsAst>,
    },
    /// An `await` expression.
    ///
    /// Every emitted arrow function is `async`, so `await` may appear
    /// anywhere an expression is legal; it is the bridge that hands a
    /// host promise (e.g. the one `js.await` bridges) back to the JS
    /// event loop for settlement.
    Await(Box<JsAst>),
    /// An arrow function.
    ArrowFn {
        /// Parameter names.
        params: Vec<String>,
        /// Body expression or block.
        body: Box<JsAst>,
    },
    /// A binary operation.
    BinOp {
        /// The operator.
        op: JsBinOp,
        /// Left operand.
        left: Box<JsAst>,
        /// Right operand.
        right: Box<JsAst>,
    },
    /// A unary operation.
    UnaryOp {
        /// The operator.
        op: JsUnaryOp,
        /// The operand.
        expr: Box<JsAst>,
    },
    /// An assignment expression / statement (`x = y`).
    Assign {
        /// Target identifier.
        name: String,
        /// New value.
        value: Box<JsAst>,
    },
    /// A `let` declaration.
    Let {
        /// Bound name.
        name: String,
        /// Initializer.
        value: Box<JsAst>,
    },
    /// A `const` declaration.
    ConstDecl {
        /// Bound name.
        name: String,
        /// Initializer.
        value: Box<JsAst>,
    },
    /// A `return` statement.
    Return(Box<JsAst>),
    /// An `if` / `if-else` statement.
    If {
        /// Condition expression.
        cond: Box<JsAst>,
        /// Then branch.
        then_branch: Box<JsAst>,
        /// Optional else branch.
        else_branch: Option<Box<JsAst>>,
    },
    /// A `while` loop.
    While {
        /// Condition expression.
        cond: Box<JsAst>,
        /// Loop body.
        body: Box<JsAst>,
    },
    /// A `for` loop.
    For {
        /// Initialization.
        init: Box<JsAst>,
        /// Condition.
        cond: Box<JsAst>,
        /// Update expression.
        update: Box<JsAst>,
        /// Body.
        body: Box<JsAst>,
    },
    /// A brace-delimited statement block.
    Block(Vec<JsAst>),
    /// A raw string snippet (runtime helpers, etc.).
    Raw(String),
    /// An ES `import` declaration (module-level emit only).
    Import {
        /// Module specifier string (emitted verbatim).
        spec: String,
        /// Named imports `(imported, local)` — `local == imported` emits
        /// `{ a }`, otherwise `{ a as c }`.
        named: Vec<(String, String)>,
        /// `import * as ns` namespace alias.
        namespace: Option<String>,
    },
    /// `export` of a declaration (`const`/`let`) — module-level emit.
    ExportDecl(Box<JsAst>),
    /// `export { a, b as c }` — re-export of bound names.
    ExportNames(Vec<(String, String)>),
    /// `export default <expr>`.
    ExportDefault(Box<JsAst>),
    /// A `//` line comment (newlines in the text are stripped).
    Comment(String),
}

/// JavaScript binary operators.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JsBinOp {
    /// `+`
    Add,
    /// `-`
    Sub,
    /// `*`
    Mul,
    /// `/`
    Div,
    /// `%`
    Mod,
    /// `===`
    Eq,
    /// `!==`
    NotEq,
    /// `<`
    Lt,
    /// `<=`
    Le,
    /// `>`
    Gt,
    /// `>=`
    Ge,
    /// `&&`
    And,
    /// `||`
    Or,
}

/// JavaScript unary operators.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JsUnaryOp {
    /// `-`
    Neg,
    /// `!`
    Not,
}

impl JsBinOp {
    /// The source token for this operator.
    pub fn as_str(&self) -> &'static str {
        match self {
            JsBinOp::Add => "+",
            JsBinOp::Sub => "-",
            JsBinOp::Mul => "*",
            JsBinOp::Div => "/",
            JsBinOp::Mod => "%",
            JsBinOp::Eq => "===",
            JsBinOp::NotEq => "!==",
            JsBinOp::Lt => "<",
            JsBinOp::Le => "<=",
            JsBinOp::Gt => ">",
            JsBinOp::Ge => ">=",
            JsBinOp::And => "&&",
            JsBinOp::Or => "||",
        }
    }
}

impl JsUnaryOp {
    /// The source token for this operator.
    pub fn as_str(&self) -> &'static str {
        match self {
            JsUnaryOp::Neg => "-",
            JsUnaryOp::Not => "!",
        }
    }
}

/// Lower any [`Term`] to a top-level JS arrow or value.
pub fn lower(term: &Term) -> Result<JsAst, CompileError> {
    match term {
        Term::Arr { .. } | Term::Const { .. } | Term::Op { .. } | Term::Seq { .. } => {
            primitive::lower(term)
        }
        Term::First { .. }
        | Term::Choice { .. }
        | Term::Loop { .. }
        | Term::Iter { .. }
        | Term::App { .. } => control::lower(term),
        Term::Child { .. } | Term::Handle { .. } => higher::lower(term),
        Term::Rewrite { .. } => proof::lower(term),
        _ => Err(CompileError::Lower(format!("unsupported term: {term:?}"))),
    }
}

/// Lower the application of a [`Term`] to an input value.
pub fn lower_applied(term: &Term, input: &JsAst) -> Result<JsAst, CompileError> {
    match term {
        Term::Arr { .. } | Term::Const { .. } | Term::Op { .. } | Term::Seq { .. } => {
            primitive::lower_applied(term, input)
        }
        Term::First { .. }
        | Term::Choice { .. }
        | Term::Loop { .. }
        | Term::Iter { .. }
        | Term::App { .. } => control::lower_applied(term, input),
        Term::Child { .. } | Term::Handle { .. } => higher::lower_applied(term, input),
        Term::Rewrite { .. } => proof::lower_applied(term, input),
        _ => Err(CompileError::Lower(format!("unsupported term: {term:?}"))),
    }
}

/// Lower a [`Value`] through the shared [`ValueAbi`] layout.
pub(crate) fn lower_value(value: &Value) -> Result<JsAst, CompileError> {
    let abi = ValueAbi::from_json(value)?;
    lower_value_abi(&abi)
}

fn lower_value_abi(value: &ValueAbi) -> Result<JsAst, CompileError> {
    match value {
        ValueAbi::Null => Ok(JsAst::Null),
        ValueAbi::Bool(b) => Ok(JsAst::Bool(*b)),
        ValueAbi::Number(n) => Ok(JsAst::Num(n.clone())),
        ValueAbi::String(s) => Ok(JsAst::Str(s.clone())),
        ValueAbi::Array(items) => {
            let lowered = items
                .iter()
                .map(lower_value_abi)
                .collect::<Result<Vec<_>, _>>()?;
            Ok(JsAst::Array(lowered))
        }
        ValueAbi::Object(map) => {
            let mut props = Vec::with_capacity(map.len());
            for (k, v) in map {
                props.push((k.clone(), lower_value_abi(v)?));
            }
            Ok(JsAst::Object(props))
        }
        ValueAbi::Bytes(_) => Ok(JsAst::Null),
        ValueAbi::Pair(a, b) => Ok(JsAst::Call {
            callee: Box::new(JsAst::Member {
                obj: Box::new(JsAst::Ident("__handl_rt".to_string())),
                prop: "mkPair".to_string(),
            }),
            args: vec![lower_value_abi(a)?, lower_value_abi(b)?],
        }),
        ValueAbi::EitherLeft(v) => Ok(JsAst::Call {
            callee: Box::new(JsAst::Member {
                obj: Box::new(JsAst::Ident("__handl_rt".to_string())),
                prop: "mkLeft".to_string(),
            }),
            args: vec![lower_value_abi(v)?],
        }),
        ValueAbi::EitherRight(v) => Ok(JsAst::Call {
            callee: Box::new(JsAst::Member {
                obj: Box::new(JsAst::Ident("__handl_rt".to_string())),
                prop: "mkRight".to_string(),
            }),
            args: vec![lower_value_abi(v)?],
        }),
        // Arrows and handles are not expected as primitive constant values.
        ValueAbi::Arrow(_) | ValueAbi::Handle(_) | ValueAbi::Resource { .. } => Ok(JsAst::Null),
    }
}
