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
