//! End-to-end tests: compile Handl sources to JavaScript through the
//! public API and *execute* the emitted JS under `node`, asserting on
//! real stdout/exit status instead of emitted strings.
//!
//! Every fixture compiles a `.hnl` module source (the same path
//! `examples/emit_module.rs` exercises) to ESM, writes it plus a tiny
//! driver `.mjs` into a temp dir, and runs `node driver.mjs`. The
//! driver imports the module's default export (the trailing
//! expression's value) and prints it, so each fixture asserts on the
//! value the program actually produced.
//!
//! The whole suite skips gracefully when `node` is not on `PATH`
//! (`node --version` is probed once); `ci.sh`'s `test` stage fails
//! fast when node is absent so CI cannot silently skip these tests.

use std::process::Command;
use std::sync::OnceLock;

use handl_language::{Ast, AstNode, Language};
use handl_plugin_compile::CompilerInput;
use handl_plugin_compile_js::{JsConfig, JsModule, Node, compile_to_string};
use hnl_engine::{ArrSource, Term};
use hnl_engine_typecheck::TypecheckReport;
use serde_json::Value;

/// `node --version` output when the binary is on `PATH`, else `None`.
fn node_version() -> Option<&'static str> {
    static NODE: OnceLock<Option<String>> = OnceLock::new();
    NODE.get_or_init(|| {
        let out = Command::new("node").arg("--version").output().ok()?;
        out.status
            .success()
            .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
    })
    .as_deref()
}

/// The canonical language for `.hnl` module sources (dev-dep cycle is
/// allowed, same as `src/tests.rs`).
fn canonical_lang() -> &'static Language {
    static LANG: OnceLock<Language> = OnceLock::new();
    LANG.get_or_init(|| {
        handl_kit_data_prelude::language_from_yaml().expect("canonical language assembles")
    })
}

/// A `CompilerInput` for a whole `.hnl` module source — identical to
/// `src/tests.rs`'s `module_input`.
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

/// The result of one `node` invocation.
struct Run {
    /// Captured stdout.
    stdout: String,
    /// Captured stderr.
    stderr: String,
    /// Process exit code (`None` when killed by a signal).
    code: Option<i32>,
}

impl Run {
    fn succeeded(&self) -> bool {
        self.code == Some(0)
    }
}

/// Compile `source` to ESM JS, write it as `prog.mjs` plus `driver.mjs`
/// into a fresh temp dir, run `node driver.mjs`, and clean up.
fn run_program_with(name: &str, source: &str, config: JsConfig, driver: &str) -> Run {
    let js = compile_to_string(&module_input(source), &config).expect("compile fixture");
    run_node(
        name,
        &[("prog.mjs", js.as_str()), ("driver.mjs", driver)],
        "driver.mjs",
    )
}

/// ESM output on the Node target.
fn esm_node() -> JsConfig {
    let mut config = JsConfig::esm_deno();
    config.target = Box::new(Node);
    config
}

/// Compile `source` with the default ESM config and run `driver`.
fn run_program(name: &str, source: &str, driver: &str) -> Run {
    run_program_with(name, source, esm_node(), driver)
}

/// Write `files` (`name → contents`) into a fresh temp dir, run
/// `node <entry>` there, and remove the dir afterwards.
fn run_node(name: &str, files: &[(&str, &str)], entry: &str) -> Run {
    let dir = std::env::temp_dir().join(format!("handl-js-e2e-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create temp dir");
    for (file, contents) in files {
        std::fs::write(dir.join(file), contents).expect("write fixture file");
    }
    let out = Command::new("node")
        .arg(entry)
        .current_dir(&dir)
        .output()
        .expect("spawn node");
    let run = Run {
        stdout: String::from_utf8_lossy(&out.stdout).to_string(),
        stderr: String::from_utf8_lossy(&out.stderr).to_string(),
        code: out.status.code(),
    };
    let _ = std::fs::remove_dir_all(&dir);
    run
}

/// The default driver: print the module's default export as JSON.
fn print_default() -> &'static str {
    "import v from \"./prog.mjs\";\nconsole.log(JSON.stringify(v));\n"
}

/// Skip guard: returns early (with a note) when `node` is absent.
macro_rules! require_node {
    () => {
        if node_version().is_none() {
            eprintln!("e2e: `node` not on PATH — skipping (ci.sh fails on this)");
            return;
        }
    };
}

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

#[test]
fn e2e_arithmetic_literals_and_operators() {
    require_node!();
    let run = run_program(
        "arith",
        r#"
add := (a, b) ~> a + b
mul := (a, b) ~> a * b
sub := (a, b) ~> a - b
div := (a, b) ~> a / b
gt := (a, b) ~> a > b
total := add(mul(6, 7), sub(20, 8))
half := div(9, 2)
bigger := gt(add(5, 4), 9)
{ total: total, half: half, bigger: bigger }
"#,
        print_default(),
    );
    assert!(run.succeeded(), "node failed:\n{}", run.stderr);
    // 6*7 + (20-8) = 54; 9/2 = 4.5; 5+4 > 9 is false.
    assert_eq!(
        run.stdout.trim(),
        r#"{"total":54,"half":4.5,"bigger":false}"#
    );
}

#[test]
fn e2e_do_block_sequences_bindings() {
    require_node!();
    let run = run_program(
        "do",
        r#"
f := (x) ~> do {
  a := x + 1
  b := a * 2
  b - 3
}
f(10)
"#,
        print_default(),
    );
    assert!(run.succeeded(), "node failed:\n{}", run.stderr);
    // ((10 + 1) * 2) - 3 = 19 — the `do` block's value is its last expr.
    assert_eq!(run.stdout.trim(), "19");
}

#[test]
fn e2e_if_branching_expression() {
    require_node!();
    let run = run_program(
        "if",
        r#"
sign := (n) ~> if n < 0 { "neg" } else { if n == 0 { "zero" } else { "pos" } }
{ neg: sign(-3), zero: sign(0), pos: sign(7) }
"#,
        print_default(),
    );
    assert!(run.succeeded(), "node failed:\n{}", run.stderr);
    assert_eq!(
        run.stdout.trim(),
        r#"{"neg":"neg","zero":"zero","pos":"pos"}"#
    );
}

#[test]
fn e2e_while_loop_bounded_iteration() {
    require_node!();
    let run = run_program(
        "while",
        r#"
countdown := (n) ~> while { n: n, acc: 0 } {
  s ~> if s.n == 0 {
    { out: s.acc }
  } else {
    { state: { n: s.n - 1, acc: s.acc + s.n } }
  }
}
countdown(5)
"#,
        print_default(),
    );
    assert!(run.succeeded(), "node failed:\n{}", run.stderr);
    // 5 + 4 + 3 + 2 + 1 = 15.
    assert_eq!(run.stdout.trim(), "15");
}

#[test]
fn e2e_match_literal_sequence_and_wildcard_arms() {
    require_node!();
    let run = run_program(
        "match",
        r#"
classify := (x) ~> match x {
  0: "zero",
  [a, b]: "pair",
  _: "other"
}
sum2 := (p) ~> match p {
  [a, b]: a + b,
  _: 0
}
{ z: classify(0), p: classify([1, 2]), o: classify("x"), s: sum2([3, 4]), d: sum2(9) }
"#,
        print_default(),
    );
    assert!(run.succeeded(), "node failed:\n{}", run.stderr);
    // `0` hits the literal arm, `[1,2]` the sequence arm, `"x"` the
    // wildcard; `sum2` binds the two sequence elements (3+4=7) and its
    // wildcard yields 0 for a non-pair.
    assert_eq!(
        run.stdout.trim(),
        r#"{"z":"zero","p":"pair","o":"other","s":7,"d":0}"#
    );
}

#[test]
fn e2e_module_dep_inlined_via_jsmodule() {
    require_node!();
    // The driver resolved `from "dep"` to a `.hnl` file: `JsConfig::
    // modules` carries it and the emit inlines it once as a
    // `__handl_rt["dep"]` namespace — no ES import is emitted.
    let mut config = esm_node();
    config.modules = vec![JsModule {
        path: "/pkgs/dep/dep.hnl".to_string(),
        source: "export double := (x) ~> x * 2\nexport quad := (x) ~> double(double(x))\n"
            .to_string(),
        imports: Vec::new(),
    }];
    config.entry_imports = vec![("dep".to_string(), "/pkgs/dep/dep.hnl".to_string())];
    let run = run_program_with(
        "dep",
        r#"
from "dep" import { quad }
quad(5)
"#,
        config,
        print_default(),
    );
    assert!(run.succeeded(), "node failed:\n{}", run.stderr);
    // double(double(5)) = 20.
    assert_eq!(run.stdout.trim(), "20");
}

#[test]
fn e2e_js_contract_interop_events() {
    require_node!();
    // `js.construct`/`js.on`/`js.off`/`js.invoke`/`js.call`/`js.await`
    // against real Node globals (`EventTarget`, `Event`, `console`).
    let run = run_program(
        "js-interop",
        r#"
from "js" import { construct as js_construct, on as js_on, off as js_off, invoke as js_invoke, call as js_call, await as js_await }
et := js_construct("", "EventTarget", [], [], "js_object", false)
ev := js_construct("", "Event", ["ping"], [], "js_object", false)
h := js_on(et, "ping", (e) ~> js_await(js_call("console", "log", ["got-ping"], [], "js_object", false)))
js_invoke(et, "dispatchEvent", [ev], [], "js_object", false)
js_off(et, "ping", h)
"done"
"#,
        print_default(),
    );
    assert!(run.succeeded(), "node failed:\n{}", run.stderr);
    // The dispatched event ran the compiled arrow (it logged via
    // `js.call` → `console.log`), `off` unregistered cleanly, and the
    // module's trailing value is `"done"`.
    assert!(run.stdout.contains("got-ping"), "{}", run.stdout);
    assert_eq!(run.stdout.trim().lines().last().unwrap(), "\"done\"");
}

#[test]
fn e2e_term_level_arithmetic_through_public_api() {
    require_node!();
    // The single-term path (synthetic `CompilerInput`, no real
    // `Language`): `export default async __in => …` — the driver
    // applies the arrow to an input and prints the awaited result.
    let term = Term::op_native(
        "add",
        vec![
            Term::const_value(Value::Number(serde_json::Number::from(20))),
            Term::const_value(Value::Number(serde_json::Number::from(22))),
        ],
    );
    let mut ast = Ast::new();
    let root = ast.push(AstNode::new("module"));
    let input = CompilerInput::new(
        "test".to_string(),
        ast,
        root,
        Language::new(),
        term,
        TypecheckReport::default(),
    );
    let js = compile_to_string(&input, &JsConfig::esm_deno()).expect("term compile");
    assert!(js.contains("export default"), "{js}");
    let run = run_node(
        "term",
        &[
            ("prog.mjs", js.as_str()),
            (
                "driver.mjs",
                "import f from \"./prog.mjs\";\nconsole.log(await f(null));\n",
            ),
        ],
        "driver.mjs",
    );
    assert!(run.succeeded(), "node failed:\n{}", run.stderr);
    assert_eq!(run.stdout.trim(), "42");
}

// ---------------------------------------------------------------------------
// Security surface (compile-time; no node needed)
// ---------------------------------------------------------------------------

#[test]
fn e2e_dangerous_foreign_op_rejected() {
    // A foreign descriptor naming `eval` fails validation — the emit
    // never reaches node (mirrors `dangerous_foreign_op_rejected_at_
    // compile_time` in src/tests.rs, kept here as the e2e security
    // fixture).
    let desc = serde_json::json!({
        "module": "x",
        "name": "eval",
        "arg_types": [],
        "return_type": "void"
    });
    let term = Term::Op {
        dim: Term::default_dim(),
        name: "eval".to_string(),
        args: vec![],
        source: ArrSource::Foreign("x".to_string()),
        lit: Some(desc),
        attrs: None,
    };
    let mut ast = Ast::new();
    let root = ast.push(AstNode::new("module"));
    let input = CompilerInput::new(
        "test".to_string(),
        ast,
        root,
        Language::new(),
        term,
        TypecheckReport::default(),
    );
    assert!(compile_to_string(&input, &JsConfig::esm_deno()).is_err());
}
