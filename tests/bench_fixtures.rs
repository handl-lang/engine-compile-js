//! Smoke check: every `bench/workloads/*.hnl` fixture compiles through
//! the public API under its intended configuration (worker fixtures
//! with `worker_entry`, the rest as plain modules). Keeps the P-002/T-017
//! bench corpus honest — a fixture that stops compiling fails here
//! long before the bench harness reports it as an error row.

use std::path::Path;
use std::sync::OnceLock;

use handl_language::{Ast, AstNode, Language};
use handl_plugin_compile::CompilerInput;
use handl_plugin_compile_js::{JsConfig, Node, compile_to_string};
use hnl_engine::Term;
use hnl_engine_typecheck::TypecheckReport;
use serde_json::Value;

fn canonical_lang() -> &'static Language {
    static LANG: OnceLock<Language> = OnceLock::new();
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
fn bench_workload_fixtures_compile() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("bench/workloads");
    let mut files: Vec<_> = std::fs::read_dir(&dir)
        .expect("bench/workloads dir")
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|e| e == "hnl"))
        .collect();
    files.sort();
    assert!(!files.is_empty(), "no bench workloads found");

    for path in files {
        let name = path.file_stem().unwrap().to_string_lossy().to_string();
        let worker = name.starts_with("worker");
        let source = std::fs::read_to_string(&path).expect("read workload");
        let mut config = JsConfig::esm_deno();
        config.target = Box::new(Node);
        config.worker_entry = worker;
        let js = compile_to_string(&module_input(&source), &config)
            .unwrap_or_else(|e| panic!("{name}.hnl failed to compile: {e}"));
        assert!(!js.is_empty(), "{name}.hnl compiled to empty output");
    }
}
