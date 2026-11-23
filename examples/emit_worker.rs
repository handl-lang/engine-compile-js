//! Dev utility: compile a `.hnl` file to a **worker-entry** JS bundle
//! — `JsConfig::worker_entry` + ESM on the `Browser` target — and print
//! the output. Used by `tests/browser_e2e.mjs`:
//!
//! ```sh
//! cargo run --offline --example emit_worker -- file.hnl > worker.mjs
//! ```
//!
//! Unlike `emit_module` (which builds `JsConfig` field-by-field and is
//! Node-targeted), this starts from `JsConfig::esm_deno()` and flips the
//! knobs that matter for a browser `DedicatedWorkerGlobalScope` bundle,
//! so it stays source-compatible as `JsConfig` grows fields.
use handl_language::{Ast, AstNode};
use handl_plugin_compile::CompilerInput;
use handl_plugin_compile_js::{Browser, Esm, JsConfig, compile_to_string};
use hnl_engine::Term;
use hnl_engine_typecheck::TypecheckReport;
use serde_json::Value;

fn main() {
    let path = std::env::args()
        .nth(1)
        .expect("usage: emit_worker <file.hnl>");
    let src = std::fs::read_to_string(path.as_str()).expect("read source");
    let lang = handl_kit_data_prelude::language_from_yaml().expect("canonical language");
    let mut ast = Ast::new();
    let root = ast.push(AstNode::new("module"));
    let input = CompilerInput::new(
        src,
        ast,
        root,
        lang,
        Term::constant(Value::Null),
        TypecheckReport::default(),
    );
    let mut config = JsConfig::esm_deno();
    config.target = Box::new(Browser);
    config.format = Box::new(Esm);
    config.worker_entry = true;
    match compile_to_string(&input, &config) {
        Ok(js) => print!("{js}"),
        Err(e) => {
            eprintln!("compile error: {e}");
            std::process::exit(1);
        }
    }
}
