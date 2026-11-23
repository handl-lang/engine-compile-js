//! The `JsCompiler` implementation.

use handl_plugin_compile::{CompileError, Compiler, CompilerConfig, CompilerInput, CompilerOutput};

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
                    return Ok(Box::new(JsOutput::new(full)));
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
        let mut full = if js_config.format.name() == "esm" || js_config.format.name() == "commonjs"
        {
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

            return Ok(Box::new(JsOutput::new_with_source_map(full, map, map_name)));
        }

        Ok(Box::new(JsOutput::new(full)))
    }

    fn target(&self) -> &str {
        "js"
    }
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
