//! The `JsCompiler` implementation.

use handl_plugin_compile::{
    Artifact, ArtifactContent, CompileError, Compiler, CompilerConfig, CompilerInput,
    CompilerOutput,
};
use hnl_tool_codegen::{
    Change, CodegenError, EmitContext, Emitter as CodegenEmitter, IrKind, IrModule,
};

use crate::config::JsConfig;
use crate::emit::source_map::{SourceMapBuilder, span_to_line_col};
use crate::emit::{JsEmitter, SecurityScanner};
use crate::lower;
use crate::output::JsOutput;
use crate::runtime::prelude::runtime_prelude;

/// JavaScript compiler for the Handl arrow calculus.
#[derive(Debug, Default, Clone, Copy)]
pub struct JsCompiler;

impl Compiler for JsCompiler {
    fn compile(
        &self,
        input: &CompilerInput,
        config: &dyn CompilerConfig,
    ) -> Result<Box<dyn CompilerOutput>, CompileError> {
        let js_config = config
            .as_any()
            .downcast_ref::<JsConfig>()
            .ok_or_else(|| CompileError::InvalidInput("expected JsConfig".to_string()))?;

        let artifacts = render_artifacts(input, js_config)?;
        Ok(Box::new(JsOutput::from_artifacts(artifacts)))
    }

    fn target(&self) -> &str {
        "js"
    }
}

/// IR for the JS backend under the desired-state codegen kernel
/// (PL-004 T-092).
///
/// `handl_calculus::Term` alone is **not** a sufficient IR for this
/// backend: the file-level module emit (`module::try_compile_module`)
/// re-derives every top-level statement from `input.source` against
/// `input.language` — the `import`/`export`/`const` surface a single
/// root `Term` does not carry — and `JsConfig` holds boxed
/// target/format trait objects plus the driver's resolved `.hnl`
/// module bundle (`modules`/`entry_imports`), none of which fit the
/// kernel's `EmitContext.config` string map. The honest IR is
/// therefore the full compilation unit *plus* its config; the config
/// rides in the IR until the kernel grows a typed-config seam.
#[derive(Debug)]
pub struct JsIr {
    /// The compilation unit — the same input the legacy `Compiler`
    /// impl receives.
    pub input: CompilerInput,
    /// The JS compiler configuration for this unit.
    pub config: JsConfig,
}

/// `IrKind` tag for the JS compilation-unit IR.
pub const JS_IR_KIND: &str = "compile-js";

impl IrModule for JsIr {
    fn kind(&self) -> IrKind {
        IrKind::new(JS_IR_KIND)
    }
}

/// `Emitter<JsIr>` over the desired-state codegen kernel (PL-004
/// T-092): the same render as [`JsCompiler::compile`], expressed as a
/// desired `Change` set (`main.js`, plus `main.js.map` when
/// `source_map` is on) with no side effects. The legacy `Compiler`
/// impl stays until `handl-plugin-compile` migrates to the kernel
/// pipeline.
impl CodegenEmitter<JsIr> for JsCompiler {
    fn materialize(&self, ir: &JsIr, _req: &EmitContext) -> Result<Vec<Change>, CodegenError> {
        let artifacts = render_artifacts(&ir.input, &ir.config)
            .map_err(|e| CodegenError::Emit(e.to_string()))?;
        Ok(artifacts
            .iter()
            .map(|a| match &a.content {
                ArtifactContent::String(s) => Change::text(a.name.clone(), s.clone()),
                ArtifactContent::Bytes(b) => Change::binary(a.name.clone(), b.clone()),
            })
            .collect())
    }
}

/// Render `input` under `js_config` into the artifact list
/// (`main.js`, optionally `main.js.map`) — shared by the legacy
/// `Compiler` impl and the kernel `Emitter` impl so both produce
/// byte-identical desired state.
fn render_artifacts(
    input: &CompilerInput,
    js_config: &JsConfig,
) -> Result<Vec<Artifact>, CompileError> {
    let format_name = js_config.format.name();

    // `worker_entry` is a bundle contract on top of the module emit:
    // a real `.hnl` module source only, no ES `import` may survive
    // that the worker cannot resolve, and the format must load as a
    // worker — `esm` for module workers (`{ type: "module" }`),
    // `iife`/`inline` for classic workers (a self-contained script:
    // `new Worker(url)`, `importScripts`, or a Blob URL without
    // `type`). `commonjs` is rejected: `module.exports` does not
    // exist in a worker global scope.
    if js_config.worker_entry && !matches!(format_name, "esm" | "iife" | "inline") {
        return Err(CompileError::InvalidInput(format!(
            "worker_entry requires the `esm` (module worker, loaded with \
                 `{{ type: \"module\" }}`) or `iife`/`inline` (classic worker \
                 script) output format — `{format_name}` cannot load as a \
                 worker"
        )));
    }

    // File-level emit: a `.hnl` file is a sequence of top-level
    // statements — walk them and emit `import`/`export`/`const`
    // declarations, lowered per format by `emit_module_surface` for
    // the non-ESM targets. Inputs without a real `Language` (tests'
    // synthetic `CompilerInput`s) fall through to the single-term
    // path, as do unrecognized custom formats.
    if matches!(format_name, "esm" | "commonjs" | "iife" | "inline") {
        match crate::module::try_compile_module(input, js_config)? {
            Some(stmts) => {
                if js_config.worker_entry {
                    check_worker_entry_imports(&stmts)?;
                }
                let emitter = JsEmitter;
                let full = if format_name == "esm" {
                    let body = emitter.emit_module(&stmts)?;
                    if js_config.security_scan {
                        SecurityScanner::new().scan(&body)?;
                    }
                    let prelude = runtime_prelude(js_config.format.as_ref());
                    format!("{prelude}{body}")
                } else {
                    let prelude = runtime_prelude(js_config.format.as_ref());
                    crate::module::emit_module_surface(
                        &stmts,
                        format_name,
                        &prelude,
                        js_config.security_scan,
                    )?
                };
                return Ok(vec![Artifact::new("main.js", full)]);
            }
            None => {
                if js_config.worker_entry {
                    return Err(CompileError::InvalidInput(
                        "worker_entry requires a `.hnl` module source — the \
                             single-term path has no module surface to wire"
                            .to_string(),
                    ));
                }
            }
        }
    }

    let main = lower::with_config(js_config, || lower::lower(&input.term))?;
    let emitter = JsEmitter;
    let main_body = if js_config.security_scan {
        emitter.emit_secure(&main)?
    } else {
        emitter.emit_expr(&main)?
    };
    let wrapped = js_config.format.wrap(&main_body);
    let mut full = if matches!(format_name, "esm" | "commonjs") {
        let prelude = runtime_prelude(js_config.format.as_ref());
        format!("{}{}", prelude, wrapped)
    } else {
        wrapped
    };

    if js_config.source_map {
        let map_name = "main.js.map";
        full.push_str(&format!("\n//# sourceMappingURL={map_name}\n"));

        let (source_line, source_col) = input
            .ast
            .get(input.root)
            .and_then(|n| n.span)
            .map(|s| span_to_line_col(&input.source, s))
            .unwrap_or((0, 0));

        let mut builder = SourceMapBuilder::new("<source>", &input.source, "main.js");
        builder.add_mapping(0, 0, source_line, source_col, None);
        let map = builder.build();

        return Ok(vec![
            Artifact::new("main.js", full),
            Artifact::new(map_name, map),
        ]);
    }

    Ok(vec![Artifact::new("main.js", full)])
}

/// Reject ES `import` specifiers a self-contained worker bundle cannot
/// resolve. Only the entry module emits `JsAst::Import` (dep modules
/// inline into `const __mod_*` IIFEs and never produce import decls),
/// so a top-level scan covers the whole bundle.
///
/// Kept specifiers: builtin module schemes (`node:`, `bun:`, `deno:`)
/// and absolute URLs (`https://…`) — both resolve inside a worker
/// global scope. Everything else (relative paths, filesystem-absolute
/// paths) has no base URL to resolve against when the bundle is loaded
/// from a blob/object URL and must come in through
/// `JsConfig::modules`/`entry_imports` inlining instead.
fn check_worker_entry_imports(stmts: &[lower::JsAst]) -> Result<(), CompileError> {
    for stmt in stmts {
        if let lower::JsAst::Import { spec, .. } = stmt
            && !worker_import_resolvable(spec)
        {
            return Err(CompileError::InvalidInput(format!(
                "worker_entry: `import … from {spec:?}` cannot resolve inside a \
                 self-contained worker bundle — resolve it to a `.hnl` module via \
                 `JsConfig::modules`/`entry_imports` so it inlines, or use a \
                 `node:`/`bun:`/`deno:` builtin or absolute-URL specifier"
            )));
        }
    }
    Ok(())
}

/// Whether an ES module specifier resolves without a base URL.
fn worker_import_resolvable(spec: &str) -> bool {
    spec.starts_with("node:")
        || spec.starts_with("bun:")
        || spec.starts_with("deno:")
        || spec.contains("://")
}

/// Convenience: compile `input` with `config` and return the generated JS string.
pub fn compile_to_string(input: &CompilerInput, config: &JsConfig) -> Result<String, CompileError> {
    let compiler = JsCompiler;
    let output = compiler.compile(input, config)?;
    match output.primary_output() {
        Some(bytes) => Ok(String::from_utf8_lossy(bytes).to_string()),
        None => Err(CompileError::Emit("no primary JS output".to_string())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use handl_language::{Ast, AstNode, Language};
    use hnl_engine::Term;
    use hnl_tool_typecheck::TypecheckReport;
    use serde_json::Value;

    fn input(term: Term) -> CompilerInput {
        let source = "test".to_string();
        let mut ast = Ast::new();
        let root = ast.push(AstNode::new("module"));
        let language = Language::new();
        let report = TypecheckReport::default();
        CompilerInput::new(source, ast, root, language, term, report)
    }

    #[test]
    fn js_ir_kind_is_compile_js() {
        let ir = JsIr {
            input: input(Term::constant(Value::Null)),
            config: JsConfig::esm_deno(),
        };
        assert_eq!(ir.kind().as_str(), JS_IR_KIND);
        assert_eq!(ir.kind().as_str(), "compile-js");
    }

    #[test]
    fn materialize_matches_compile_artifacts() {
        for config in [JsConfig::esm_deno(), JsConfig::cjs_node()] {
            let term = Term::constant(Value::Null);
            let expected = JsCompiler
                .compile(&input(term.clone()), &config)
                .expect("compile");
            let ir = JsIr {
                input: input(term),
                config,
            };
            let changes = JsCompiler
                .materialize(&ir, &EmitContext::new())
                .expect("materialize");
            let artifacts = expected.artifacts();
            assert_eq!(changes.len(), artifacts.len(), "{changes:?}");
            for (change, artifact) in changes.iter().zip(artifacts.iter()) {
                assert_eq!(change.artifact().as_str(), artifact.name);
                let want: &[u8] = match &artifact.content {
                    ArtifactContent::String(s) => s.as_bytes(),
                    ArtifactContent::Bytes(b) => b.as_slice(),
                };
                assert_eq!(change.content().unwrap(), want);
            }
        }
    }

    #[test]
    fn materialize_source_map_emits_map_artifact() {
        let mut config = JsConfig::esm_deno();
        config.source_map = true;
        let ir = JsIr {
            input: input(Term::constant(Value::Null)),
            config,
        };
        let changes = JsCompiler
            .materialize(&ir, &EmitContext::new())
            .expect("materialize");
        assert_eq!(changes.len(), 2);
        assert_eq!(changes[0].artifact().as_str(), "main.js");
        assert_eq!(changes[1].artifact().as_str(), "main.js.map");
        assert!(
            String::from_utf8_lossy(changes[0].content().unwrap())
                .contains("sourceMappingURL=main.js.map")
        );
    }
}
