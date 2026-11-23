//! Dev utility: dump the desugared AST per top-level statement for a
//! `.hnl` source file — mirrors `module.rs`'s `parse_statement`
//! (`handl_runtime::desugar_pass::apply_desugar`).
//! `cargo run --offline --example dump_ast -- file.hnl`.
use handl_language::{AstRef, Language};
use handl_plugin_ext_diag::split_statements;
use hnl_tool_lex_pattern::lex;
use hnl_tool_parse::{ParserEngine, ParserRequest};
use hnl_tool_parse_pratt::PrattEngine;

fn main() {
    let path = std::env::args().nth(1).expect("usage: dump_ast <file.hnl>");
    let src = std::fs::read_to_string(&path).expect("read source");
    let lang: Language = handl_kit_data_prelude::language_from_yaml().expect("canonical language");
    for (si, chunk) in split_statements(&src).iter().enumerate() {
        println!("=== statement {}: {:?} ===", si + 1, chunk.text);
        let tokens = lex(&lang, &chunk.text).expect("lex");
        if tokens.as_slice().is_empty() {
            continue;
        }
        let req = ParserRequest::new(tokens);
        let ast = PrattEngine::new().parse(&lang, &req).expect("parse");
        if ast.is_empty() {
            continue;
        }
        let ast = handl_runtime::desugar_pass::apply_desugar(&lang, &ast).expect("desugar");
        for (i, n) in ast.nodes().iter().enumerate() {
            println!("{i:3} {:<16} {}", n.kind, n.payload);
        }
        let _ = AstRef::new(ast.len() - 1);
    }
}
