//! Dev utility: compile a `.hnl` file to a JS module with the canonical
//! language and print the output — `cargo run --offline --example
//! emit_module -- file.hnl > out.mjs`.
use handl_language::{Ast, AstNode};
use handl_plugin_compile::CompilerInput;
use handl_plugin_compile_js::{Esm, JsConfig, compile_to_string};
use plugin_engine_typecheck::TypecheckReport;
use plugin_engine::Term;
use serde_json::Value;

fn main() {
    let path = std::env::args().nth(1).expect("usage: emit_module <file.hnl>");
    let src = std::fs::read_to_string(&path).expect("read source");
    let lang = handl_kit_data_canonical::language_from_yaml().expect("canonical language");
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
    };
    match compile_to_string(&input, &config) {
        Ok(js) => print!("{js}"),
        Err(e) => {
            eprintln!("compile error: {e}");
            std::process::exit(1);
        }
    }
}
