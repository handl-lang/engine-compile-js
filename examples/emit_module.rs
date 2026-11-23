//! Dev utility: compile a `.hnl` file to a JS module with the canonical
//! language and print the output — `cargo run --offline --example
//! emit_module -- file.hnl > out.mjs`. Pass `--worker` for a
//! self-contained worker-entry bundle (`JsConfig::worker_entry`).
use handl_language::{Ast, AstNode};
use handl_plugin_compile::CompilerInput;
use handl_plugin_compile_js::{Esm, JsConfig, compile_to_string};
use hnl_engine::Term;
use hnl_engine_typecheck::TypecheckReport;
use serde_json::Value;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let worker_entry = args.iter().any(|a| a == "--worker");
    let path = args
        .iter()
        .find(|a| a.as_str() != "--worker")
        .expect("usage: emit_module [--worker] <file.hnl>");
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
    let config = JsConfig {
        target: Box::new(handl_plugin_compile_js::Node),
        format: Box::new(Esm),
        debug: false,
        no_inline_foreign: false,
        source_map: false,
        modules: Vec::new(),
        entry_imports: Vec::new(),
        worker_entry,
        op_lowering: handl_plugin_compile_js::OpLowering::Mixed,
        loop_max_iterations: handl_plugin_compile_js::DEFAULT_LOOP_MAX_ITERATIONS,
        security_scan: true,
        unsafe_inline_foreign: false,
        dispatch_all_ops: false,
    };
    match compile_to_string(&input, &config) {
        Ok(js) => print!("{js}"),
        Err(e) => {
            eprintln!("compile error: {e}");
            std::process::exit(1);
        }
    }
}
