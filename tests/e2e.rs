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
use handl_plugin_compile_js::{Iife, Inline, JsConfig, JsModule, Node, compile_to_string};
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
// Runtime-op fixtures (T-002): `record_merge`, `flow`, `ctor_name`/
// `ctor_fields` (via ctor-object match patterns), `assert`/
// `runtime_assert`, and the `{x}` object-pattern shorthand.
// ---------------------------------------------------------------------------

#[test]
fn e2e_record_merge_object_spread() {
    require_node!();
    let run = run_program(
        "record-merge",
        r#"
base := { a: 1, b: 2 }
ext := { b: 9, c: 3 }
merged := { x: 0, ...base, ...ext, y: 5 }
merged
"#,
        print_default(),
    );
    assert!(run.succeeded(), "node failed:\n{}", run.stderr);
    // Later keys win (`b: 9` from `ext`), non-overlapping keys union,
    // and literal runs merge in source order — the
    // `record_merge(record_merge(…), …)` fold.
    assert_eq!(run.stdout.trim(), r#"{"x":0,"a":1,"b":9,"c":3,"y":5}"#);
}

#[test]
fn e2e_flow_composes_and_applies_arrow_values() {
    require_node!();
    // `mk(1) >>> mk(2)` — call results are held arrow *values*, so the
    // `eval.flow` dispatch composes them (`second(first(x))`);
    // `21 >>> f` applies the right operand to the left value.
    let run = run_program(
        "flow",
        r#"
mk := (x) ~> (y) ~> x + y
comp := mk(1) >>> mk(2)
dbl := (x) ~> x * 2
run3 := (f) ~> 21 >>> f
{ comp: comp(5), applied: run3(dbl) }
"#,
        print_default(),
    );
    assert!(run.succeeded(), "node failed:\n{}", run.stderr);
    // comp(5) = mk2(mk1(5)) = (5 + 1) + 2 = 8; run3(dbl) = dbl(21) = 42.
    assert_eq!(run.stdout.trim(), r#"{"comp":8,"applied":42}"#);
}

#[test]
fn e2e_ctor_object_pattern_uses_ctor_name_and_fields() {
    require_node!();
    // `Pair(a, b)` is a tag/call pattern: the discriminant tries
    // `__tag__` first, then `ctor_name(v)` against the ctor sym's name;
    // ctor binders fall back to `ctor_fields` (sorted non-`__ctor__`/
    // `type` values). The driver supplies the `__ctor__` wire record —
    // ctor objects are not expressible as `.hnl` literals.
    let run = run_program(
        "ctor-pat",
        r#"
f := (v) ~> match v {
  Pair(a, b): a + b,
  _: 0
}
f
"#,
        "import f from \"./prog.mjs\";\n\
         const ctor = {__ctor__: {__sym__: {name: \"Pair\", handle: 1}}, a: 4, b: 5};\n\
         console.log(JSON.stringify([await f(ctor), await f({a: 1, b: 2}), await f(7)]));\n",
    );
    assert!(run.succeeded(), "node failed:\n{}", run.stderr);
    // ctor object: ctor_name → "Pair", ctor_fields → [4, 5] → 4 + 5;
    // plain object and non-object scrutinees fall through to `_` → 0.
    assert_eq!(run.stdout.trim(), "[9,0,0]");
}

#[test]
fn e2e_assert_and_runtime_assert_fail_like_eval() {
    require_node!();
    // `Op{assert, []}`/`Op{runtime_assert, []}` carry no payload (the
    // expr/predicate children are AST-level), so the JS backend throws
    // the eval-faithful "unsupported" diagnostic — a real `Error`, not
    // a `TypeError: … is not a function`.
    for (name, op) in [("assert", "assert"), ("runtime-assert", "runtime_assert")] {
        let term = Term::op_native(op, vec![]);
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
        let js = compile_to_string(&input, &esm_node()).expect("compile fixture");
        let run = run_node(
            name,
            &[
                ("prog.mjs", js.as_str()),
                (
                    "driver.mjs",
                    "import f from \"./prog.mjs\";\ntry { await f(null); console.log(\"no-throw\"); } catch (e) { console.log(\"threw:\" + e.message); }\n",
                ),
            ],
            "driver.mjs",
        );
        assert!(run.succeeded(), "{name}: node failed:\n{}", run.stderr);
        assert!(
            run.stdout.contains(&format!("threw:{op}:")),
            "{name}: {}",
            run.stdout
        );
        assert!(
            !run.stdout.contains("is not a function"),
            "{name}: {}",
            run.stdout
        );
    }
}

#[test]
fn e2e_object_pattern_shorthand_binds_nothing() {
    require_node!();
    // `{x}` parses as an `object` node with a bare `var` child — eval
    // (`try_object_pattern`) ignores non-entry children, so the pattern
    // is an *empty* object pattern and `x` stays unbound. The compiled
    // `x` is a bare `Op{x, []}` reference: `__opRef` throws
    // "unsupported op: x" exactly like `term_interpreter`, instead of
    // the old silent `undefined`. The `{x: y}` entry form binds `y`
    // from the scrutinee field and is unaffected.
    let run = run_program(
        "objpat",
        r#"
getx := (r) ~> match r { {x}: x, _: -1 }
gety := (r) ~> match r { {x: y}: y, _: -1 }
{ shorthand: getx, entry: gety }
"#,
        "import v from \"./prog.mjs\";\n\
         const out = {};\n\
         for (const [k, f] of Object.entries(v)) {\n\
           for (const inp of [{x: 42}, {y: 1}]) {\n\
             try { out[k + \":\" + JSON.stringify(inp)] = await f(inp); }\n\
             catch (e) { out[k + \":\" + JSON.stringify(inp)] = \"ERR:\" + e.message; }\n\
           }\n\
         }\n\
         console.log(JSON.stringify(out));\n",
    );
    assert!(run.succeeded(), "node failed:\n{}", run.stderr);
    assert_eq!(
        run.stdout.trim(),
        r#"{"shorthand:{\"x\":42}":"ERR:unsupported op: x","shorthand:{\"y\":1}":"ERR:unsupported op: x","entry:{\"x\":42}":42,"entry:{\"y\":1}":-1}"#
    );
}

// ---------------------------------------------------------------------------
// T-003: contract/signature decls and `data` families.
// ---------------------------------------------------------------------------

#[test]
fn e2e_signature_export_emits_valid_esm() {
    require_node!();
    // `export f: T ~> U` is a *signature* — no runtime implementation.
    // module_prep erases it to `null`; the emit must still produce valid
    // ESM (`export let f = null`), not `export { f }` with no binding
    // (a parse-time SyntaxError under node).
    let run = run_program(
        "sig-export",
        r#"
export f: Nat ~> Nat
export g : (x : Nat) ~> Nat = n ~> n
g(21)
"#,
        print_default(),
    );
    assert!(run.succeeded(), "node failed:\n{}", run.stderr);
    assert_eq!(run.stdout.trim(), "21");
}

#[test]
fn e2e_signature_decl_then_impl_reassigns() {
    require_node!();
    // `f : T` binds `f` to `null` (type-only erasure); the later
    // `f := impl` reassigns the same `let` — no duplicate `const`.
    let run = run_program(
        "sig-impl",
        r#"
f : Nat ~> Number
f := (n) ~> n + 1
f(1)
"#,
        print_default(),
    );
    assert!(run.succeeded(), "node failed:\n{}", run.stderr);
    assert_eq!(run.stdout.trim(), "2");
}

#[test]
fn e2e_data_decl_constructors_and_match() {
    require_node!();
    // `Nat := { Zero : Nat, Succ : (pred : Nat) ~> Nat }` desugars to
    // former/ctor/recursor assigns; `Succ(Succ(Zero))` builds a tagged
    // ctor object and `match` dispatches on `__ctor__` — `Zero:` hits
    // the ctor-value pattern (globals-literal `eq`), `Succ(p)` the
    // `ctor_name`/`ctor_fields` path.
    let run = run_program(
        "data-match",
        r#"
Nat := { Zero : Nat, Succ : (pred : Nat) ~> Nat }
to_num : Nat ~> Number = n ~> match n { Zero: 0, Succ(p): 1 + to_num(p) }
two : Nat = Succ(Succ(Zero))
to_num(two)
"#,
        print_default(),
    );
    assert!(run.succeeded(), "node failed:\n{}", run.stderr);
    // to_num(Succ(Succ(Zero))) = 2.
    assert_eq!(run.stdout.trim(), "2");
}

#[test]
fn e2e_data_decl_parameterized_family() {
    require_node!();
    // `Opt<a>` — a parameterized family: `Some(41)` builds
    // `{__ctor__, x, type}`; `Some(x)` binds `x` through
    // `ctor_fields`; `None` matches the nullary ctor-value pattern.
    let run = run_program(
        "data-param",
        r#"
Opt<a : Type> := { None : Opt, Some : (x : a) ~> Opt }
get_or : Opt ~> Number ~> Number = o ~> d ~> match o { None: d, Some(x): x }
{ some: get_or(Some(41))(0), none: get_or(None)(7) }
"#,
        print_default(),
    );
    assert!(run.succeeded(), "node failed:\n{}", run.stderr);
    assert_eq!(run.stdout.trim(), r#"{"some":41,"none":7}"#);
}

#[test]
fn e2e_export_data_record_form_with_plain_export() {
    require_node!();
    // `export data F := { … }` (the record spelling) alongside a plain
    // `export x := …` — both surface as real ESM named exports while
    // the ctors keep matching in-module: `Zero:` hits the ctor-value
    // pattern (the decl's own ctor literal in `lang.globals`),
    // `Succ(p)` the `ctor_name`/`ctor_fields` dispatch.
    let run = run_program(
        "export-data-record",
        r#"
export data F := { Zero : F, Succ : (pred : F) ~> F }
export x := 40
size := n ~> match n { Zero: 0, Succ(p): 1 + size(p), _: 9 }
{ f: size(Succ(Zero)), x: x }
"#,
        "import def, { F, Zero, Succ, x } from \"./prog.mjs\";\n\
         const two = await Succ(await Succ(Zero));\n\
         console.log(JSON.stringify({ def: def, x: x,\n\
           ctor: two.__ctor__.__sym__.name,\n\
           hasF: typeof F === \"object\" && F.__former__ != null }));\n",
    );
    assert!(run.succeeded(), "node failed:\n{}", run.stderr);
    // size(Succ(Zero)) = 1; x = 40; `two` is a Succ ctor object; F is
    // the exported family former.
    assert_eq!(
        run.stdout.trim(),
        r#"{"def":{"f":1,"x":40},"x":40,"ctor":"Succ","hasF":true}"#
    );
}

#[test]
fn e2e_export_data_family_and_ctors_importable() {
    require_node!();
    // `export data Nat :: …` inside an inlined dep exports the family
    // and each constructor (`module_prep`'s export-record projection —
    // `F.rec` is dotted and stays off the record). The consumer imports
    // them and `match`es: `Zero:` resolves through the ctor literal the
    // dep propagated into the importer's `lang.globals` (eval's
    // scope-value parity), `Succ(p)` through `ctor_name` dispatch.
    let mut config = esm_node();
    config.modules = vec![JsModule {
        path: "/pkgs/nat/nat.hnl".to_string(),
        source: "export data Nat :: Zero | Succ(pred)\n\
                 export to_num := n ~> match n { Zero: 0, Succ(p): 1 + to_num(p) }\n\
                 export one := Succ(Zero)\n"
            .to_string(),
        imports: Vec::new(),
    }];
    config.entry_imports = vec![("nat".to_string(), "/pkgs/nat/nat.hnl".to_string())];
    let run = run_program_with(
        "export-data-dep",
        r#"
from "nat" import { Nat, Zero, Succ, to_num, one }
kind := n ~> match n { Zero: "zero", Succ(p): "succ", _: "other" }
{
  ka: kind(Zero),
  kb: kind(one),
  kc: to_num(one),
  kd: to_num(Succ(Zero)),
  kn: Nat.__former__ == null
}
"#,
        config,
        print_default(),
    );
    assert!(run.succeeded(), "node failed:\n{}", run.stderr);
    // kind(Zero) = "zero" (ctor-value pattern on the imported literal),
    // kind(one) = "succ" (`one` is the dep's Succ(Zero) ctor object);
    // to_num counts through both the dep's recursor-free match and a
    // locally built `Succ(Zero)`; `Nat.__former__` is a real object
    // (not null) — the family former exported.
    assert_eq!(
        run.stdout.trim(),
        r#"{"ka":"zero","kb":"succ","kc":1,"kd":1,"kn":false}"#
    );
}

// ---------------------------------------------------------------------------
// T-004: worker-entry bundles — a `.hnl` worker module compiles to ONE
// self-contained ESM file (prelude + inlined deps) that a real
// `node:worker_threads` Worker (or a browser-shaped `self` global)
// loads and answers `"message"` events on.
// ---------------------------------------------------------------------------

/// Compile `source` as a worker-entry bundle (ESM + `worker_entry`).
fn compile_worker(source: &str) -> String {
    let mut config = esm_node();
    config.worker_entry = true;
    compile_to_string(&module_input(source), &config).expect("compile worker entry")
}

#[test]
fn e2e_worker_entry_node_worker_roundtrip_and_transfer() {
    require_node!();
    // The worker module wires its own message loop — the `js/worker`
    // convention (`lib-js/src/worker/index.hnl`): `js.on` over a concrete
    // EventTarget. Node's worker global scope has no `self`/`postMessage`
    // (verified: all undefined), so the entry imports `parentPort` — a
    // `node:` builtin specifier the worker-entry check keeps as a real
    // ES import. The handler explicitly `postMessage`s a record
    // carrying an `ArrayBuffer` plus a transfer list, then returns a
    // value the `js.on` reply convention posts back automatically; the
    // reply's `remaining` field is the buffer's `byteLength` *after*
    // the transfer — 0 proves the detach, 16 would mean it cloned.
    let worker_js = compile_worker(
        r#"
from "node:worker_threads" import { parentPort }
from "js" import {
  on as js_on,
  call as js_call,
  construct as js_construct,
  invoke as js_invoke,
}

ab := js_construct("", "ArrayBuffer", [16], [], "js_object", false)

js_on(parentPort, "message", (msg) ~> do {
  js_invoke(parentPort, "postMessage", [{ echoed: msg.n, buf: ab }, [ab]], [], "void", false)
  remaining := js_call("Reflect", "get", [ab, "byteLength"], [], "js_number", false)
  { echoed: msg.n, remaining: remaining }
})
"#,
    );
    let driver = r#"
import { Worker } from "node:worker_threads";
const worker = new Worker(new URL("./worker.mjs", import.meta.url));
const msgs = await new Promise((resolve, reject) => {
  const acc = [];
  worker.on("message", (m) => {
    acc.push(m);
    if (acc.length === 2) resolve(acc);
  });
  worker.on("error", reject);
  worker.postMessage({ n: 21 });
  setTimeout(() => reject(new Error("worker reply timeout")), 10000);
});
worker.terminate();
const [explicit, reply] = msgs;
if (!(explicit.buf instanceof ArrayBuffer) || explicit.buf.byteLength !== 16) {
  throw new Error("transfer failed — buf did not arrive as a 16-byte ArrayBuffer: " + JSON.stringify(explicit));
}
if (explicit.echoed !== 21 || reply.echoed !== 21 || reply.remaining !== 0) {
  throw new Error("bad round-trip: " + JSON.stringify(msgs));
}
console.log("worker-ok");
"#;
    let run = run_node(
        "worker-node",
        &[("worker.mjs", worker_js.as_str()), ("driver.mjs", driver)],
        "driver.mjs",
    );
    assert!(run.succeeded(), "node failed:\n{}", run.stderr);
    assert_eq!(run.stdout.trim(), "worker-ok");
}

#[test]
fn e2e_worker_entry_browser_shaped_self() {
    require_node!();
    // The browser-worker spelling of the same convention: the entry
    // registers `js.on("self", "message", arrow)` and the `on` trampoline
    // replies to the worker global itself (`isPeer`). Node has no real
    // worker `self`, so the driver installs a minimal
    // `DedicatedWorkerGlobalScope`-shaped fake *before* importing the
    // bundle — `globalThis["self"]` is what the shim resolves.
    let worker_js = compile_worker(
        r#"
from "js" import { on as js_on }

js_on("self", "message", (msg) ~> { echo: msg.n, pong: "yes" })
"#,
    );
    let driver = r#"
const listeners = new Map();
const sent = [];
globalThis.self = {
  addEventListener: (name, fn) => listeners.set(name, fn),
  removeEventListener: (name) => listeners.delete(name),
  postMessage: (m) => sent.push(m),
  importScripts: () => {},
  close: () => {},
};
await import("./worker.mjs");
const listener = listeners.get("message");
if (typeof listener !== "function") throw new Error("worker did not register a message listener");
listener({ data: { n: 7 } });
await new Promise((r) => setTimeout(r, 100));
if (sent.length !== 1) throw new Error("expected 1 reply, got " + sent.length);
const m = sent[0];
if (m.echo !== 7 || m.pong !== "yes") throw new Error("bad reply: " + JSON.stringify(m));
console.log("self-ok");
"#;
    let run = run_node(
        "worker-self",
        &[("worker.mjs", worker_js.as_str()), ("driver.mjs", driver)],
        "driver.mjs",
    );
    assert!(run.succeeded(), "node failed:\n{}", run.stderr);
    assert_eq!(run.stdout.trim(), "self-ok");
}

// ---------------------------------------------------------------------------
// T-005: non-ESM module formats — `commonjs`/`iife`/`inline` module emit,
// exercised under real `node`.
// ---------------------------------------------------------------------------

/// `commonjs` output on the Node target.
fn cjs_node() -> JsConfig {
    JsConfig::cjs_node()
}

/// A script format (`iife`/`inline`) on the Node target — the format,
/// not the target, decides the module shape; Node is where drivers run.
fn script_format<F: handl_plugin_compile_js::JsOutputFormat + 'static>(format: F) -> JsConfig {
    let mut config = JsConfig::esm_deno();
    config.target = Box::new(Node);
    config.format = Box::new(format);
    config
}

#[test]
fn e2e_commonjs_module_namespace_via_require() {
    require_node!();
    // A `.hnl` module with a resolved dep compiles to CJS: dep inlining
    // is unchanged, `export`s collect into the namespace object, and
    // `module.exports` is the async-IIFE promise — `await require()`
    // yields `{ double, quad, default }`.
    let mut config = cjs_node();
    config.modules = vec![JsModule {
        path: "/pkgs/dep/dep.hnl".to_string(),
        source: "export double := (x) ~> x * 2\n".to_string(),
        imports: Vec::new(),
    }];
    config.entry_imports = vec![("dep".to_string(), "/pkgs/dep/dep.hnl".to_string())];
    let js = compile_to_string(
        &module_input(
            "from \"dep\" import { double }\n\
             export quad := (x) ~> double(double(x))\n\
             quad(5)\n",
        ),
        &config,
    )
    .expect("compile cjs fixture");
    let driver = r#"
const ns = require("./prog.cjs");
Promise.resolve(ns).then(async (m) => {
  const q = await m.quad(5);
  // `double` was imported for `quad`'s body, not re-exported — the
  // namespace carries the entry's own exports only.
  console.log(JSON.stringify({ quad: q, def: m.default,
    hasQuad: typeof m.quad === "function", hasDouble: "double" in m }));
}).catch((e) => { console.error(e); process.exit(1); });
"#;
    let run = run_node(
        "cjs-module",
        &[("prog.cjs", js.as_str()), ("driver.cjs", driver)],
        "driver.cjs",
    );
    assert!(run.succeeded(), "node failed:\n{}", run.stderr);
    // quad(5) = double(double(5)) = 20 through the inlined dep; the
    // trailing `quad(5)` is the default export.
    assert_eq!(
        run.stdout.trim(),
        r#"{"quad":20,"def":20,"hasQuad":true,"hasDouble":false}"#
    );
}

#[test]
fn e2e_iife_module_namespace_via_eval() {
    require_node!();
    // IIFE: `(async function () { … })();` — `eval` yields the
    // completion value, the namespace promise.
    let js = compile_to_string(
        &module_input("export plus := (a, b) ~> a + b\nplus(20, 22)\n"),
        &script_format(Iife),
    )
    .expect("compile iife fixture");
    assert!(js.starts_with("(async function()"), "{js}");
    let driver = r#"
import { readFileSync } from "node:fs";
const src = readFileSync("./prog.js", "utf8");
const p = eval(src);
const ns = await p;
// Arrow call convention: one arg is fed bare, several pack into an
// array — `plus(20, 22)` is `ns.plus([20, 22])`.
const total = await ns.plus([20, 22]);
console.log(JSON.stringify({ def: ns.default, total: total,
  hasPlus: typeof ns.plus === "function" }));
"#;
    let run = run_node(
        "iife-module",
        &[("prog.js", js.as_str()), ("driver.mjs", driver)],
        "driver.mjs",
    );
    assert!(run.succeeded(), "node failed:\n{}", run.stderr);
    // plus(20,22) = 42 both as the default export and via the named
    // export — proving the export-surface mapping, not just eval.
    assert_eq!(run.stdout.trim(), r#"{"def":42,"total":42,"hasPlus":true}"#);
}

#[test]
fn e2e_inline_module_namespace_via_eval() {
    require_node!();
    // Inline: `var __handl_rt = …; (async () => { … })();` — a bare
    // classic script whose eval completion value is the promise.
    let js = compile_to_string(
        &module_input("export dbl := (x) ~> x * 2\ndbl(21)\n"),
        &script_format(Inline),
    )
    .expect("compile inline fixture");
    assert!(js.contains("(async () => {"), "{js}");
    let driver = r#"
import { readFileSync } from "node:fs";
const src = readFileSync("./prog.js", "utf8");
const p = eval(src);
const ns = await p;
const v = await ns.dbl(21);
console.log(JSON.stringify({ def: ns.default, dbl: v }));
"#;
    let run = run_node(
        "inline-module",
        &[("prog.js", js.as_str()), ("driver.mjs", driver)],
        "driver.mjs",
    );
    assert!(run.succeeded(), "node failed:\n{}", run.stderr);
    assert_eq!(run.stdout.trim(), r#"{"def":42,"dbl":42}"#);
}

#[test]
fn e2e_worker_entry_iife_classic_script_roundtrip() {
    require_node!();
    // `worker_entry` + `iife`: the bundle is a classic worker script —
    // no ES module syntax — that registers `js.on("self", "message")`
    // exactly like the ESM worker fixture. Node has no real worker
    // `self`, so the driver installs the same minimal fake and evals
    // the script (the classic-script load path a Blob/`importScripts`
    // worker takes).
    let mut config = script_format(Iife);
    config.worker_entry = true;
    let worker_js = compile_to_string(
        &module_input(
            r#"
from "js" import { on as js_on }

js_on("self", "message", (msg) ~> { echo: msg.n, pong: "yes" })
"#,
        ),
        &config,
    )
    .expect("compile iife worker entry");
    assert!(worker_js.starts_with("(async function()"), "{worker_js}");
    assert!(!worker_js.contains("export"), "{worker_js}");
    assert!(!worker_js.contains("import { "), "{worker_js}");
    assert!(!worker_js.contains("module.exports"), "{worker_js}");
    let driver = r#"
import { readFileSync } from "node:fs";
const listeners = new Map();
const sent = [];
globalThis.self = {
  addEventListener: (name, fn) => listeners.set(name, fn),
  removeEventListener: (name) => listeners.delete(name),
  postMessage: (m) => sent.push(m),
  importScripts: () => {},
  close: () => {},
};
const src = readFileSync("./worker.js", "utf8");
eval(src);
const listener = listeners.get("message");
if (typeof listener !== "function") throw new Error("worker did not register a message listener");
listener({ data: { n: 7 } });
await new Promise((r) => setTimeout(r, 100));
if (sent.length !== 1) throw new Error("expected 1 reply, got " + sent.length);
const m = sent[0];
if (m.echo !== 7 || m.pong !== "yes") throw new Error("bad reply: " + JSON.stringify(m));
console.log("iife-worker-ok");
"#;
    let run = run_node(
        "worker-iife",
        &[("worker.js", worker_js.as_str()), ("driver.mjs", driver)],
        "driver.mjs",
    );
    assert!(run.succeeded(), "node failed:\n{}", run.stderr);
    assert_eq!(run.stdout.trim(), "iife-worker-ok");
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

// ---------------------------------------------------------------------------
// T-006 (P5a/P5b): effect semantics — `handle`/`arr <op>` dispatch, the
// `return` clause, carried handlers, residual `IO` formers, and
// compile-error fixtures for the denied spellings (`perform`/`io.perform`
// included — eval's `perform_dispatch` needs machinery outside the
// subset, so only the runtime `__handl_rt.perform` member remains, for
// host/driver JS).
// ---------------------------------------------------------------------------

#[test]
fn e2e_handle_arr_op_dispatch_and_carried_handlers() {
    require_node!();
    // `handle (arr Foo)(21) with {Foo: impl}` performs `Foo` through the
    // handler stack; `handle arr Foo with h` binds a *reusable* handled
    // arrow — the carried handler reinstalls on `f(5)` outside the
    // `handle`.
    let run = run_program(
        "handle-dispatch",
        r#"
f := handle arr Foo with { Foo: (x) ~> x + 1 }
{
  arr_op: handle (arr Foo)(21) with { Foo: (x) ~> x + 1 },
  carried: f(5)
}
"#,
        print_default(),
    );
    assert!(run.succeeded(), "node failed:\n{}", run.stderr);
    assert_eq!(run.stdout.trim(), r#"{"arr_op":22,"carried":6}"#);
}

#[test]
fn e2e_handle_nested_handlers_and_return_clause() {
    require_node!();
    // Innermost-first handler resolution and the `return` clause
    // postlude (`h["return"](r)`).
    let run = run_program(
        "handle-nested",
        r#"
with_ret := handle (arr Foo)(5) with { Foo: (x) ~> x + 1, "return": (v) ~> v * 10 }
nested := handle { a: handle (arr Foo)(1) with { Foo: (x) ~> x + 10 }, b: (arr Foo)(3) } with { Foo: (x) ~> x - 1 }
{ ret: with_ret, nested: nested }
"#,
        print_default(),
    );
    assert!(run.succeeded(), "node failed:\n{}", run.stderr);
    // (5+1)*10 = 60; inner handle → 11, outer body op → 3-1 = 2.
    assert_eq!(run.stdout.trim(), r#"{"ret":60,"nested":{"a":11,"b":2}}"#);
}

#[test]
fn e2e_handle_interpret_io_former_and_runtime_perform() {
    require_node!();
    // The `perform`/`io.perform` op spellings are compile-denied, but
    // the runtime halves remain for host/driver JS: `handle`'s
    // `__interpretIO` post-pass interprets a `type.io` former record —
    // covered sig fields dispatch through the still-installed handler,
    // uncovered entries stay data — and `__handl_rt.perform` does the
    // in-flight dispatch / residual-former mint when called directly.
    let run = run_program(
        "handle-io",
        "x := 1\n",
        r#"
await import("./prog.mjs");
const rt = globalThis.__handl_rt;
const former = {__former__: {__sym__: {name: "type.io", handle: 0}},
                indices: [{Bar: 7, Foo: 3}]};
const impl = async () => ({Foo: async (x) => x + 1});
const interpreted = await rt.handle(async () => former, "T", impl, null);
console.log("io:" + JSON.stringify(interpreted.indices[0]));
const dispatched = await rt.handle(async () => rt.perform("Foo", 5), "T", impl, null);
console.log("perform:" + dispatched);
const residual = await rt.perform("Baz", 9);
console.log("residual:" + JSON.stringify(residual.indices[0]));
// Negative: an `acc`-carrying handler value and a ≥2-ary impl (the
// multi-shot `resume` shape) fail loudly, not silently.
try {
  await rt.handle(async () => 1, "T", async () => ({__acc__: 0}), null);
  console.log("acc:no-throw");
} catch (e) { console.log("acc:" + e.message); }
try {
  await rt.handle(async () => rt.perform("Foo", 1), "T",
    async () => ({Foo: async (a, b) => a}), null);
  console.log("resume:no-throw");
} catch (e) { console.log("resume:" + e.message); }
"#,
    );
    assert!(run.succeeded(), "node failed:\n{}", run.stderr);
    // `Foo` covered → 3+1 = 4; `Bar` uncovered → stays 7. In-flight
    // `perform("Foo", 5)` dispatches → 6. Uncovered `perform` mints
    // the residual former. `__acc__` and ≥2-ary impls throw the
    // documented subset errors.
    assert_eq!(
        run.stdout.trim(),
        r#"io:{"Bar":7,"Foo":4}
perform:6
residual:{"Baz":9}
acc:handle: `acc` clause is not supported by the JS backend
resume:handler impl is ≥2-ary: multi-shot `resume` is not supported by the JS backend"#
    );
}

#[test]
fn e2e_handle_pop_on_throw_and_unsupported_op() {
    require_node!();
    // A body whose op no handler covers throws "unsupported op" inside
    // the dynamic extent; `handle`'s `finally` still pops the frame, so
    // `__handlers` is empty after the rejected module eval — and the
    // error is the eval-faithful diagnostic, not a TypeError.
    let run = run_program(
        "handle-throw",
        r#"
bad := handle (arr Bar)(1) with { Foo: (x) ~> x + 1 }
"#,
        "try { await import(\"./prog.mjs\"); } catch (e) { console.log(\"threw:\" + e.message); }\n\
         console.log(\"handlers:\" + globalThis.__handl_rt.__handlers.length);\n",
    );
    assert!(run.succeeded(), "node failed:\n{}", run.stderr);
    assert!(
        run.stdout.contains("threw:unsupported op: Bar"),
        "{}",
        run.stdout
    );
    assert!(run.stdout.contains("handlers:0"), "{}", run.stdout);
}

#[test]
fn e2e_denylisted_effect_op_spellings_fail_to_compile() {
    // P5a: `handle`/`handle_dyn`/`rewrite`/`path`/`equiv`/`perform`/
    // `io.perform` op spellings are compile errors — they must never
    // reach `__handl_rt.<name>` or `__opRef` (the old stubs fabricated
    // results, and `perform` needs resume-journal machinery the subset
    // does not carry).
    for name in [
        "handle",
        "handle_dyn",
        "rewrite",
        "path",
        "equiv",
        "perform",
        "io.perform",
    ] {
        for args in [
            vec![],
            vec![Term::const_value(Value::Number(serde_json::Number::from(
                1,
            )))],
        ] {
            let term = Term::op_native(name, args);
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
            assert!(
                compile_to_string(&input, &esm_node()).is_err(),
                "op `{name}` unexpectedly compiled"
            );
        }
    }
}

// ---------------------------------------------------------------------------
// T-016 (P-002): compiler configurability — a non-default `JsConfig`
// knob produces working JS under node.
// ---------------------------------------------------------------------------

#[test]
fn e2e_op_lowering_prelude_strategy() {
    require_node!();
    // `JsConfig::op_lowering = OpLowering::Prelude` — the uniform
    // prelude-call strategy: every scalar op emits a `__handl_rt.<name>`
    // member call instead of a raw JS operator. The emitted bundle must
    // still evaluate to the same values under node.
    let mut config = esm_node();
    config.op_lowering = handl_plugin_compile_js::OpLowering::Prelude;
    let run = run_program_with(
        "op-lowering-prelude",
        r#"
add := (a, b) ~> a + b
mul := (a, b) ~> a * b
lt := (a, b) ~> a < b
gt := (a, b) ~> a > b
flip := (n) ~> 0 - n
total := add(mul(6, 7), 8)
half := flip(add(9, 9))
checks := { smaller: lt(3, 4), bigger: gt(5, 4) }
{ total: total, half: half, smaller: checks.smaller, bigger: checks.bigger }
"#,
        config,
        print_default(),
    );
    assert!(run.succeeded(), "node failed:\n{}", run.stderr);
    // 6*7+8 = 50; -(9+9) = -18; 3<4 and 5>4.
    assert_eq!(
        run.stdout.trim(),
        r#"{"total":50,"half":-18,"smaller":true,"bigger":true}"#
    );
}

// ---------------------------------------------------------------------------
// T-010: `data` declarations in expression position — `data` (or the
// `F := { … }` family-record spelling) inside `do { … }` blocks.
// `splice_decl_blocks` flattens the desugared former/ctor/`F.rec` assign
// group into the *enclosing* block's statement list, so the constructors
// bind in that block's scope: usable by later statements of the same
// block, popped with it — eval's `eval_block` pushes a scope and
// `env_assign` defines block-locally (`engine-exec` `native_ops.rs`).
// These fixtures execute the emitted JS under node.
// ---------------------------------------------------------------------------

#[test]
fn e2e_data_decl_inside_do_block_binds_for_later_statements() {
    require_node!();
    // `Nat := { … }` inside `do { … }` desugars to the former/ctor/
    // `Nat.rec` assign group spliced into the block — `Succ`/`Zero`
    // resolve for `two` and the `match` in later statements of the
    // same block. The block's value is its last statement's value.
    let run = run_program(
        "data-in-do",
        r#"
probe := do {
  Nat := { Zero : Nat, Succ : (p : Nat) ~> Nat }
  two := Succ(Succ(Zero))
  match two { Zero: 0, Succ(p): match p { Zero: 1, Succ(q): 2, _: 9 }, _: 8 }
}
probe
"#,
        print_default(),
    );
    assert!(run.succeeded(), "node failed:\n{}", run.stderr);
    // match(Succ(Succ(Zero))) → 2 through two levels of ctor dispatch.
    assert_eq!(run.stdout.trim(), "2");
}

#[test]
fn e2e_data_decl_inside_do_block_scoped_to_block() {
    require_node!();
    // Eval scopes the spliced decl group to the `do` block
    // (`eval_block`'s pushed scope): `Nat`/`Zero`/`Succ` are unbound
    // after the block — the compiled module must fail the same way
    // (`unsupported op: Nat`), not leak the family into module scope.
    // This is also the regression guard for the flattened-decl-group
    // check: a bare `do {}` whose children are *all* bindings must not
    // take the `data`-decl emit path.
    let run = run_program(
        "data-in-do-scope",
        r#"
do {
  Nat := { Zero : Nat, Succ : (p : Nat) ~> Nat }
  leaked := Succ(Zero)
}
Nat
"#,
        "try { const v = await import(\"./prog.mjs\"); \
         console.log(\"imported:\" + JSON.stringify(v.default)); } \
         catch (e) { console.log(\"threw:\" + e.message); }\n",
    );
    assert!(run.succeeded(), "node failed:\n{}", run.stderr);
    assert!(
        run.stdout.contains("threw:unsupported op: Nat"),
        "{}",
        run.stdout
    );
}

#[test]
fn e2e_data_keyword_decl_inside_do_block() {
    require_node!();
    // `data Nat := { … }` (the retired keyword, still accepted) inside
    // a `do` block: the parser's stray `var "data"` marker is stripped
    // the way `module_prep` strips declarations textually, so the
    // constructors bind in the block scope like the keyword-free
    // spelling.
    let run = run_program(
        "data-kw-in-do",
        r#"
probe := do {
  data Nat := { Zero : Nat, Succ : (p : Nat) ~> Nat }
  two := Succ(Succ(Zero))
  match two { Zero: 0, Succ(p): match p { Zero: 1, Succ(q): 2, _: 9 }, _: 8 }
}
probe
"#,
        print_default(),
    );
    assert!(run.succeeded(), "node failed:\n{}", run.stderr);
    assert_eq!(run.stdout.trim(), "2");
}

#[test]
fn e2e_data_keyword_decl_top_level_binds_module_scope() {
    require_node!();
    // `data Nat := { … }` at top level — previously the desugared
    // statement AST's last arena node was an orphaned ctor `alt`, so
    // the last-node-is-root assumption failed the compile outright.
    // The constructors must bind at module scope for later statements.
    let run = run_program(
        "data-kw-top",
        r#"
data Nat := { Zero : Nat, Succ : (p : Nat) ~> Nat }
to_num := n ~> match n { Zero: 0, Succ(p): 1 + to_num(p) }
to_num(Succ(Succ(Zero)))
"#,
        print_default(),
    );
    assert!(run.succeeded(), "node failed:\n{}", run.stderr);
    assert_eq!(run.stdout.trim(), "2");
}

// ---------------------------------------------------------------------------
// T-013: source maps — `JsConfig::source_map` on the single-term path
// appends `//# sourceMappingURL=` and a second `main.js.map` artifact.
// (The `.hnl` module path currently ignores the flag — noted gap.)
// ---------------------------------------------------------------------------

#[test]
fn e2e_source_map_emits_url_and_valid_v3_map() {
    use handl_plugin_compile::{ArtifactContent, Compiler, CompilerConfig};
    use handl_plugin_compile_js::JsCompiler;

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
    let mut config = JsConfig::esm_deno();
    config.source_map = true;
    let output = JsCompiler
        .compile(&input, &config as &dyn CompilerConfig)
        .expect("compile with source_map");
    let js = String::from_utf8_lossy(output.primary_output().expect("primary output")).to_string();
    assert!(
        js.contains("//# sourceMappingURL=main.js.map"),
        "missing sourceMappingURL trailer:\n{js}"
    );
    let map_artifact = output
        .artifacts()
        .iter()
        .find(|a| a.name == "main.js.map")
        .expect("main.js.map artifact");
    let ArtifactContent::String(map) = &map_artifact.content else {
        panic!("source map artifact is not a string");
    };
    let v: Value = serde_json::from_str(map).expect("source map is valid JSON");
    assert_eq!(v["version"], 3, "{map}");
    assert_eq!(v["file"], "main.js", "{map}");
    assert!(
        v["mappings"].as_str().is_some_and(|m| !m.is_empty()),
        "{map}"
    );
    assert_eq!(v["sources"], serde_json::json!(["<source>"]), "{map}");
    assert_eq!(v["sourcesContent"], serde_json::json!(["test"]), "{map}");
    assert!(v["names"].is_array(), "{map}");
    // Flat V3 map (no `sections` — that field is for index maps).
    assert!(v.get("sections").is_none(), "{map}");
}

// ---------------------------------------------------------------------------
// T-013: deterministic property fixture — random arithmetic/record
// pipelines compiled through the module emit and executed under node,
// asserting eval-equivalent output (the expected value is computed in
// Rust while generating the source). Bounded: 8 cases ≈ 8 node spawns.
// ---------------------------------------------------------------------------

/// A random integer arithmetic expression over literals — fully
/// parenthesized `+`/`-`/`*` trees (no division: keeps results integral
/// and JSON-comparison exact). Returns `(source, value)`.
fn gen_arith<R: rand::Rng>(rng: &mut R, depth: usize) -> (String, i64) {
    if depth == 0 || rng.random_range(0..4) == 0 {
        let n = rng.random_range(-20..=20);
        return (n.to_string(), n as i64);
    }
    let (ls, lv) = gen_arith(rng, depth - 1);
    let (rs, rv) = gen_arith(rng, depth - 1);
    match rng.random_range(0..3) {
        0 => (format!("({ls} + {rs})"), lv + rv),
        1 => (format!("({ls} - {rs})"), lv - rv),
        _ => (format!("({ls} * {rs})"), lv * rv),
    }
}

#[test]
fn e2e_fuzz_arithmetic_record_pipelines_match_eval() {
    require_node!();
    use rand::SeedableRng;
    let mut rng = rand::rngs::StdRng::seed_from_u64(0xC0FFEE);
    for case in 0..8 {
        // `{ a: e1, b: e2, c: e3 }` — a record literal of random
        // arithmetic pipelines as the module's trailing value.
        let (e1, v1) = gen_arith(&mut rng, 3);
        let (e2, v2) = gen_arith(&mut rng, 3);
        let (e3, v3) = gen_arith(&mut rng, 2);
        let source = format!("{{ a: {e1}, b: {e2}, c: {e3} }}\n");
        let expected = format!(r#"{{"a":{v1},"b":{v2},"c":{v3}}}"#);
        let run = run_program(&format!("fuzz-{case}"), &source, print_default());
        assert!(run.succeeded(), "case {case} node failed:\n{}", run.stderr);
        assert_eq!(run.stdout.trim(), expected, "case {case} ({source})");
    }
}

// ---------------------------------------------------------------------------
// T-018: `import.alias` end-to-end — aliased `js` contract imports and a
// `* as` namespace binding executed under real `node`.
// ---------------------------------------------------------------------------

#[test]
fn e2e_import_alias_js_contract_and_namespace() {
    require_node!();
    // The original T-018 repro shape plus the single-name and namespace
    // alias forms: `{ on as js_on, call as js_call, await as js_await }`,
    // `* as jsm`. Every alias binds the same `__handl_rt` shim/await op
    // the original name would.
    let run = run_program(
        "import-alias",
        r#"
from "js" import { on as js_on, call as js_call, await as js_await }
from "js" import * as jsm
et := jsm.construct("", "EventTarget", [], [], "js_object", false)
ev := jsm.construct("", "Event", ["ping"], [], "js_object", false)
h := js_on(et, "ping", (e) ~> js_await(js_call("console", "log", ["aliased-ping"], [], "js_object", false)))
jsm.invoke(et, "dispatchEvent", [ev], [], "js_object", false)
jsm.off(et, "ping", h)
"done"
"#,
        print_default(),
    );
    assert!(run.succeeded(), "node failed:\n{}", run.stderr);
    assert!(run.stdout.contains("aliased-ping"), "{}", run.stdout);
    assert_eq!(run.stdout.trim().lines().last().unwrap(), "\"done\"");
}

// ---------------------------------------------------------------------------
// T-008: effect-parity remainder — `dispatchForeign` interception,
// `grades` coverage, and the incoming-value handler channel, exercised
// against the runtime directly (compiled `.hnl` cannot spell `Foreign`
// ops; the runtime member is the honest surface for host/driver JS).
// ---------------------------------------------------------------------------

#[test]
fn e2e_dispatch_foreign_interception_grades_and_incoming() {
    require_node!();
    let run = run_program(
        "dispatch-foreign",
        "x := 1\n",
        r#"
await import("./prog.mjs");
const rt = globalThis.__handl_rt;
globalThis.__probe_mod = { ping: async (n) => n * 10 };
// Covered foreign op: the `handle` frame's impl intercepts before the
// host fallback (`globalThis[module][name]`).
const covered = await rt.handle(
  async () => rt.dispatchForeign("ping", [4], null, null, "__probe_mod", null, true),
  "T", async () => ({ ping: async (a) => a + 1 }), null);
console.log("covered:" + covered);
// Uncovered foreign op falls back to the inline host call.
const bare = await rt.dispatchForeign("ping", [4], null, null, "__probe_mod", null, true);
console.log("bare:" + bare);
// `grades` coverage: the handler declares grades ["S"] — a "T"-dimmed
// dispatch skips it (host fallback), an "S"-dimmed one intercepts.
const graded = await rt.handle(
  async () => ({
    s: await rt.dispatchForeign("ping", [2], "S", null, "__probe_mod", null, true),
    t: await rt.dispatchForeign("ping", [2], "T", null, "__probe_mod", null, true),
  }),
  "T", async () => ({ grades: ["S"], ping: async (a) => a + 100 }), null);
console.log("graded:" + JSON.stringify(graded));
// The incoming value's own `impls` is the second handler channel —
// consulted after the whole stack, with no grades constraint.
const viaIncoming = await rt.dispatchForeign(
  "ping", [2], null, { impls: { ping: async (a) => a + 1000 } },
  "__probe_mod", null, true);
console.log("incoming:" + viaIncoming);
"#,
    );
    assert!(run.succeeded(), "node failed:\n{}", run.stderr);
    assert_eq!(
        run.stdout.trim(),
        r#"covered:5
bare:40
graded:{"s":102,"t":20}
incoming:1002"#
    );
}

#[test]
fn e2e_flow_chain_in_arrow_body_applies_stages() {
    require_node!();
    // T-019 regression: `x >>> s1 >>> s2` inside an arrow body lowers
    // to `Seq` stages whose `snd` are bare `Op{name, []}` lazy
    // references — they must *apply* the named arrows to the stage
    // input (`s2(s1(x))`), not yield the arrow values. `comp` exercises
    // the `Op{flow}` dispatch + quoted-arrow rhs path.
    let run = run_program(
        "flow_seq",
        r#"
s1 := (x) ~> x + 1
s2 := (x) ~> x * 10
pipe := (x) ~> x >>> s1 >>> s2
comp := s1 >>> s2
apply2 := (g, x) ~> g(x)
five := 5
read5 := (a) ~> five
{ piped: pipe(5), composed: comp(5), passed: apply2(s1, 5), read: read5(9) }
"#,
        print_default(),
    );
    assert!(run.succeeded(), "node failed:\n{}", run.stderr);
    // pipe(5) = s2(s1(5)) = 60; comp(5) = 60; apply2(s1,5) = 6;
    // read5(9) reads the non-arrow binding `five` = 5.
    assert_eq!(
        run.stdout.trim(),
        r#"{"piped":60,"composed":60,"passed":6,"read":5}"#
    );
}
