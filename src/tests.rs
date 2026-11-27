use handl_language::{Ast, AstNode, Language};
use handl_plugin_compile::{ArtifactContent, Compiler, CompilerInput, CompilerOutput};
use hnl_engine::term::RewriteWitness;
use hnl_engine::{ArrSource, Term};
use hnl_tool_typecheck::TypecheckReport;
use rand::Rng;
use serde_json::{Number, Value, json};

use crate::security::{is_safe_for_inlining, validate_foreign_descriptor};
use crate::{
    Browser, Bun, Iife, Inline, JsCompiler, JsConfig, JsOutput, JsTarget, compile_to_string,
};

fn input(term: Term) -> CompilerInput {
    let source = "test".to_string();
    let mut ast = Ast::new();
    let root = ast.push(AstNode::new("module"));
    let language = Language::new();
    let report = TypecheckReport::default();
    CompilerInput::new(source, ast, root, language, term, report)
}

#[test]
fn compiler_target_is_js() {
    assert_eq!(JsCompiler.target(), "js");
}

#[test]
fn const_null() {
    let js = compile_to_string(&input(Term::constant(Value::Null)), &JsConfig::esm_deno()).unwrap();
    assert!(js.contains("export default null;"), "{js}");
}

#[test]
fn const_bool() {
    let js = compile_to_string(&input(Term::constant(true)), &JsConfig::esm_deno()).unwrap();
    assert!(js.contains("export default true;"), "{js}");
}

#[test]
fn const_int() {
    let js = compile_to_string(
        &input(Term::constant(Value::Number(Number::from(42)))),
        &JsConfig::esm_deno(),
    )
    .unwrap();
    assert!(js.contains("export default 42;"), "{js}");
}

#[test]
fn const_float() {
    let js = compile_to_string(
        &input(Term::constant(Value::Number(
            Number::from_f64(1.25).unwrap(),
        ))),
        &JsConfig::esm_deno(),
    )
    .unwrap();
    assert!(js.contains("export default 1.25;"), "{js}");
}

#[test]
fn const_string() {
    let js = compile_to_string(
        &input(Term::constant(Value::String("hello".to_string()))),
        &JsConfig::esm_deno(),
    )
    .unwrap();
    assert!(js.contains(r#"export default "hello";"#), "{js}");
}

#[test]
fn arr_constant_value() {
    let js = compile_to_string(
        &input(Term::const_value(Value::Number(Number::from(42)))),
        &JsConfig::esm_deno(),
    )
    .unwrap();
    assert!(js.contains("=> 42"), "{js}");
    assert!(js.contains("__in"), "{js}");
}

#[test]
fn op_add() {
    let term = Term::op_native(
        "add",
        vec![
            Term::const_value(Value::Number(Number::from(1))),
            Term::const_value(Value::Number(Number::from(2))),
        ],
    );
    let js = compile_to_string(&input(term), &JsConfig::esm_deno()).unwrap();
    assert!(js.contains("1 + 2"), "{js}");
}

#[test]
fn op_proj_env() {
    let term = Term::op_native_lit("proj_env", vec![], Value::String("x".to_string()));
    let js = compile_to_string(&input(term), &JsConfig::esm_deno()).unwrap();
    assert!(js.contains("__handl_rt.projEnv"), "{js}");
    assert!(js.contains(r#""x""#), "{js}");
}

#[test]
fn seq_constant_arrows() {
    let term = Term::seq(
        Term::const_value(Value::Number(Number::from(1))),
        Term::const_value(Value::Number(Number::from(2))),
    );
    let js = compile_to_string(&input(term), &JsConfig::esm_deno()).unwrap();
    assert!(js.contains("=>"), "{js}");
    assert!(js.contains("2"), "{js}");
}

#[test]
fn esm_wrapping() {
    let js = compile_to_string(
        &input(Term::const_value(Value::Number(Number::from(1)))),
        &JsConfig::esm_deno(),
    )
    .unwrap();
    assert!(js.contains("export default"), "{js}");
    assert!(js.contains("var __handl_rt"), "{js}");
}

#[test]
fn commonjs_wrapping() {
    let js = compile_to_string(
        &input(Term::const_value(Value::Number(Number::from(1)))),
        &JsConfig::cjs_node(),
    )
    .unwrap();
    assert!(js.contains("module.exports ="), "{js}");
    assert!(js.contains("var __handl_rt"), "{js}");
}

#[test]
fn first_on_pair() {
    let term = Term::first(Term::op_native_lit(
        "proj_env",
        vec![],
        Value::String("x".to_string()),
    ));
    let js = compile_to_string(&input(term), &JsConfig::esm_deno()).unwrap();
    assert!(js.contains("__handl_rt.fst"), "{js}");
    assert!(js.contains("__handl_rt.snd"), "{js}");
    assert!(js.contains("__handl_rt.mkPair"), "{js}");
    assert!(js.contains("=>"), "{js}");
}

#[test]
fn choice_on_either() {
    let term = Term::choice(
        Term::op_native_lit("proj_env", vec![], Value::String("a".to_string())),
        Term::op_native_lit("proj_env", vec![], Value::String("b".to_string())),
    );
    let js = compile_to_string(&input(term), &JsConfig::esm_deno()).unwrap();
    assert!(js.contains("__handl_rt.isLeft"), "{js}");
    assert!(js.contains("__handl_rt.leftValue"), "{js}");
    assert!(js.contains("__handl_rt.rightValue"), "{js}");
    assert!(js.contains("if ("), "{js}");
}

#[test]
fn loop_emits_while() {
    let term = Term::loop_feedback(Term::const_value(Value::Number(Number::from(42))));
    let js = compile_to_string(&input(term), &JsConfig::esm_deno()).unwrap();
    assert!(js.contains("while ("), "{js}");
    assert!(js.contains("__handl_rt.isLeft"), "{js}");
    assert!(js.contains("__handl_rt.isRight"), "{js}");
    assert!(js.contains("__handl_rt.isPair"), "{js}");
    assert!(js.contains("100000"), "{js}");
}

#[test]
fn iter_emits_bounded_loop() {
    let term = Term::Iter {
        fuel: Box::new(Term::const_value(Value::Number(Number::from(10)))),
        predicate: Box::new(Term::const_value(Value::Bool(true))),
        step: Box::new(Term::const_value(Value::Number(Number::from(1)))),
        initial: Box::new(Term::const_value(Value::Number(Number::from(0)))),
    };
    let js = compile_to_string(&input(term), &JsConfig::esm_deno()).unwrap();
    assert!(js.contains("while ("), "{js}");
    assert!(js.contains("__state"), "{js}");
    assert!(js.contains("__cnt"), "{js}");
    assert!(js.contains("return __state"), "{js}");
}

#[test]
fn app_emits_call() {
    let term = Term::app(
        Term::const_value(Value::String("fn".to_string())),
        vec![Term::const_value(Value::Number(Number::from(1)))],
    );
    let js = compile_to_string(&input(term), &JsConfig::esm_deno()).unwrap();
    // Applications route through `__apply` (carried-handler reinstall).
    assert!(js.contains(r#"__handl_rt.__apply("fn", 1)"#), "{js}");
    assert!(js.contains("__in"), "{js}");
}

#[test]
fn combined_first_then_loop() {
    let term = Term::seq(
        Term::first(Term::const_value(Value::Number(Number::from(1)))),
        Term::loop_feedback(Term::const_value(Value::Number(Number::from(2)))),
    );
    let js = compile_to_string(&input(term), &JsConfig::esm_deno()).unwrap();
    assert!(js.contains("__handl_rt.fst"), "{js}");
    assert!(js.contains("__handl_rt.mkPair"), "{js}");
    assert!(js.contains("while ("), "{js}");
}

#[test]
fn combined_first_then_choice() {
    let term = Term::seq(
        Term::first(Term::op_native_lit(
            "proj_env",
            vec![],
            Value::String("x".to_string()),
        )),
        Term::choice(
            Term::op_native_lit("proj_env", vec![], Value::String("l".to_string())),
            Term::op_native_lit("proj_env", vec![], Value::String("r".to_string())),
        ),
    );
    let js = compile_to_string(&input(term), &JsConfig::esm_deno()).unwrap();
    assert!(js.contains("__handl_rt.fst"), "{js}");
    assert!(js.contains("__handl_rt.isLeft"), "{js}");
    assert!(js.contains("__handl_rt.mkPair"), "{js}");
}

#[test]
fn child_lowering() {
    let term = Term::Child {
        role: "body".to_string(),
    };
    let js = compile_to_string(&input(term), &JsConfig::esm_deno()).unwrap();
    assert!(js.contains("__handl_rt.child"), "{js}");
    assert!(js.contains(r#""body""#), "{js}");
}

#[test]
fn handle_lowering() {
    let term = Term::handle(
        Term::const_value(Value::Number(Number::from(1))),
        "cap",
        Term::const_value(Value::Number(Number::from(2))),
    );
    let js = compile_to_string(&input(term), &JsConfig::esm_deno()).unwrap();
    assert!(js.contains("__handl_rt.handle"), "{js}");
    assert!(js.contains(r#""cap""#), "{js}");
}

#[test]
fn rewrite_lowering() {
    // `Term::Rewrite` has no eval semantics (every evaluator returns
    // `NotImplemented`), so the honest lowering is a compile error —
    // never a fabricated `__handl_rt.rewrite` call.
    let term = Term::Rewrite {
        source: Box::new(Term::const_value(Value::Number(Number::from(1)))),
        target: Box::new(Term::const_value(Value::Number(Number::from(2)))),
        witness: RewriteWitness::Named("foo".to_string()),
    };
    let err = compile_to_string(&input(term), &JsConfig::esm_deno()).unwrap_err();
    assert!(format!("{err}").contains("rewrite"), "{err}");
}

#[test]
fn bun_target_name() {
    assert_eq!(Bun.name(), "bun");
}

#[test]
fn browser_target_name() {
    assert_eq!(Browser.name(), "browser");
}

#[test]
fn iife_wrapping() {
    let config = JsConfig {
        target: Box::new(Browser),
        format: Box::new(Iife),
        debug: false,
        no_inline_foreign: false,
        source_map: false,
        modules: Vec::new(),
        entry_imports: Vec::new(),
        worker_entry: false,
        op_lowering: crate::OpLowering::Mixed,
        loop_max_iterations: crate::DEFAULT_LOOP_MAX_ITERATIONS,
        security_scan: true,
        unsafe_inline_foreign: false,
        dispatch_all_ops: false,
    };
    let js = compile_to_string(
        &input(Term::const_value(Value::Number(Number::from(42)))),
        &config,
    )
    .unwrap();
    assert!(js.starts_with("(function"), "{js}");
    assert!(js.contains("return async __in => 42"), "{js}");
    assert!(js.contains("var __handl_rt"), "{js}");
}

#[test]
fn inline_format() {
    let config = JsConfig {
        target: Box::new(Browser),
        format: Box::new(Inline),
        debug: false,
        no_inline_foreign: false,
        source_map: false,
        modules: Vec::new(),
        entry_imports: Vec::new(),
        worker_entry: false,
        op_lowering: crate::OpLowering::Mixed,
        loop_max_iterations: crate::DEFAULT_LOOP_MAX_ITERATIONS,
        security_scan: true,
        unsafe_inline_foreign: false,
        dispatch_all_ops: false,
    };
    let js = compile_to_string(
        &input(Term::const_value(Value::Number(Number::from(42)))),
        &config,
    )
    .unwrap();
    assert!(!js.contains("export"), "{js}");
    assert!(!js.contains("module.exports"), "{js}");
    assert!(js.contains("42"), "{js}");
}

fn artifact_to_string(content: &ArtifactContent) -> String {
    match content {
        ArtifactContent::String(s) => s.clone(),
        ArtifactContent::Bytes(b) => String::from_utf8_lossy(b).to_string(),
    }
}

#[test]
fn source_map_enabled() {
    let mut config = JsConfig::esm_deno();
    config.source_map = true;
    let output = JsCompiler
        .compile(&input(Term::constant(42)), &config)
        .unwrap();
    assert_eq!(output.artifacts().len(), 2);

    let js = artifact_to_string(&output.artifacts()[0].content);
    assert!(js.contains("//# sourceMappingURL=main.js.map"), "{js}");

    let map = artifact_to_string(&output.artifacts()[1].content);
    let parsed: serde_json::Value = serde_json::from_str(&map).unwrap();
    assert_eq!(parsed["version"], 3);
    assert!(!parsed["sources"].as_array().unwrap().is_empty());
    assert!(!parsed["mappings"].as_str().unwrap().is_empty());
    assert_eq!(parsed["sourcesContent"].as_array().unwrap().len(), 1);
}

#[test]
fn source_map_disabled() {
    let config = JsConfig::esm_deno();
    let output = JsCompiler
        .compile(&input(Term::constant(42)), &config)
        .unwrap();
    assert_eq!(output.artifacts().len(), 1);
    let js = artifact_to_string(&output.artifacts()[0].content);
    assert!(!js.contains("//# sourceMappingURL"), "{js}");
}

#[test]
fn safe_foreign_descriptor_validates_and_inlines() {
    let desc = json!({
        "module": "math",
        "name": "add",
        "arg_types": ["number", "number"],
        "return_type": "number"
    });
    let fd = validate_foreign_descriptor(&desc).unwrap();
    assert!(is_safe_for_inlining(&fd));

    let term = Term::Op {
        dim: Box::new(Term::constant(Value::Number(Number::from(1)))),
        name: "add".to_string(),
        args: vec![
            Term::constant(Value::Number(Number::from(1))),
            Term::constant(Value::Number(Number::from(2))),
        ],
        source: ArrSource::Foreign("math".to_string()),
        lit: Some(desc),
        attrs: None,
    };
    let js = compile_to_string(&input(term), &JsConfig::esm_deno()).unwrap();
    // Foreign ops route through `dispatchForeign` (T-008: handler
    // interception parity) — a safe descriptor selects the `inline`
    // flag, whose uncovered fallback is the prelude's
    // `globalThis[module][name](...)` call.
    assert!(js.contains(r#"__handl_rt.dispatchForeign("add""#), "{js}");
    assert!(js.contains(r#""math", null, true)"#), "{js}");
    assert!(js.contains("globalThis[module][name]"), "{js}");
}

#[test]
fn unsafe_foreign_descriptor_rejected_by_validation() {
    let desc = json!({
        "module": "x",
        "name": "eval",
        "arg_types": [],
        "return_type": "void"
    });
    assert!(validate_foreign_descriptor(&desc).is_err());
}

#[test]
fn no_inline_foreign_emits_dispatch_foreign_fallback() {
    let desc = json!({
        "module": "math",
        "name": "add",
        "arg_types": ["number", "number"],
        "return_type": "number"
    });
    let mut config = JsConfig::esm_deno();
    config.no_inline_foreign = true;
    let term = Term::Op {
        dim: Box::new(Term::constant(Value::Number(Number::from(1)))),
        name: "add".to_string(),
        args: vec![
            Term::constant(Value::Number(Number::from(1))),
            Term::constant(Value::Number(Number::from(2))),
        ],
        source: ArrSource::Foreign("math".to_string()),
        lit: Some(desc),
        attrs: None,
    };
    let js = compile_to_string(&input(term), &config).unwrap();
    // `dispatchForeign` carries the validated descriptor JSON and the
    // `inline = false` flag — its uncovered fallback is `jsCall`.
    assert!(js.contains(r#"__handl_rt.dispatchForeign("add""#), "{js}");
    assert!(js.contains(r#""math", {"#), "{js}");
    assert!(js.contains(r#""module":"math""#), "{js}");
    assert!(js.contains(", false)"), "{js}");
}

#[test]
fn generated_js_has_no_dangerous_call_patterns() {
    let js = compile_to_string(
        &input(Term::constant(Value::Number(Number::from(42)))),
        &JsConfig::esm_deno(),
    )
    .unwrap();
    assert!(!js.contains("eval("), "{js}");
    assert!(!js.contains("new Function("), "{js}");
    assert!(!js.contains("Function("), "{js}");
}

#[test]
fn arrows_emit_async_and_applications_are_awaited() {
    // Every emitted arrow is `async`; applying one yields a Promise the
    // caller awaits — the uniform rule that makes `js.await` honest.
    let term = Term::op_native(
        "add",
        vec![
            Term::const_value(Value::Number(Number::from(1))),
            Term::const_value(Value::Number(Number::from(2))),
        ],
    );
    let js = compile_to_string(&input(term), &JsConfig::esm_deno()).unwrap();
    assert!(js.contains("async __in =>"), "{js}");

    let app = Term::app(
        Term::const_value(Value::String("fn".to_string())),
        vec![Term::const_value(Value::Number(Number::from(1)))],
    );
    let js = compile_to_string(&input(app), &JsConfig::esm_deno()).unwrap();
    assert!(js.contains("await"), "{js}");
    assert!(js.contains(r#"__handl_rt.__apply("fn", 1)"#), "{js}");
}

#[test]
fn await_op_emits_real_await() {
    // `js.await` (the promise bridge) lowers to a JS `await` inside an
    // async arrow — both for the `Native` spelling the arrow lowerer
    // produces and for a `Foreign("js")` host-boundary spelling.
    let native = Term::Op {
        dim: Term::default_dim(),
        name: "await".to_string(),
        args: vec![Term::op_native("id", vec![])],
        source: ArrSource::Native,
        lit: None,
        attrs: None,
    };
    let js = compile_to_string(&input(native), &JsConfig::esm_deno()).unwrap();
    assert!(js.contains("async __in => (await"), "{js}");
    assert!(js.contains("__handl_rt.id"), "{js}");

    let foreign = Term::Op {
        dim: Term::default_dim(),
        name: "await".to_string(),
        args: vec![Term::op_native("id", vec![])],
        source: ArrSource::Foreign("js".to_string()),
        lit: None,
        attrs: None,
    };
    let js = compile_to_string(&input(foreign), &JsConfig::esm_deno()).unwrap();
    assert!(js.contains("async __in => (await"), "{js}");
    assert!(!js.contains(r#"jsCall("js""#), "{js}");
}

#[test]
fn await_op_requires_exactly_one_arg() {
    let term = Term::Op {
        dim: Term::default_dim(),
        name: "await".to_string(),
        args: vec![],
        source: ArrSource::Native,
        lit: None,
        attrs: None,
    };
    assert!(compile_to_string(&input(term), &JsConfig::esm_deno()).is_err());
}

#[test]
fn await_op_application_is_awaited() {
    // `js_await(p)` applied in a `Seq` chain: the op's arrow is invoked
    // and the (async) result awaited.
    let term = Term::seq(
        Term::op_native("id", vec![]),
        Term::Op {
            dim: Term::default_dim(),
            name: "await".to_string(),
            args: vec![Term::op_native("id", vec![])],
            source: ArrSource::Native,
            lit: None,
            attrs: None,
        },
    );
    let js = compile_to_string(&input(term), &JsConfig::esm_deno()).unwrap();
    assert!(js.contains("async __in =>"), "{js}");
    assert!(js.contains("await"), "{js}");
}

#[test]
fn dangerous_foreign_op_rejected_at_compile_time() {
    let desc = json!({
        "module": "x",
        "name": "eval",
        "arg_types": [],
        "return_type": "void"
    });
    let term = Term::Op {
        dim: Box::new(Term::constant(Value::Number(Number::from(1)))),
        name: "eval".to_string(),
        args: vec![],
        source: ArrSource::Foreign("x".to_string()),
        lit: Some(desc),
        attrs: None,
    };
    assert!(compile_to_string(&input(term), &JsConfig::esm_deno()).is_err());
}

#[test]
fn fuzz_random_terms_compile() {
    let mut rng = rand::rng();
    for _ in 0..50 {
        let term = random_term(&mut rng, 3);
        if let Ok(js) = compile_to_string(&input(term), &JsConfig::esm_deno()) {
            assert!(!js.is_empty());
            assert!(!js.contains("eval("), "{js}");
            assert!(!js.contains("new Function("), "{js}");
            assert!(!js.contains("Function("), "{js}");
        }
    }
}

#[test]
fn js_output_stream_writes_source() {
    let output = JsOutput::new("export default 42;");
    let mut buf = Vec::new();
    output.stream(&mut buf).unwrap();
    assert_eq!(String::from_utf8(buf).unwrap(), "export default 42;");
}

#[test]
fn js_forbid_unsafe_code() {
    let js = compile_to_string(
        &input(Term::constant(Value::Number(Number::from(42)))),
        &JsConfig::esm_deno(),
    )
    .unwrap();
    assert!(!js.contains("eval("), "{js}");
    assert!(!js.contains("new Function("), "{js}");
    assert!(!js.contains("Function("), "{js}");
}

#[test]
fn js_value_bounds_check() {
    let js = compile_to_string(
        &input(Term::loop_feedback(Term::constant(Value::Number(
            Number::from(42),
        )))),
        &JsConfig::esm_deno(),
    )
    .unwrap();
    assert!(js.contains("__handl_rt.isLeft"), "{js}");
    assert!(js.contains("__handl_rt.isRight"), "{js}");
    assert!(js.contains(r#"__either__ === "left""#), "{js}");
    assert!(js.contains(r#"__either__ === "right""#), "{js}");
}

// ---------------------------------------------------------------------------
// File-level module emit — real `.hnl` sources over the canonical language.
// ---------------------------------------------------------------------------

/// The canonical language for module-emit tests (dev-dep cycle is allowed).
fn canonical_lang() -> &'static Language {
    static LANG: std::sync::OnceLock<Language> = std::sync::OnceLock::new();
    LANG.get_or_init(|| {
        handl_kit_data_prelude::language_from_yaml().expect("canonical language assembles")
    })
}

fn module_input(source: &str) -> CompilerInput {
    let mut ast = Ast::new();
    let root = ast.push(AstNode::new("module"));
    CompilerInput::new(
        source.to_string(),
        ast,
        root,
        canonical_lang().clone(),
        Term::constant(Value::Null),
        TypecheckReport::default(),
    )
}

#[test]
fn module_exports_and_internal_bindings() {
    let js = compile_to_string(
        &module_input(
            "export plus := (a, b) ~> a + b\nhelper := (x) ~> x * 2\nexport main := (n) ~> helper(plus(n, 1))\n",
        ),
        &JsConfig::esm_deno(),
    )
    .unwrap();
    assert!(js.contains("export const plus = async __in =>"), "{js}");
    assert!(js.contains("const helper = async __in =>"), "{js}");
    assert!(js.contains("export const main = async __in =>"), "{js}");
    assert!(js.contains("__handl_rt.elem(__in, 0)"), "{js}");
    assert!(js.contains("+"), "{js}");
    // Cross-statement references emit the bound JS name through
    // `__apply` (carried-handler reinstall) with the `app` arg-input
    // (the arg tuple).
    assert!(js.contains("await __handl_rt.__apply(helper, "), "{js}");
    assert!(js.contains("__handl_rt.__apply(plus, ["), "{js}");
    assert!(!js.contains("__handl_rt.export"), "{js}");
}

#[test]
fn module_imports_emit_es_and_shims() {
    let js = compile_to_string(
        &module_input(
            "from \"../pkgs/foo\" import { a, b as c }\nfrom \"js\" import { await as js_await, invoke as js_invoke }\nexport run := (x) ~> js_await(js_invoke(x, \"m\", [], [], \"js_object\", false))\n",
        ),
        &JsConfig::esm_deno(),
    )
    .unwrap();
    assert!(
        js.contains("import { a, b as c } from \"../pkgs/foo\";"),
        "{js}"
    );
    // The bare `js` contract package binds to `__handl_rt` shims — no ES
    // import for it.
    assert!(!js.contains("from \"js\""), "{js}");
    assert!(js.contains("__handl_rt.invoke("), "{js}");
    assert!(js.contains("(await "), "{js}");
}

#[test]
fn module_js_construct_on_off_bind_to_shims() {
    // `construct`/`on`/`off` are `js` contract ops: any name imported
    // `from "js"` binds `__handl_rt.<name>` — the prelude supplies the
    // shims (new-expression, event trampoline, removeEventListener).
    let js = compile_to_string(
        &module_input(
            "from \"js\" import { construct as js_construct, on as js_on, off as js_off }\nexport run := (w, h) ~> do {\n  c := js_construct(\"\", \"Worker\", [\"w.js\"], [], \"js_object\", false)\n  l := js_on(w, \"message\", h)\n  js_off(w, \"message\", l)\n}\n",
        ),
        &JsConfig::esm_deno(),
    )
    .unwrap();
    assert!(js.contains("__handl_rt.construct("), "{js}");
    assert!(js.contains("__handl_rt.on("), "{js}");
    assert!(js.contains("__handl_rt.off("), "{js}");
    // The prelude shims themselves ship in the output.
    assert!(js.contains("Reflect.construct"), "{js}");
    assert!(js.contains("addEventListener"), "{js}");
    assert!(js.contains("removeEventListener"), "{js}");
}

#[test]
fn module_js_get_set_bind_to_shims() {
    // `get`/`set` are `js` contract ops too: the import binds
    // `__handl_rt.get`/`__handl_rt.set` — the prelude members do the
    // property read/write (dotted-path walk, `__lastError` convention,
    // promise-boxed reads).
    let js = compile_to_string(
        &module_input(
            "from \"js\" import { get as js_get, set as js_set }\nexport run := (o, k, v) ~> do {\n  js_set(o, k, v)\n  js_get(o, k)\n}\n",
        ),
        &JsConfig::esm_deno(),
    )
    .unwrap();
    assert!(js.contains("__handl_rt.set("), "{js}");
    assert!(js.contains("__handl_rt.get("), "{js}");
    // The prelude members themselves ship in the output.
    assert!(js.contains("__handl_promise__"), "{js}");
    assert!(js.contains("__lastError"), "{js}");
}

#[test]
fn module_js_import_module_binds_to_shim() {
    // `import_module` is the `js` contract's dynamic `import(spec)` op —
    // `import()` is syntax, not a member call, so `call`/`invoke` cannot
    // reach it. The import binds `__handl_rt.import_module`, the prelude
    // member that emits a real `import(spec)` promise-boxed for
    // `js.await` (the `.hnl`-reachable spelling `lib-js`'s worker shim
    // uses for `worker_threads` instead of `eval`).
    let js = compile_to_string(
        &module_input(
            "from \"js\" import { import_module as js_import_module, await as js_await }\nexport run := (spec) ~> js_await(js_import_module(spec))\n",
        ),
        &JsConfig::esm_deno(),
    )
    .unwrap();
    assert!(js.contains("__handl_rt.import_module("), "{js}");
    // The prelude shim ships `import(spec)` wrapped in `__promiseBox`.
    assert!(js.contains("__promiseBox(import(spec))"), "{js}");
    // No ES import is emitted for the `js` contract package.
    assert!(!js.contains("from \"js\""), "{js}");
}

#[test]
fn module_js_zero_arg_shim_value_position_calls() {
    // I-001: `e := js_error()` in value position must *invoke* the shim —
    // a bare `__handl_rt.error` member reads as the function object, so
    // `e == null` never holds and `lib-js`'s `spawn_code` returned null
    // unconditionally. Scheme position (`u >>> js_error`) keeps the
    // referent + `__appOrValue` reading.
    let js = compile_to_string(
        &module_input(
            "from \"js\" import { error as js_error }\nexport run := () ~> do {\n  e := js_error()\n  e == null\n}\n",
        ),
        &JsConfig::esm_deno(),
    )
    .unwrap();
    // The value-position read is a call, not the bare member.
    assert!(js.contains("__handl_rt.error()"), "{js}");
    assert!(!js.contains("(await __handl_rt.error),"), "{js}");
    // `x >>> js_error` still applies the referent to the incoming.
    let js2 = compile_to_string(
        &module_input(
            "from \"js\" import { error as js_error }\nerr_now := (u) ~> u >>> js_error\n",
        ),
        &JsConfig::esm_deno(),
    )
    .unwrap();
    assert!(
        js2.contains("__handl_rt.__appOrValue(__handl_rt.error,"),
        "{js2}"
    );
}

#[test]
fn module_effect_statements_and_default_export() {
    let js = compile_to_string(
        &module_input("x := (a) ~> a + 1\nx(2)\n"),
        &JsConfig::esm_deno(),
    )
    .unwrap();
    assert!(js.contains("const x = async __in =>"), "{js}");
    // The trailing expression statement runs for effect at import time
    // and its result is the default export.
    assert!(js.contains("export default"), "{js}");
    assert!(js.contains("await __handl_rt.__apply(x, 2)"), "{js}");
}

#[test]
fn module_resolved_deps_inline_into_handl_rt_namespaces() {
    // The driver resolved `from "dep-pkg" import { f }` to a file: the
    // dep compiles once into a `const __mod_*` bound under
    // `__handl_rt["dep-pkg"]`, and `f` resolves to its member path with
    // arrow-call semantics (`await`ed, packed arg input).
    let mut config = JsConfig::esm_deno();
    config.modules = vec![crate::JsModule {
        path: "/pkgs/dep/dep.hnl".to_string(),
        source: "export f := (x) ~> x + 1\nexport g := (x) ~> f(x)\n".to_string(),
        imports: Vec::new(),
    }];
    config.entry_imports = vec![("dep-pkg".to_string(), "/pkgs/dep/dep.hnl".to_string())];
    let js = compile_to_string(
        &module_input("from \"dep-pkg\" import { f }\nexport main := (x) ~> f(x)\n"),
        &config,
    )
    .unwrap();
    assert!(
        js.contains("const __mod_dep_0 = (await (async () => {"),
        "{js}"
    );
    assert!(js.contains(r#"return {"f": f, "g": g};"#), "{js}");
    assert!(
        js.contains(r#"__handl_rt["dep-pkg"] = __mod_dep_0;"#),
        "{js}"
    );
    assert!(
        js.contains(r#"__handl_rt.__apply(__handl_rt["dep-pkg"].f, "#),
        "{js}"
    );
    // No dead ES import or stub-member reference is emitted for it.
    assert!(!js.contains("import { f } from"), "{js}");
}

#[test]
fn module_dep_transitive_imports_and_no_export_fallback() {
    // `dep` imports `./inner` (resolved against the dep's own dir) and
    // `inner` declares no exports — the record projects every bound
    // name (`module_prep` parity).
    let mut config = JsConfig::esm_deno();
    config.modules = vec![
        crate::JsModule {
            path: "/pkgs/dep/dep.hnl".to_string(),
            source: "from \"./inner\" import { base }\nexport f := (x) ~> base(x)\n".to_string(),
            imports: vec![("./inner".to_string(), "/pkgs/dep/inner.hnl".to_string())],
        },
        crate::JsModule {
            path: "/pkgs/dep/inner.hnl".to_string(),
            source: "base := (x) ~> x * 2\n".to_string(),
            imports: Vec::new(),
        },
    ];
    config.entry_imports = vec![("dep".to_string(), "/pkgs/dep/dep.hnl".to_string())];
    let js = compile_to_string(
        &module_input("from \"dep\" import { f }\nexport main := (x) ~> f(x)\n"),
        &config,
    )
    .unwrap();
    // `inner` is emitted before `dep` (DFS post-order) and its record
    // projects the bound `base`.
    let inner = js.find("const __mod_inner").expect("{js}");
    let dep = js.find("const __mod_dep").expect("{js}");
    assert!(inner < dep, "{js}");
    assert!(js.contains(r#"return {"base": base};"#), "{js}");
    assert!(
        js.contains(r#"__handl_rt["./inner"] = __mod_inner_0;"#),
        "{js}"
    );
    assert!(
        js.contains(r#"__handl_rt.__apply(__handl_rt["./inner"].base, "#),
        "{js}"
    );
}

#[test]
fn module_circular_dep_errors_like_the_loader() {
    // A dep importing itself by a second spec is a circular import —
    // the same error `ModuleLoader` reports at eval time.
    let mut config = JsConfig::esm_deno();
    config.modules = vec![crate::JsModule {
        path: "/pkgs/a/a.hnl".to_string(),
        source: "from \"./self\" import { f }\nexport g := (x) ~> f(x)\n".to_string(),
        imports: vec![("./self".to_string(), "/pkgs/a/a.hnl".to_string())],
    }];
    config.entry_imports = vec![("a".to_string(), "/pkgs/a/a.hnl".to_string())];
    let err = compile_to_string(
        &module_input("from \"a\" import { g }\nexport main := (x) ~> g(x)\n"),
        &config,
    )
    .unwrap_err();
    assert!(err.to_string().contains("circular"), "{err}");
}

#[test]
fn module_unbound_names_fall_back_to_runtime_members() {
    let js = compile_to_string(
        &module_input("f := (x) ~> unknown_host(x)\n"),
        &JsConfig::esm_deno(),
    )
    .unwrap();
    // Unbound names emit a native op call — routed through `dispatch`,
    // whose uncovered fallback resolves the `__handl_rt.<name>` member
    // (or throws "unsupported op") at run time.
    assert!(js.contains(r#"__handl_rt.dispatch("unknown_host""#), "{js}");
}

#[test]
fn module_signature_export_binds_null_instead_of_dangling_export() {
    // `export f: T ~> U` is a signature — module_prep erases it to
    // `null`; the emit must produce a real binding (`export let f =
    // null`), never `export { f }` with no `f` in scope (a parse-time
    // SyntaxError under node).
    let js = compile_to_string(
        &module_input("export f: Nat ~> Nat\n"),
        &JsConfig::esm_deno(),
    )
    .unwrap();
    assert!(js.contains("export let f = null;"), "{js}");
    assert!(!js.contains("export { f }"), "{js}");
}

#[test]
fn module_signature_then_impl_reassigns_the_let() {
    // `f : T` binds `let f = null`; the later `f := impl` reassigns it
    // (no duplicate `const`) and calls see the implementation.
    let js = compile_to_string(
        &module_input("f : Nat ~> Number\nf := (n) ~> n + 1\nf(1)\n"),
        &JsConfig::esm_deno(),
    )
    .unwrap();
    assert!(js.contains("let f = null;"), "{js}");
    assert!(js.contains("f = async"), "{js}");
    assert!(!js.contains("const f ="), "{js}");
}

#[test]
fn module_export_name_resolves_a_later_binding() {
    // `export f` before `f := …` — the deferred export resolves once
    // every statement is walked.
    let js = compile_to_string(
        &module_input("export f\nf := (x) ~> x + 1\n"),
        &JsConfig::esm_deno(),
    )
    .unwrap();
    assert!(js.contains("const f ="), "{js}");
    assert!(js.contains("export { f };"), "{js}");
}

#[test]
fn module_data_decl_emits_constructor_bindings() {
    // `Nat := { Zero : Nat, Succ : (pred : Nat) ~> Nat }` desugars to a
    // `__splice__`d block of former/ctor assigns — every name must bind
    // at module scope (`const Zero`, `const Succ`, the `Nat` former and
    // the `Nat.rec` recursor as a flat `const Nat_rec` bound under the
    // dotted name), not vanish into a comment.
    let js = compile_to_string(
        &module_input(
            "Nat := { Zero : Nat, Succ : (pred : Nat) ~> Nat }\ntwo := Succ(Succ(Zero))\n",
        ),
        &JsConfig::esm_deno(),
    )
    .unwrap();
    assert!(js.contains("const Zero ="), "{js}");
    assert!(js.contains("const Succ ="), "{js}");
    assert!(js.contains("const Nat_rec ="), "{js}");
    assert!(js.contains("\"rec\": (await Nat_rec)"), "{js}");
    assert!(!js.contains("`data` declaration"), "{js}");
}

#[test]
fn module_unsupported_data_shape_errors() {
    // The retired `data` declaration keyword no longer parses — the
    // canonical family form is `X := new Inductive { … }`. Both the
    // bare `data F := …` spelling and `export data F …` fail loudly
    // rather than silently dropping the declaration.
    compile_to_string(
        &module_input("data Bad := C(Bad ~> Nat)\n"),
        &JsConfig::esm_deno(),
    )
    .unwrap_err();
    compile_to_string(
        &module_input("export data Nat := Zero | Succ(Nat)\n"),
        &JsConfig::esm_deno(),
    )
    .unwrap_err();
    // `export F := new Inductive { … }` desugars to the family decl
    // group under the `stmt.export` — it compiles and exports the
    // family name only: the family record carries its constructors
    // (`module_prep` parity; `F.rec` is dotted and ctor names are not
    // export-surface names either).
    let js = compile_to_string(
        &module_input(
            "export Nat := new Inductive { Zero : Nat, Succ : (pred : Nat) ~> Nat }\n",
        ),
        &JsConfig::esm_deno(),
    )
    .expect("export new Inductive compiles");
    assert!(
        js.contains("export { Nat }"),
        "family name must be exported:\n{js}"
    );
    for name in ["Zero", "Succ"] {
        assert!(
            !js.contains(&format!("export {{ {name} }}")),
            "{name}:\n{js}"
        );
    }
}

// ---------------------------------------------------------------------------
// T-004: worker-entry bundles — ESM-only, module-only, self-contained.
// ---------------------------------------------------------------------------

#[test]
fn worker_entry_rejects_commonjs_format() {
    // `commonjs` emits `module.exports = …`, which does not exist in a
    // worker global scope — only `esm` (module worker) and
    // `iife`/`inline` (classic worker script) can carry a worker entry.
    let mut config = JsConfig::cjs_node();
    config.worker_entry = true;
    let err = compile_to_string(&module_input("x := 1\n"), &config).unwrap_err();
    assert!(err.to_string().contains("esm"), "{err}");
    assert!(err.to_string().contains("commonjs"), "{err}");
}

#[test]
fn worker_entry_iife_emits_classic_worker_script() {
    // `worker_entry` + `iife`: a self-contained classic-script bundle —
    // no `import`/`export` declarations, async IIFE wrapper (the body
    // uses `await` for inlined deps / applied expressions).
    let mut config = JsConfig {
        target: Box::new(Browser),
        format: Box::new(Iife),
        debug: false,
        no_inline_foreign: false,
        source_map: false,
        modules: Vec::new(),
        entry_imports: Vec::new(),
        worker_entry: true,
        op_lowering: crate::OpLowering::Mixed,
        loop_max_iterations: crate::DEFAULT_LOOP_MAX_ITERATIONS,
        security_scan: true,
        unsafe_inline_foreign: false,
        dispatch_all_ops: false,
    };
    config.modules = vec![crate::JsModule {
        path: "/pkgs/dep/dep.hnl".to_string(),
        source: "export double := (x) ~> x * 2\n".to_string(),
        imports: Vec::new(),
    }];
    config.entry_imports = vec![("dep".to_string(), "/pkgs/dep/dep.hnl".to_string())];
    let js = compile_to_string(
        &module_input("from \"dep\" import { double }\necho := (m) ~> double(m)\n"),
        &config,
    )
    .unwrap();
    assert!(js.starts_with("(async function()"), "{js}");
    assert!(js.contains("const __mod_dep_0"), "{js}");
    // No ES module syntax survives (the prelude mentions `import` in
    // comments/`import(spec)`, so match declaration forms only).
    assert!(!js.contains("export"), "{js}");
    assert!(!js.contains("import { "), "{js}");
    assert!(!js.contains("import * as"), "{js}");
    assert!(!js.contains("module.exports"), "{js}");
}

#[test]
fn worker_entry_rejects_single_term_input() {
    // A synthetic `CompilerInput` (empty `Language`) cannot take the
    // file-level module emit — there is no module surface to wire.
    let mut config = JsConfig::esm_deno();
    config.worker_entry = true;
    let err = compile_to_string(&input(Term::constant(Value::Null)), &config).unwrap_err();
    assert!(err.to_string().contains("module source"), "{err}");
}

#[test]
fn worker_entry_rejects_unresolvable_imports() {
    let mut config = JsConfig::esm_deno();
    config.worker_entry = true;
    // `./dep.hnl` was not resolved through `entry_imports`, so a real ES
    // `import` survives — unresolvable inside a blob/object-URL worker.
    let err = compile_to_string(
        &module_input("from \"./dep.hnl\" import { f }\nf(1)\n"),
        &config,
    )
    .unwrap_err();
    assert!(err.to_string().contains("worker_entry"), "{err}");
    assert!(err.to_string().contains("./dep.hnl"), "{err}");
}

#[test]
fn worker_entry_keeps_builtin_imports_and_inlines_deps() {
    // `node:` builtins resolve inside a worker global scope and stay as
    // real ES imports; resolved `.hnl` deps inline into `__handl_rt`
    // namespaces — the bundle carries no unresolvable specifier.
    let mut config = JsConfig::esm_deno();
    config.worker_entry = true;
    config.modules = vec![crate::JsModule {
        path: "/pkgs/dep/dep.hnl".to_string(),
        source: "export double := (x) ~> x * 2\n".to_string(),
        imports: Vec::new(),
    }];
    config.entry_imports = vec![("dep".to_string(), "/pkgs/dep/dep.hnl".to_string())];
    let js = compile_to_string(
        &module_input(
            "from \"node:worker_threads\" import { parentPort }\n\
             from \"dep\" import { double }\n\
             echo := (msg) ~> double(msg)\n",
        ),
        &config,
    )
    .unwrap();
    assert!(
        js.contains("import { parentPort } from \"node:worker_threads\";"),
        "{js}"
    );
    assert!(js.contains("__handl_rt.dep = __mod_dep_0;"), "{js}");
    assert!(
        js.contains("__handl_rt.__apply(__handl_rt.dep.double, "),
        "{js}"
    );
    assert!(!js.contains("from \"dep\""), "{js}");
    // Prelude + inlined dep + entry body — one self-contained file.
    assert!(js.contains("var __handl_rt"), "{js}");
    assert!(js.contains("const __mod_dep_0"), "{js}");
}

// ---------------------------------------------------------------------------
// Runtime-op coverage: `record_merge`, `flow`, `ctor_name`, `ctor_fields`,
// `assert`/`runtime_assert`, and the `__opRef` unbound-reference guard.
// ---------------------------------------------------------------------------

#[test]
fn op_record_merge_emits_prelude_call() {
    let term = Term::op_native(
        "record_merge",
        vec![
            Term::const_value(json!({"a": 1})),
            Term::const_value(json!({"b": 2})),
        ],
    );
    let js = compile_to_string(&input(term), &JsConfig::esm_deno()).unwrap();
    // `record_merge` is a `Native` op with no dedicated match arm — it
    // routes through `dispatch`, whose uncovered fallback calls the
    // `__handl_rt.record_merge` member.
    assert!(js.contains(r#"__handl_rt.dispatch("record_merge""#), "{js}");
}

#[test]
fn op_flow_emits_prelude_call() {
    let term = Term::op_native(
        "flow",
        vec![
            Term::const_value(Value::Number(Number::from(1))),
            Term::const_value(Value::Number(Number::from(2))),
        ],
    );
    let js = compile_to_string(&input(term), &JsConfig::esm_deno()).unwrap();
    // `flow` has no dedicated match arm — as a `Native` op it routes
    // through `dispatch`, whose uncovered fallback calls the
    // `__handl_rt.flow` member.
    assert!(js.contains(r#"__handl_rt.dispatch("flow""#), "{js}");
}

#[test]
fn op_ctor_name_and_fields_read_the_incoming_value() {
    // The `Seq[pv, Op]` form carries no args — the ops read `__in`
    // (the `is_obj`/`tag_fields` convention); the `native_ops` first-arg
    // fallback lowers the arg term instead.
    let term = Term::op_native("ctor_name", vec![]);
    let js = compile_to_string(&input(term), &JsConfig::esm_deno()).unwrap();
    assert!(js.contains("__handl_rt.ctor_name(__in)"), "{js}");

    let term = Term::op_native("ctor_fields", vec![Term::op_native("id", vec![])]);
    let js = compile_to_string(&input(term), &JsConfig::esm_deno()).unwrap();
    assert!(js.contains("__handl_rt.ctor_fields("), "{js}");
}

#[test]
fn op_assert_and_runtime_assert_emit_calls_not_members() {
    // Both ops arrive arg-less (`Op{assert, []}`/`Op{runtime_assert, []}`)
    // but must *call* the throwing shim — a bare `__handl_rt.<name>`
    // member would evaluate to `undefined`, not the eval-faithful error.
    for name in ["assert", "runtime_assert"] {
        let term = Term::op_native(name, vec![]);
        let js = compile_to_string(&input(term), &JsConfig::esm_deno()).unwrap();
        assert!(js.contains(&format!("__handl_rt.{name}()")), "{name}: {js}");
    }
}

#[test]
fn op_unknown_zero_arg_reference_uses_opref() {
    // A bare `Op{name, []}` is an unbound-var/lazy op reference: `__opRef`
    // resolves real runtime members and throws "unsupported op" for
    // unknown names — never a silent `undefined` member access.
    let term = Term::op_native("mystery_op", vec![]);
    let js = compile_to_string(&input(term), &JsConfig::esm_deno()).unwrap();
    assert!(js.contains(r#"__handl_rt.__opRef("mystery_op")"#), "{js}");
}

#[test]
fn prelude_defines_the_requested_shims() {
    let js = compile_to_string(&input(Term::constant(Value::Null)), &JsConfig::esm_deno()).unwrap();
    for shim in [
        "record_merge:",
        "flow:",
        "ctor_name:",
        "ctor_fields:",
        "assert:",
        "runtime_assert:",
        "__opRef:",
    ] {
        assert!(js.contains(shim), "missing {shim}: {js}");
    }
}

#[test]
fn module_object_spread_lowers_to_record_merge() {
    let js = compile_to_string(
        &module_input("o := { b: 2 }\nmerged := { a: 1, ...o, c: 3 }\nmerged\n"),
        &JsConfig::esm_deno(),
    )
    .unwrap();
    assert!(js.contains(r#"__handl_rt.dispatch("record_merge""#), "{js}");
}

#[test]
fn module_flow_between_held_arrows_lowers_to_flow_op() {
    // `mk(1) >>> mk(2)` — call results are held *values*, so the
    // `eval.flow` dispatch (`Op{flow}`) is emitted rather than `Seq`.
    let js = compile_to_string(
        &module_input("mk := (x) ~> (y) ~> x + y\ncomp := mk(1) >>> mk(2)\ncomp(5)\n"),
        &JsConfig::esm_deno(),
    )
    .unwrap();
    assert!(js.contains(r#"__handl_rt.dispatch("flow""#), "{js}");
}

// ---------------------------------------------------------------------------
// T-005: non-ESM module formats — `commonjs`/`iife`/`inline` module emit.
// ---------------------------------------------------------------------------

/// IIFE output on the Browser target.
fn iife_browser() -> JsConfig {
    JsConfig {
        target: Box::new(Browser),
        format: Box::new(Iife),
        debug: false,
        no_inline_foreign: false,
        source_map: false,
        modules: Vec::new(),
        entry_imports: Vec::new(),
        worker_entry: false,
        op_lowering: crate::OpLowering::Mixed,
        loop_max_iterations: crate::DEFAULT_LOOP_MAX_ITERATIONS,
        security_scan: true,
        unsafe_inline_foreign: false,
        dispatch_all_ops: false,
    }
}

/// Inline output on the Browser target.
fn inline_browser() -> JsConfig {
    JsConfig {
        target: Box::new(Browser),
        format: Box::new(Inline),
        debug: false,
        no_inline_foreign: false,
        source_map: false,
        modules: Vec::new(),
        entry_imports: Vec::new(),
        worker_entry: false,
        op_lowering: crate::OpLowering::Mixed,
        loop_max_iterations: crate::DEFAULT_LOOP_MAX_ITERATIONS,
        security_scan: true,
        unsafe_inline_foreign: false,
        dispatch_all_ops: false,
    }
}

#[test]
fn module_commonjs_exports_become_awaited_namespace() {
    // `export` decls strip to plain bindings; the namespace object —
    // named exports plus `"default"` — is returned from an async IIFE
    // assigned to `module.exports`, so `require()` yields a Promise.
    let js = compile_to_string(
        &module_input("export plus := (a, b) ~> a + b\nplus(20, 22)\n"),
        &JsConfig::cjs_node(),
    )
    .unwrap();
    assert!(js.contains("module.exports = (async () => {"), "{js}");
    assert!(js.contains("const plus = async __in =>"), "{js}");
    assert!(js.contains(r#""plus": plus"#), "{js}");
    assert!(js.contains(r#""default":"#), "{js}");
    assert!(js.contains("return {"), "{js}");
    assert!(!js.contains("export "), "{js}");
    assert!(!js.contains("import { "), "{js}");
}

#[test]
fn module_commonjs_lowers_surviving_imports_to_require() {
    // CJS has a real module loader: an unresolved path specifier and a
    // `node:` builtin both become `require()` bindings — the same
    // specifier-resolution contract ESM imports have (the embedder's
    // concern), never a silent global.
    let js = compile_to_string(
        &module_input(
            "from \"node:path\" import { join }\nimport \"./setup.js\"\nexport run := (x) ~> join(x)\n",
        ),
        &JsConfig::cjs_node(),
    )
    .unwrap();
    assert!(
        js.contains(r#"const { "join": join } = require("node:path")"#),
        "{js}"
    );
    assert!(js.contains(r#"require("./setup.js");"#), "{js}");
    assert!(js.contains(r#""run": run"#), "{js}");
}

#[test]
fn module_commonjs_inlined_deps_still_inline() {
    // Dep inlining is format-independent: the `__mod_*` const +
    // `__handl_rt[spec]` binding are emitted inside the async wrapper.
    let mut config = JsConfig::cjs_node();
    config.modules = vec![crate::JsModule {
        path: "/pkgs/dep/dep.hnl".to_string(),
        source: "export double := (x) ~> x * 2\n".to_string(),
        imports: Vec::new(),
    }];
    config.entry_imports = vec![("dep".to_string(), "/pkgs/dep/dep.hnl".to_string())];
    let js = compile_to_string(
        &module_input("from \"dep\" import { double }\nexport quad := (x) ~> double(double(x))\n"),
        &config,
    )
    .unwrap();
    assert!(
        js.contains("const __mod_dep_0 = (await (async () => {"),
        "{js}"
    );
    // `dep` is a valid JS ident → `.dep` member path (bracket form is
    // for kebab specs like `dep-pkg`).
    assert!(js.contains("__handl_rt.dep = __mod_dep_0;"), "{js}");
    assert!(js.contains(r#""quad": quad"#), "{js}");
    assert!(!js.contains("from \"dep\""), "{js}");
}

#[test]
fn module_iife_returns_namespace_promise() {
    // IIFE: a self-contained `(async function () { … })();` — the
    // prelude lives inside the wrapper; the script evaluates to a
    // Promise of the export namespace.
    let js = compile_to_string(
        &module_input("export inc := (x) ~> x + 1\ninc(41)\n"),
        &iife_browser(),
    )
    .unwrap();
    assert!(js.starts_with("(async function() {"), "{js}");
    assert!(js.contains("var __handl_rt"), "{js}");
    assert!(js.contains(r#""inc": inc"#), "{js}");
    assert!(js.contains(r#""default":"#), "{js}");
    assert!(!js.contains("export"), "{js}");
    assert!(!js.contains("module.exports"), "{js}");
}

#[test]
fn module_inline_emits_classic_script() {
    // Inline: prelude + a bare async-IIFE expression statement — no
    // module header at all; the completion value is the namespace
    // promise.
    let js = compile_to_string(
        &module_input("export dbl := (x) ~> x * 2\ndbl(21)\n"),
        &inline_browser(),
    )
    .unwrap();
    assert!(js.contains("var __handl_rt"), "{js}");
    assert!(js.contains("(async () => {"), "{js}");
    assert!(js.contains(r#""dbl": dbl"#), "{js}");
    assert!(js.contains(r#""default":"#), "{js}");
    assert!(!js.contains("export"), "{js}");
    assert!(!js.contains("module.exports"), "{js}");
}

#[test]
fn module_iife_and_inline_reject_external_imports() {
    // A classic script has no module loader — not even `node:` builtins
    // — so any surviving ES `import` is a hard compile error, while
    // `commonjs` lowers the same source to `require()` (covered above).
    for (name, config) in [("iife", iife_browser()), ("inline", inline_browser())] {
        let err = compile_to_string(
            &module_input("from \"node:path\" import { join }\njoin(\"a\")\n"),
            &config,
        )
        .unwrap_err();
        assert!(
            err.to_string().contains("cannot be expressed"),
            "{name}: {err}"
        );
        assert!(err.to_string().contains("node:path"), "{name}: {err}");
    }
    // `commonjs` accepts the same source.
    compile_to_string(
        &module_input("from \"node:path\" import { join }\njoin(\"a\")\n"),
        &JsConfig::cjs_node(),
    )
    .unwrap();
}

#[test]
fn module_non_esm_export_name_reexport_and_signature() {
    // `export f` deferred + `export { g }`-style pending exports and a
    // signature export all land in the namespace object.
    let js = compile_to_string(
        &module_input(
            "export f\nf := (x) ~> x + 1\nexport sig : Nat ~> Nat\nexport run := (x) ~> f(x)\n",
        ),
        &JsConfig::cjs_node(),
    )
    .unwrap();
    assert!(js.contains(r#""f": f"#), "{js}");
    assert!(js.contains(r#""sig": sig"#), "{js}");
    assert!(js.contains(r#""run": run"#), "{js}");
    assert!(js.contains("let sig = null;"), "{js}");
}

fn random_term<R: Rng>(rng: &mut R, depth: usize) -> Term {
    if depth == 0 {
        let v = match rng.random_range(0..4) {
            0 => Value::Null,
            1 => Value::Bool(rng.random()),
            2 => Value::Number(Number::from(rng.random_range(-100..100))),
            3 => Value::String(format!("s{}", rng.random::<u32>())),
            _ => Value::Null,
        };
        return Term::constant(v);
    }
    match rng.random_range(0..5) {
        0 | 1 => random_term(rng, depth - 1),
        2 => {
            let a = random_term(rng, depth - 1);
            let b = random_term(rng, depth - 1);
            Term::seq(a, b)
        }
        3 => {
            let a = Term::constant(Value::Number(Number::from(rng.random_range(-100..100))));
            let b = Term::constant(Value::Number(Number::from(rng.random_range(-100..100))));
            Term::op_native("add", vec![a, b])
        }
        4 => Term::op_native_lit(
            "proj_env",
            vec![],
            Value::String(format!("f{}", rng.random::<u32>())),
        ),
        _ => random_term(rng, depth - 1),
    }
}

// ---------------------------------------------------------------------------
// T-006 (P5a/P5b): effect-semantics parity — `handle` runtime call shape,
// `dispatch` routing for unknown ops, and the op-spelling denylist.
// ---------------------------------------------------------------------------

#[test]
fn handle_lowering_emits_runtime_handle_call() {
    // `Term::Handle` lowers to `await __handl_rt.handle(bodyFn, target,
    // implThunk, input)` — the runtime owns the handler stack, dispatch,
    // IO-former post-pass, `return` clause, and carried-handler attach.
    let term = Term::handle(
        Term::op_native(
            "Foo",
            vec![Term::const_value(Value::Number(Number::from(1)))],
        ),
        "Foo",
        Term::op_native_lit(
            "record",
            vec![Term::op_native(
                "add",
                vec![
                    Term::const_value(Value::Number(Number::from(1))),
                    Term::const_value(Value::Number(Number::from(2))),
                ],
            )],
            json!(["Foo"]),
        ),
    );
    let js = compile_to_string(&input(term), &JsConfig::esm_deno()).unwrap();
    assert!(js.contains("__handl_rt.handle("), "{js}");
    assert!(js.contains(r#""Foo""#), "{js}");
}

#[test]
fn handle_acc_lowering_emits_seed_thunk() {
    // ADR-0158 `acc`: `Term::Handle{acc: Some(s0)}` lowers to a fifth
    // `__handl_rt.handle` argument — a `() => s0` thunk the runtime
    // evaluates on the body's incoming to seed the frame's `__acc__`
    // slot. `Term::handle` (no `acc`) keeps the 4-argument call.
    let term = Term::handle_acc(
        Term::const_value(Value::Number(Number::from(1))),
        "cap",
        Term::const_value(Value::Number(Number::from(2))),
        Term::const_value(Value::Number(Number::from(0))),
    );
    let js = compile_to_string(&input(term), &JsConfig::esm_deno()).unwrap();
    assert!(js.contains("__handl_rt.handle("), "{js}");
    // The acc thunk is the trailing (5th) argument: `…, __in,
    // async () => <seed>(__in))`.
    assert!(js.contains(", __in, async () =>"), "{js}");
    let plain = Term::handle(
        Term::const_value(Value::Number(Number::from(1))),
        "cap",
        Term::const_value(Value::Number(Number::from(2))),
    );
    let js_plain = compile_to_string(&input(plain), &JsConfig::esm_deno()).unwrap();
    assert!(!js_plain.contains(", __in, async () =>"), "{js_plain}");
}

#[test]
fn unknown_op_with_args_lowers_to_dispatch() {
    // Ops outside the builtin match route through
    // `__handl_rt.dispatch("name", [args])` so a covering `handle` frame
    // can intercept them (eval's `dispatch_op`).
    let term = Term::op_native(
        "HttpGet",
        vec![Term::const_value(Value::String("u".to_string()))],
    );
    let js = compile_to_string(&input(term), &JsConfig::esm_deno()).unwrap();
    assert!(js.contains(r#"__handl_rt.dispatch("HttpGet""#), "{js}");
}

#[test]
fn perform_lowers_to_runtime_perform_call() {
    // `perform`/`io.perform` are supported spellings (the io module's
    // `PerformFn` builtin): they emit `__handl_rt.perform(op, arg)` —
    // in-flight handler dispatch plus the residual `type.io` former
    // mint — for the flat native op, the qualified native name, and
    // the foreign `io` spelling. The bare op-name operand lowers to
    // its name string (eval's unbound-var rule), not `__opRef`.
    for (term, expect) in [
        (
            Term::op_native(
                "perform",
                vec![
                    Term::op_native("Foo", vec![]),
                    Term::const_value(Value::Number(Number::from(1))),
                ],
            ),
            r#"__handl_rt.perform("Foo", 1)"#,
        ),
        (
            Term::op_native(
                "perform",
                vec![
                    Term::const_value(Value::String("Foo".to_string())),
                    Term::const_value(Value::Number(Number::from(1))),
                ],
            ),
            r#"__handl_rt.perform("Foo", 1)"#,
        ),
        (Term::op_native("io.perform", vec![]), "__handl_rt.perform"),
        (
            Term::Op {
                dim: Term::default_dim(),
                name: "perform".to_string(),
                args: vec![Term::const_value(Value::String("Foo".to_string()))],
                source: ArrSource::Foreign("io".to_string()),
                lit: None,
                attrs: None,
            },
            r#"__handl_rt.perform("Foo")"#,
        ),
    ] {
        let js = compile_to_string(&input(term), &JsConfig::esm_deno())
            .unwrap_or_else(|e| panic!("perform spelling failed to compile: {e}"));
        assert!(js.contains(expect), "expected `{expect}` in:\n{js}");
    }
}

#[test]
fn denylisted_op_spellings_fail_to_compile() {
    // `handle`/`handle_dyn`/`rewrite`/`path`/`equiv` must never reach
    // `__handl_rt.<name>` or `__opRef` — both the applied and the bare
    // 0-arg reference forms are compile errors. (`perform`/`io.perform`
    // are *not* denied — see `perform_lowers_to_runtime_perform_call`.)
    for name in ["handle", "handle_dyn", "rewrite", "path", "equiv"] {
        for args in [
            vec![],
            vec![Term::const_value(Value::Number(Number::from(1)))],
        ] {
            let term = Term::op_native(name, args);
            let result = compile_to_string(&input(term), &JsConfig::esm_deno());
            assert!(result.is_err(), "op `{name}` compiled: {result:?}");
            assert!(
                format!("{:?}", result.unwrap_err()).contains(name),
                "op `{name}` error should name the op"
            );
        }
    }
}

// ---------------------------------------------------------------------------
// T-016 (P-002): compiler configurability — `JsConfig` knobs for the
// hardcoded lowering choices (`op_lowering`, `loop_max_iterations`,
// `security_scan`, `unsafe_inline_foreign`).
// ---------------------------------------------------------------------------

#[test]
fn op_lowering_mixed_is_the_default_inline_emit() {
    // `OpLowering::Mixed` (the default) keeps today's behavior: scalar
    // ops emit raw JS operators, not `__handl_rt` member calls.
    let term = Term::op_native(
        "add",
        vec![
            Term::const_value(Value::Number(Number::from(1))),
            Term::const_value(Value::Number(Number::from(2))),
        ],
    );
    let mut config = JsConfig::esm_deno();
    config.op_lowering = crate::OpLowering::Mixed;
    let js = compile_to_string(&input(term), &config).unwrap();
    assert!(js.contains("1 + 2"), "{js}");
    assert!(!js.contains("__handl_rt.add("), "{js}");
}

#[test]
fn op_lowering_prelude_routes_scalar_ops_through_rt() {
    // `OpLowering::Prelude` — the uniform prelude-call strategy — emits
    // `__handl_rt.<name>(…)` member calls for the whole scalar family
    // instead of raw JS operators.
    let mut config = JsConfig::esm_deno();
    config.op_lowering = crate::OpLowering::Prelude;

    let add = Term::op_native(
        "add",
        vec![
            Term::const_value(Value::Number(Number::from(1))),
            Term::const_value(Value::Number(Number::from(2))),
        ],
    );
    let js = compile_to_string(&input(add), &config).unwrap();
    assert!(js.contains("__handl_rt.add(1, 2)"), "{js}");
    assert!(!js.contains("1 + 2"), "{js}");

    let lt = Term::op_native(
        "lt",
        vec![
            Term::const_value(Value::Number(Number::from(1))),
            Term::const_value(Value::Number(Number::from(2))),
        ],
    );
    let js = compile_to_string(&input(lt), &config).unwrap();
    assert!(js.contains("__handl_rt.lt(1, 2)"), "{js}");
    assert!(!js.contains("1 < 2"), "{js}");

    let not = Term::op_native("not", vec![Term::const_value(Value::Bool(true))]);
    let js = compile_to_string(&input(not), &config).unwrap();
    assert!(js.contains("__handl_rt.not(true)"), "{js}");
    assert!(!js.contains("(!true)"), "{js}");
}

#[test]
fn op_lowering_prelude_applies_to_module_sources() {
    // The knob threads through `lower::with_config` on the file-level
    // module emit too — `.hnl` sources get the same strategy.
    let mut config = JsConfig::esm_deno();
    config.op_lowering = crate::OpLowering::Prelude;
    let js = compile_to_string(
        &module_input("add := (a, b) ~> a + b\nadd(20, 22)\n"),
        &config,
    )
    .unwrap();
    assert!(js.contains("__handl_rt.add("), "{js}");
}

#[test]
fn prelude_defines_scalar_op_members_for_prelude_strategy() {
    // The `Prelude` strategy's `__handl_rt.<name>` calls need members
    // for the whole scalar family — `add`/`sub`/`mul`/`div` already
    // existed; `lt`/`lte`/`gt`/`gte`/`mod`/`neg`/`not` were added for
    // the knob. They are emitted unconditionally (the prelude text is
    // config-independent).
    let js = compile_to_string(&input(Term::constant(Value::Null)), &JsConfig::esm_deno()).unwrap();
    for member in [
        "lt: (a, b) => (a < b)",
        "lte: (a, b) => (a <= b)",
        "gt: (a, b) => (a > b)",
        "gte: (a, b) => (a >= b)",
        "mod: (a, b) => (a % b)",
        "neg: (a) => (-a)",
        "not: (v) => (!v)",
    ] {
        assert!(js.contains(member), "missing `{member}`: {js}");
    }
}

#[test]
fn loop_max_iterations_is_configurable() {
    // `Term::Loop` feedback emits `__cnt < N`; `N` is
    // `JsConfig::loop_max_iterations` (default 100 000).
    let term = Term::loop_feedback(Term::const_value(Value::Number(Number::from(42))));
    let mut config = JsConfig::esm_deno();
    config.loop_max_iterations = 7;
    let js = compile_to_string(&input(term), &config).unwrap();
    assert!(js.contains("__cnt < 7"), "{js}");
    assert!(!js.contains("100000"), "{js}");

    // Default stays 100 000.
    let js = compile_to_string(
        &input(Term::loop_feedback(Term::const_value(Value::Number(
            Number::from(42),
        )))),
        &JsConfig::esm_deno(),
    )
    .unwrap();
    assert!(js.contains("__cnt < 100000"), "{js}");
}

#[test]
fn security_scan_disabled_permits_eval_substring() {
    // A string literal containing `eval(` trips the SecurityScanner's
    // substring check on the single-term path; `security_scan = false`
    // is the documented escape hatch.
    let term = Term::constant(Value::String("eval(".to_string()));
    assert!(compile_to_string(&input(term.clone()), &JsConfig::esm_deno()).is_err());

    let mut config = JsConfig::esm_deno();
    config.security_scan = false;
    let js = compile_to_string(&input(term), &config).unwrap();
    assert!(js.contains(r#""eval(""#), "{js}");
}

#[test]
fn unsafe_inline_foreign_bypasses_safety_check() {
    // A descriptor whose module is not a static identifier fails
    // `is_safe_for_inlining` → `__handl_rt.jsCall` by default;
    // `unsafe_inline_foreign` (experimental) inlines it anyway.
    let desc = json!({
        "module": "my lib",
        "name": "run",
        "arg_types": [],
        "return_type": "void"
    });
    let foreign_term = || Term::Op {
        dim: Box::new(Term::constant(Value::Number(Number::from(1)))),
        name: "run".to_string(),
        args: vec![],
        source: ArrSource::Foreign("my lib".to_string()),
        lit: Some(desc.clone()),
        attrs: None,
    };

    let js = compile_to_string(&input(foreign_term()), &JsConfig::esm_deno()).unwrap();
    // Unsafe descriptor → `dispatchForeign` with `inline = false`
    // (the `jsCall` fallback) and the descriptor JSON carried through.
    assert!(js.contains(r#"__handl_rt.dispatchForeign("run""#), "{js}");
    assert!(js.contains(r#""my lib", {"#), "{js}");
    assert!(js.contains(r#""module":"my lib""#), "{js}");
    assert!(js.contains(", false)"), "{js}");
    assert!(!js.contains(r#""my lib", null, true"#), "{js}");

    let mut config = JsConfig::esm_deno();
    config.unsafe_inline_foreign = true;
    let js = compile_to_string(&input(foreign_term()), &config).unwrap();
    // `unsafe_inline_foreign` selects `inline = true` (`null`
    // descriptors) — the prelude's `globalThis[module][name]` call.
    assert!(js.contains(r#""my lib", null, true)"#), "{js}");
    assert!(js.contains("globalThis[module][name]"), "{js}");

    // `no_inline_foreign` still wins when both flags are set.
    config.no_inline_foreign = true;
    let js = compile_to_string(&input(foreign_term()), &config).unwrap();
    assert!(js.contains(r#"__handl_rt.dispatchForeign("run""#), "{js}");
    assert!(js.contains(r#""my lib", {"#), "{js}");
    assert!(js.contains(", false)"), "{js}");
    assert!(!js.contains(r#""my lib", null, true"#), "{js}");
}

// ---------------------------------------------------------------------------
// T-018: `import.alias` — `{ orig as alias }`, `name as local`, `* as ns`.
// The parser leaves `import.alias` pair nodes as arena residue before
// `stmt.import`; only the normalized `payload.names`/`payload.alias`
// drive the binds below.
// ---------------------------------------------------------------------------

#[test]
fn module_js_package_braced_aliases_bind_shims() {
    // The T-018 repro: `from "js" import { on as js_on, call as js_call }`
    // — each alias binds the same `__handl_rt` shim the original name
    // would (`on` → event trampoline, `call` → host call).
    let js = compile_to_string(
        &module_input(
            "from \"js\" import { on as js_on, call as js_call }\nexport run := (t, h) ~> do {\n  l := js_on(t, \"ping\", h)\n  js_call(\"console\", \"log\", [l], [], \"js_object\", false)\n}\n",
        ),
        &JsConfig::esm_deno(),
    )
    .unwrap();
    assert!(js.contains("__handl_rt.on("), "{js}");
    assert!(js.contains("__handl_rt.call("), "{js}");
    // No ES import is emitted for the `js` contract package.
    assert!(!js.contains("from \"js\""), "{js}");
}

#[test]
fn module_js_package_single_name_and_namespace_aliases() {
    // `from "s" import name as local` — `payload.names` is the bare
    // string and `payload.alias` is the local rename; `await` still
    // binds the real await op under the alias.
    let js = compile_to_string(
        &module_input(
            "from \"js\" import on as js_on\nfrom \"js\" import await as js_await\nexport run := (p) ~> js_await(js_on(\"self\", \"x\", p))\n",
        ),
        &JsConfig::esm_deno(),
    )
    .unwrap();
    assert!(js.contains("__handl_rt.on("), "{js}");
    assert!(js.contains("(await "), "{js}");

    // `from "js" import * as jsm` — the namespace alias binds the whole
    // `__handl_rt` contract object; `jsm.on` member-calls resolve to
    // the same shim.
    let js = compile_to_string(
        &module_input(
            "from \"js\" import * as jsm\nexport run := (t, h) ~> jsm.on(t, \"ping\", h)\n",
        ),
        &JsConfig::esm_deno(),
    )
    .unwrap();
    // `jsm` binds the `__handl_rt` contract object; the member access
    // emits the bracket-path call into the same `on` shim.
    assert!(js.contains("__handl_rt[\"on\"]"), "{js}");
}

#[test]
fn module_dep_and_external_aliases() {
    // A resolved `.hnl` dep: `{ f as g }` binds `g` to the dep's `f`
    // member path — alias changes only the local name.
    let mut config = JsConfig::esm_deno();
    config.modules = vec![crate::JsModule {
        path: "/pkgs/dep/dep.hnl".to_string(),
        source: "export f := (x) ~> x + 1\n".to_string(),
        imports: Vec::new(),
    }];
    config.entry_imports = vec![("dep".to_string(), "/pkgs/dep/dep.hnl".to_string())];
    let js = compile_to_string(
        &module_input(
            "from \"dep\" import { f as g }\nfrom \"node:path\" import { join as j }\nexport run := (x) ~> j(g(x))\n",
        ),
        &config,
    )
    .unwrap();
    assert!(
        js.contains(r#"__handl_rt.__apply(__handl_rt.dep.f"#),
        "{js}"
    );
    // Unresolved path specifier → real ES import with `orig as alias`.
    assert!(
        js.contains(r#"import { join as j } from "node:path";"#),
        "{js}"
    );
}

// ---------------------------------------------------------------------------
// T-008: effect-parity remainder — `dispatch_all_ops`, dim-label
// threading, and the `dispatchForeign` routing.
// ---------------------------------------------------------------------------

#[test]
fn dispatch_all_ops_routes_scalar_ops_through_dispatch() {
    // Default (`dispatch_all_ops = false`): `add` keeps its dedicated
    // emit — documented divergence from eval's `dispatch_op`, which
    // consults handlers for every `Native` op.
    let term = || {
        Term::op_native(
            "add",
            vec![
                Term::const_value(Value::Number(Number::from(1))),
                Term::const_value(Value::Number(Number::from(2))),
            ],
        )
    };
    let js = compile_to_string(&input(term()), &JsConfig::esm_deno()).unwrap();
    assert!(!js.contains(r#"dispatch("add""#), "{js}");

    // The flag routes the pure arg-op family through `dispatch` so a
    // covering `handle` frame can intercept it.
    let mut config = JsConfig::esm_deno();
    config.dispatch_all_ops = true;
    let js = compile_to_string(&input(term()), &config).unwrap();
    assert!(js.contains(r#"__handl_rt.dispatch("add""#), "{js}");
}

#[test]
fn op_dim_label_reaches_handler_dispatch() {
    // A literal-string dim (`Const "S"`/`Arr "S"` — the `~[S]~>` grade
    // surface) is forwarded to `dispatch`/`dispatchForeign` as the
    // `grades`-coverage label; the incoming value rides along as the
    // second handler channel.
    let named = Term::op_native_dim(
        "mystery",
        Term::constant(Value::String("S".to_string())),
        vec![Term::const_value(Value::Number(Number::from(1)))],
    );
    let js = compile_to_string(&input(named), &JsConfig::esm_deno()).unwrap();
    assert!(
        js.contains(r#"__handl_rt.dispatch("mystery", [1], "S", __in)"#),
        "{js}"
    );

    let foreign = Term::Op {
        dim: Box::new(Term::constant(Value::String("net".to_string()))),
        name: "fetch".to_string(),
        args: vec![],
        source: ArrSource::Foreign("http".to_string()),
        lit: None,
        attrs: None,
    };
    let js = compile_to_string(&input(foreign), &JsConfig::esm_deno()).unwrap();
    assert!(
        js.contains(r#"dispatchForeign("fetch", [], "net", __in, "http", null, false)"#),
        "{js}"
    );
}

// ---------------------------------------------------------------------------
// T-019: `>>>` (`flow`/`Seq`) lazy-reference application — `Seq.snd` and
// quoted-arrow bodies must *apply* a bare `Op{name, []}` to the stage
// input (`__appOrValue`), while argument positions keep the referent.
// ---------------------------------------------------------------------------

#[test]
fn seq_lazy_op_ref_applies_via_app_or_value() {
    // `Seq[id, Op{s1, []}]` — the snd is a body-arrow position: the
    // named binding applies to the stage input. With no module env the
    // name is unbound, so the referent resolves through `__opRef` and
    // `__appOrValue` applies it (or reads it back when non-callable).
    let term = Term::seq(Term::op_native("id", vec![]), Term::op_native("s1", vec![]));
    let js = compile_to_string(&input(term), &JsConfig::esm_deno()).unwrap();
    assert!(js.contains("__appOrValue"), "{js}");
    assert!(js.contains(r#"__opRef("s1")"#), "{js}");
}

#[test]
fn seq_lazy_op_ref_bound_arrow_applies_binding() {
    // Module scope: `s1` binds an arrow `const`, so the scheme-position
    // reference applies the binding itself — `__appOrValue(s1, …)`.
    let js = compile_to_string(
        &module_input("s1 := (x) ~> x + 1\npipe := (x) ~> x >>> s1\npipe(5)\n"),
        &JsConfig::esm_deno(),
    )
    .unwrap();
    assert!(js.contains("__appOrValue(s1"), "{js}");
}

#[test]
fn quoted_lazy_op_ref_arrow_body_applies_binding() {
    // `comp := s1 >>> s2` lowers to `Op{flow, [Op{s1, []}, Arr{Op{s2, []}}]}`
    // — the quoted rhs is an arrow *body*, so its `Op{s2, []}` must
    // apply `s2` to the incoming (`__in => s2(__in)`), not return the
    // `s2` binding. Pin the `__appOrValue` emit inside the arrow.
    let js = compile_to_string(
        &module_input("s1 := (x) ~> x + 1\ns2 := (x) ~> x * 10\ncomp := s1 >>> s2\ncomp(5)\n"),
        &JsConfig::esm_deno(),
    )
    .unwrap();
    assert!(js.contains("__appOrValue(s2, __in)"), "{js}");
}

#[test]
fn op_arg_positions_keep_the_referent_reading() {
    // `apply2(s1, 5)` — `s1` in argument position is a *value* read:
    // the arrow itself is passed, never applied to the ambient input.
    let js = compile_to_string(
        &module_input("s1 := (x) ~> x + 1\napply2 := (g, x) ~> g(x)\napply2(s1, 5)\n"),
        &JsConfig::esm_deno(),
    )
    .unwrap();
    assert!(!js.contains("__appOrValue(s1"), "{js}");
}
