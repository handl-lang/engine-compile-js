//! P-002 / T-017 empirical strategy evaluation harness.
//!
//! Compiles every `bench/workloads/*.hnl` fixture to JavaScript under
//! each strategy variant (module format × `OpLowering`), runs the
//! emitted code under `node`, and records wall-time (median/p95),
//! peak RSS, emitted code size, compile time, and module-load
//! (startup) cost. Results land in `bench/results.json` and a markdown
//! summary on stdout.
//!
//! Run: `cargo run --offline --example bench` (add `-- --quick` for a
//! smoke pass with fewer node iterations, `-- --only <substr>` to
//! filter workloads). `node` must be on `PATH`; the harness degrades
//! to compile-only metrics when it is not.
//!
//! Iteration counts are env-tunable: `BENCH_N` (timed iterations,
//! default 30), `BENCH_WARM` (warmup calls, default 5),
//! `BENCH_WORKER_N` (worker round-trips, default 40).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;
use std::time::Instant;

use handl_language::{Ast, AstNode, Language};
use handl_plugin_compile::CompilerInput;
use handl_plugin_compile_js::{JsConfig, Node, OpLowering, compile_to_string};
use hnl_engine::Term;
use hnl_engine_typecheck::TypecheckReport;
use serde_json::{Value, json};

/// Default timed iterations per workload call (spec: N ≥ 30 after warmup).
const DEFAULT_N: usize = 30;
/// Default warmup calls before timing.
const DEFAULT_WARM: usize = 5;
/// Worker round-trips (each is a full postMessage → compute → reply).
const DEFAULT_WORKER_N: usize = 40;
/// Wall-clock guard per node invocation (seconds) — a stuck workload
/// reports an error row instead of hanging the harness.
const NODE_TIMEOUT_SECS: u64 = 180;

fn env_usize(name: &str, default: usize) -> usize {
    std::env::var(name)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

/// The canonical language for `.hnl` module sources (same dev-dep
/// cycle the e2e tests use).
fn canonical_lang() -> &'static Language {
    static LANG: OnceLock<Language> = OnceLock::new();
    LANG.get_or_init(|| {
        handl_kit_data_prelude::language_from_yaml().expect("canonical language assembles")
    })
}

/// A `CompilerInput` for a whole `.hnl` module source — identical to
/// `tests/e2e.rs`'s `module_input`.
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

/// Module output format under test.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FormatKind {
    Esm,
    CommonJs,
    Iife,
    Inline,
}

impl FormatKind {
    fn all() -> [FormatKind; 4] {
        [
            FormatKind::Esm,
            FormatKind::CommonJs,
            FormatKind::Iife,
            FormatKind::Inline,
        ]
    }

    /// File extension the emitted program is written under.
    fn ext(self) -> &'static str {
        match self {
            FormatKind::Esm => "mjs",
            FormatKind::CommonJs => "cjs",
            FormatKind::Iife | FormatKind::Inline => "js",
        }
    }

    /// Short name used in variant labels.
    fn slug(self) -> &'static str {
        match self {
            FormatKind::Esm => "esm",
            FormatKind::CommonJs => "cjs",
            FormatKind::Iife => "iife",
            FormatKind::Inline => "inline",
        }
    }

    /// Build the `JsConfig` for this format + lowering on the Node
    /// target. `worker_entry` is set by the caller for worker fixtures.
    fn config(self, lowering: OpLowering, worker_entry: bool) -> JsConfig {
        let mut config = match self {
            FormatKind::Esm => JsConfig::esm_deno(),
            FormatKind::CommonJs => JsConfig::cjs_node(),
            FormatKind::Iife => {
                let mut c = JsConfig::esm_deno();
                c.format = Box::new(handl_plugin_compile_js::Iife);
                c
            }
            FormatKind::Inline => {
                let mut c = JsConfig::esm_deno();
                c.format = Box::new(handl_plugin_compile_js::Inline);
                c
            }
        };
        config.target = Box::new(Node);
        config.op_lowering = lowering;
        config.worker_entry = worker_entry;
        config
    }
}

/// A strategy variant: module format × op lowering.
#[derive(Debug, Clone, Copy)]
struct Variant {
    name: &'static str,
    format: FormatKind,
    lowering: OpLowering,
}

impl Variant {
    fn all() -> Vec<Variant> {
        let mut out = Vec::new();
        for format in FormatKind::all() {
            for lowering in [OpLowering::Mixed, OpLowering::Prelude] {
                let lname = match lowering {
                    OpLowering::Mixed => "mixed",
                    OpLowering::Prelude => "prelude",
                };
                // Leak the small name string — the harness is a
                // short-lived process.
                let name: &'static str =
                    Box::leak(format!("{}-{lname}", format.slug()).into_boxed_str());
                out.push(Variant {
                    name,
                    format,
                    lowering,
                });
            }
        }
        out
    }
}

/// One bench workload discovered under `bench/workloads/`.
struct Workload {
    /// File stem, e.g. `loop_iter`.
    name: String,
    /// Source text.
    source: String,
    /// `worker_entry` bundle (file name starts with `worker`).
    worker: bool,
}

fn load_workloads(dir: &Path) -> Vec<Workload> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
        .map(|rd| {
            rd.filter_map(|e| e.ok().map(|e| e.path()))
                .filter(|p| p.extension().is_some_and(|e| e == "hnl"))
                .collect()
        })
        .unwrap_or_default();
    files.sort();
    files
        .into_iter()
        .map(|path| {
            let name = path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("workload")
                .to_string();
            Workload {
                worker: name.starts_with("worker"),
                source: std::fs::read_to_string(&path).expect("read workload"),
                name,
            }
        })
        .collect()
}

/// `node --version`, or `None` when node is absent.
fn node_version() -> Option<String> {
    let out = Command::new("node").arg("--version").output().ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// Shared tail of the per-call benchmark driver: warmup, timed
/// iterations, stats + memory report on a `BENCHJSON ` stdout line.
fn bench_tail(n: usize, warm: usize) -> String {
    format!(
        r#"
const WARM = {warm};
const N = {n};
for (let i = 0; i < WARM; i++) await bench(i);
const ts = [];
let sink = 0;
for (let i = 0; i < N; i++) {{
  const t = performance.now();
  const r = await bench(1000 + i);
  ts.push(performance.now() - t);
  sink += (typeof r === "number" ? r : 0);
}}
ts.sort((a, b) => a - b);
const mean = ts.reduce((a, b) => a + b, 0) / N;
const m = process.memoryUsage();
console.log("BENCHJSON " + JSON.stringify({{
  median: ts[Math.floor(N / 2)],
  p95: ts[Math.min(N - 1, Math.floor(N * 0.95))],
  mean,
  min: ts[0],
  max: ts[N - 1],
  heapMB: +(m.heapUsed / 1048576).toFixed(1),
  rssMB: +(m.rss / 1048576).toFixed(1),
  sink,
}}));
"#
    )
}

/// The driver prelude that loads `prog.<ext>` per format and extracts
/// the `bench` arrow (the module's `default` export / namespace entry).
fn load_prelude(format: FormatKind) -> String {
    match format {
        FormatKind::Esm => "import bench from \"./prog.mjs\";\n".to_string(),
        FormatKind::CommonJs => {
            "import ns from \"./prog.cjs\";\nconst bench = (await ns).default;\n".to_string()
        }
        FormatKind::Iife | FormatKind::Inline => {
            "import { readFileSync } from \"node:fs\";\n\
             const rec = await eval(readFileSync(new URL(\"./prog.js\", import.meta.url), \"utf8\"));\n\
             const bench = rec.default;\n"
                .to_string()
        }
    }
}

/// Startup-cost driver: time module load/eval and time until the
/// module's top-level await settles (namespace ready).
fn startup_driver(format: FormatKind) -> String {
    match format {
        FormatKind::Esm => {
            "const t0 = performance.now();\n\
             const m = await import(\"./prog.mjs\");\n\
             const t1 = performance.now();\n\
             console.log(\"STARTJSON \" + JSON.stringify({ load: t1 - t0, ready: t1 - t0, hasDefault: typeof m.default }));\n"
                .to_string()
        }
        FormatKind::CommonJs => {
            "const t0 = performance.now();\n\
             const p = await import(\"./prog.cjs\");\n\
             const t1 = performance.now();\n\
             console.log(\"STARTJSON \" + JSON.stringify({ load: t1 - t0, ready: t1 - t0, hasDefault: typeof p.default }));\n"
                .to_string()
        }
        FormatKind::Iife | FormatKind::Inline => {
            "import { readFileSync } from \"node:fs\";\n\
             const src = readFileSync(new URL(\"./prog.js\", import.meta.url), \"utf8\");\n\
             const t0 = performance.now();\n\
             const pr = eval(src);\n\
             const t1 = performance.now();\n\
             await pr;\n\
             const t2 = performance.now();\n\
             console.log(\"STARTJSON \" + JSON.stringify({ load: t1 - t0, ready: t2 - t0 }));\n"
                .to_string()
        }
    }
}

/// Worker round-trip driver: `n` sequential postMessage → reply
/// cycles, reporting per-cycle latency stats. `is_mjs` selects the
/// emitted bundle name (`worker.mjs` for esm module workers,
/// `worker.js` for classic-script bundles).
fn worker_driver(n: usize, warm: usize, is_mjs: bool) -> String {
    let bundle = if is_mjs { "worker.mjs" } else { "worker.js" };
    format!(
        r#"
import {{ Worker }} from "node:worker_threads";
const w = new Worker(new URL("./{bundle}", import.meta.url));
const N = {n};
const WARM = {warm};
const one = (n) => new Promise((res, rej) => {{
  const h = (m) => {{ w.off("message", h); res(m); }};
  w.on("message", h);
  w.postMessage({{ n }});
  setTimeout(() => rej(new Error("worker reply timeout")), 30000);
}});
for (let i = 0; i < WARM; i++) await one(i);
const ts = [];
let sink = 0;
for (let i = 0; i < N; i++) {{
  const t = performance.now();
  const m = await one(1000 + i);
  ts.push(performance.now() - t);
  sink += (m && typeof m.sum === "number" ? m.sum : 0);
}}
ts.sort((a, b) => a - b);
const mean = ts.reduce((a, b) => a + b, 0) / N;
const mem = process.memoryUsage();
console.log("BENCHJSON " + JSON.stringify({{
  median: ts[Math.floor(N / 2)],
  p95: ts[Math.min(N - 1, Math.floor(N * 0.95))],
  mean, min: ts[0], max: ts[N - 1],
  heapMB: +(mem.heapUsed / 1048576).toFixed(1),
  rssMB: +(mem.rss / 1048576).toFixed(1),
  sink,
}}));
await w.terminate();
"#
    )
}

/// Run `node <script>` inside `dir`, returning stdout on success.
/// Uses `timeout` when available so a stuck program cannot hang the
/// harness; falls back to a bare spawn otherwise.
fn run_node(dir: &Path, script: &str, envs: &[(&str, String)]) -> Result<String, String> {
    let has_timeout = Command::new("timeout")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    let mut cmd = if has_timeout {
        let mut c = Command::new("timeout");
        c.arg(format!("{NODE_TIMEOUT_SECS}s")).arg("node");
        c
    } else {
        Command::new("node")
    };
    cmd.arg(script).current_dir(dir);
    for (k, v) in envs {
        cmd.env(k, v);
    }
    let out = cmd.output().map_err(|e| format!("spawn node: {e}"))?;
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    if !out.status.success() {
        return Err(format!(
            "node exited {:?}: {}{}",
            out.status.code(),
            stdout.lines().last().unwrap_or(""),
            stderr.lines().last().unwrap_or("")
        ));
    }
    Ok(stdout)
}

/// Extract the JSON payload after a `PREFIX ` stdout marker.
fn extract_json(stdout: &str, marker: &str) -> Option<Value> {
    stdout.lines().find_map(|l| {
        l.strip_prefix(marker)
            .and_then(|rest| serde_json::from_str(rest.trim()).ok())
    })
}

/// Peak RSS via `/usr/bin/time -v` when present (kB → MB).
fn peak_rss_mb(dir: &Path, script: &str) -> Option<f64> {
    let time = ["/usr/bin/time", "/bin/time"]
        .iter()
        .find(|p| Path::new(p).exists())?;
    let out = Command::new(time)
        .args(["-v", "node", script])
        .current_dir(dir)
        .env("BENCH_N", "3")
        .env("BENCH_WARM", "1")
        .env("BENCH_WORKER_N", "3")
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&out.stderr);
    text.lines().find_map(|l| {
        l.trim()
            .strip_prefix("Maximum resident set size")
            .and_then(|r| r.split_whitespace().next())
            .and_then(|n| n.parse::<u64>().ok())
            .map(|kb| kb as f64 / 1024.0)
    })
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let quick = args.iter().any(|a| a == "--quick");
    let only = args
        .iter()
        .position(|a| a == "--only")
        .and_then(|i| args.get(i + 1))
        .cloned();

    let bench_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("bench");
    let workloads_dir = bench_dir.join("workloads");
    let out_dir = bench_dir.join("out");
    let _ = std::fs::remove_dir_all(&out_dir);
    std::fs::create_dir_all(&out_dir).expect("create bench/out");

    let (n, warm, worker_n) = if quick {
        (5, 2, 8)
    } else {
        (
            env_usize("BENCH_N", DEFAULT_N),
            env_usize("BENCH_WARM", DEFAULT_WARM),
            env_usize("BENCH_WORKER_N", DEFAULT_WORKER_N),
        )
    };

    let node = node_version();
    eprintln!(
        "bench: node={} n={} warm={} worker_n={} quick={}",
        node.as_deref().unwrap_or("ABSENT"),
        n,
        warm,
        worker_n,
        quick
    );

    let mut workloads = load_workloads(&workloads_dir);
    if let Some(filter) = &only {
        workloads.retain(|w| w.name.contains(filter.as_str()));
    }
    assert!(
        !workloads.is_empty(),
        "no workloads found in {workloads_dir:?}"
    );

    let variants = Variant::all();
    let mut results: Vec<Value> = Vec::new();

    // Warm the compiler once (language snapshot load + JIT) before any
    // timed compile.
    let warm_input = module_input(&workloads[0].source);
    let _ = compile_to_string(
        &warm_input,
        &variants[0]
            .format
            .config(OpLowering::Mixed, workloads[0].worker),
    );

    for wl in &workloads {
        for var in &variants {
            // `commonjs` cannot be a worker bundle (no `module.exports`
            // in a worker global scope) — skip that cell. Worker
            // entries also only make sense with the formats the flag
            // supports; iife/inline reject `node:` imports at compile
            // time, which the harness records as data.
            if wl.worker && var.format == FormatKind::CommonJs {
                continue;
            }
            let label = format!("{}/{}/{}", wl.name, var.name, var.format.ext());
            let run_dir = out_dir.join(format!("{}-{}", wl.name, var.name));
            std::fs::create_dir_all(&run_dir).expect("create run dir");

            // --- compile (timed; two reps, keep all samples) ---
            let config = var.format.config(var.lowering, wl.worker);
            let mut compile_ms = Vec::new();
            let mut js = String::new();
            let mut compile_err = None;
            for _ in 0..2 {
                let input = module_input(&wl.source);
                let t = Instant::now();
                match compile_to_string(&input, &config) {
                    Ok(s) => {
                        compile_ms.push(t.elapsed().as_secs_f64() * 1000.0);
                        js = s;
                    }
                    Err(e) => {
                        compile_err = Some(e.to_string());
                        break;
                    }
                }
            }
            let bytes = js.len();
            let mut row = json!({
                "workload": wl.name,
                "variant": var.name,
                "format": var.format.slug(),
                "lowering": match var.lowering {
                    OpLowering::Mixed => "mixed",
                    OpLowering::Prelude => "prelude",
                },
                "worker": wl.worker,
                "compile_ms": compile_ms,
                "bytes": bytes,
            });
            if let Some(e) = compile_err {
                row["error"] = json!(format!("compile: {e}"));
                results.push(row);
                eprintln!("{label}: COMPILE FAIL {e}");
                continue;
            }

            // --- emit program + drivers ---
            let prog_name = if wl.worker {
                format!("worker.{}", var.format.ext())
            } else {
                format!("prog.{}", var.format.ext())
            };
            std::fs::write(run_dir.join(&prog_name), &js).expect("write prog");

            if node.is_none() {
                row["error"] = json!("node not on PATH — runtime metrics skipped");
                results.push(row);
                eprintln!("{label}: compiled {bytes}B, node absent");
                continue;
            }

            let driver_src = if wl.worker {
                worker_driver(worker_n, warm, var.format == FormatKind::Esm)
            } else {
                format!("{}{}", load_prelude(var.format), bench_tail(n, warm))
            };
            std::fs::write(run_dir.join("driver.mjs"), &driver_src).expect("write driver");
            std::fs::write(run_dir.join("startup.mjs"), startup_driver(var.format))
                .expect("write startup driver");

            // --- run benchmark ---
            match run_node(&run_dir, "driver.mjs", &[]) {
                Ok(stdout) => match extract_json(&stdout, "BENCHJSON ") {
                    Some(stats) => {
                        for (k, v) in stats.as_object().unwrap() {
                            row[k] = v.clone();
                        }
                    }
                    None => {
                        row["error"] = json!(format!(
                            "no BENCHJSON in stdout: {}",
                            stdout.lines().last().unwrap_or("")
                        ));
                    }
                },
                Err(e) => row["error"] = json!(e),
            }

            // --- startup cost + peak RSS ---
            if row.get("error").is_none() {
                match run_node(&run_dir, "startup.mjs", &[]) {
                    Ok(stdout) => {
                        if let Some(s) = extract_json(&stdout, "STARTJSON ") {
                            row["startup"] = s;
                        }
                    }
                    Err(e) => row["startup_error"] = json!(e),
                }
                if let Some(mb) = peak_rss_mb(&run_dir, "driver.mjs") {
                    row["peak_rss_mb"] = json!(mb);
                }
            }

            let status = if row.get("error").is_some() {
                "ERR"
            } else {
                "ok"
            };
            eprintln!(
                "{label}: {status} median={:?}ms p95={:?}ms bytes={bytes}",
                row.get("median"),
                row.get("p95")
            );
            results.push(row);
        }
    }

    // --- correctness check: `sink` must agree across variants -------
    let mut by_workload: BTreeMap<String, Vec<u64>> = BTreeMap::new();
    for r in &results {
        if let (Some(w), Some(s)) = (
            r["workload"].as_str(),
            r["sink"].as_f64().map(|f| f.to_bits()),
        ) {
            by_workload.entry(w.to_string()).or_default().push(s);
        }
    }
    let mut sink_mismatch = Vec::new();
    for (w, sinks) in &by_workload {
        if sinks
            .iter()
            .collect::<std::collections::BTreeSet<_>>()
            .len()
            > 1
        {
            sink_mismatch.push(w.clone());
        }
    }

    let report = json!({
        "bench": "handl-plugin-compile-js P-002/T-017 strategy evaluation",
        "node": node,
        "params": { "n": n, "warm": warm, "worker_n": worker_n, "quick": quick },
        "sink_mismatches": sink_mismatch,
        "results": results,
    });
    let json_path = bench_dir.join("results.json");
    std::fs::write(&json_path, serde_json::to_string_pretty(&report).unwrap())
        .expect("write results.json");
    eprintln!("wrote {}", json_path.display());

    // --- markdown summary -------------------------------------------
    println!("# bench summary (median / p95 ms per bench() call)\n");
    let var_names: Vec<&str> = variants.iter().map(|v| v.name).collect();
    println!("| workload | {} |", var_names.join(" | "));
    println!(
        "|---|{}|",
        var_names
            .iter()
            .map(|_| "---:")
            .collect::<Vec<_>>()
            .join("|")
    );
    let mut workload_names: Vec<&str> = Vec::new();
    for r in report["results"].as_array().unwrap() {
        let w = r["workload"].as_str().unwrap_or("");
        if !workload_names.contains(&w) {
            workload_names.push(w);
        }
    }
    for w in &workload_names {
        let mut cells = Vec::new();
        for v in &var_names {
            let cell = report["results"]
                .as_array()
                .unwrap()
                .iter()
                .find(|r| r["workload"] == *w && r["variant"] == *v)
                .map(|r| {
                    if r.get("error").is_some() {
                        "ERR".to_string()
                    } else {
                        format!(
                            "{:.1} / {:.1}",
                            r["median"].as_f64().unwrap_or(0.0),
                            r["p95"].as_f64().unwrap_or(0.0)
                        )
                    }
                })
                .unwrap_or_else(|| "—".to_string());
            cells.push(cell);
        }
        println!("| {w} | {} |", cells.join(" | "));
    }
    println!("\n(cell = median / p95 ms; compile_ms, bytes, startup in results.json)");
}
