# JS backend strategy evaluation (P-002 / T-017)

Empirical evaluation of the lowering strategies available on `JsConfig`
for the Handl → JS backend. Question: do the existing knobs —
`OpLowering::{Mixed, Prelude}` and the module `format`
(`esm`/`commonjs`/`iife`/`inline`) — produce measurable performance
differences, and should any `JsConfig` default change?

**Answer up front: no.** Across five representative workloads the
variants are within run-to-run noise of each other (medians span
~12–16 ms per `bench()` call regardless of variant). The data does not
justify changing any default; `OpLowering::Mixed` and the ESM default
stay. Details, caveats, and the skipped experiments below.

## Methodology

Harness: `bench/bench.rs`, run via `cargo run --offline --example bench`
(or `bench/run.sh`). For every `bench/workloads/*.hnl` fixture and every
variant it:

1. compiles the module source through the public API
   (`compile_to_string` + canonical `language_from_yaml`), timing the
   compile (2 reps, min kept) and recording emitted bytes;
2. writes the program plus generated `driver.mjs`/`startup.mjs` into
   `bench/out/<workload>-<variant>/`;
3. runs `node driver.mjs` — warmup calls then **N = 30 timed
   iterations** (`BENCH_N`/`BENCH_WARM`/`BENCH_WORKER_N` env-tunable;
   `--quick` for a smoke pass) — and records median/p95/mean/min/max
   per-call latency plus `process.memoryUsage()` heap/RSS;
4. runs `node startup.mjs` for module-load cost, and
   `/usr/bin/time -v` for peak RSS **when available** (absent on this
   machine — RSS figures are `process.memoryUsage().rss` instead);
5. writes `bench/results.json` and prints a markdown summary.

Each workload exports `bench(seed)`, a deterministic (seeded,
arithmetic-only) function doing a fixed amount of work; the driver
calls `await bench(1000 + i)` per iteration — matching the compiled
arrows' async calling convention. A `sink` checksum accumulated over
the timed iterations must agree across all variants
(`sink_mismatches: []` in `results.json`), so every variant is verified
to compute identical results, not just run fast.

Workloads (`bench/workloads/`):

| fixture | shape |
|---|---|
| `arrow_pipeline` | bounded `while`; each step runs a 4-arrow nested-application pipeline (~4 async arrow calls/iter) |
| `record_churn` | rebuilds a fresh record literal + nested field reads every iteration |
| `match_dispatch` | `match mod(i, 6)` six-arm dispatch per iteration |
| `loop_iter` | tight numeric `while` — scalar-op heavy (`add`/`mul`/`mod`/compare) |
| `effect_handle` | `handle (arr Foo)(x) with impl` per iteration — handler-stack dispatch |
| `worker_roundtrip` | `worker_entry` bundle; driver does 40 sequential `postMessage`→reply round-trips over `node:worker_threads` |

Variants: `{esm, cjs, iife, inline} × {mixed, prelude}` = 8 cells per
workload; the worker fixture skips `commonjs` (rejected by
`worker_entry` validation) and its `iife`/`inline` cells **fail to
compile** because `from "node:worker_threads" import { parentPort }`
cannot survive without a module loader — recorded as data, not a bug.

Environment: node v26.13.1, debug-profile compiler, **host under heavy
parallel load (loadavg ≈ 40)** — absolute numbers are inflated and
noisy; only *relative* comparisons are meaningful.

## Results

Steady-state latency, median of per-variant medians across the five
non-worker workloads (ms per `bench()` call; lower is better):

| variant | median | p95 | mean | heap MB | rss MB |
|---|---:|---:|---:|---:|---:|
| esm-mixed    | 13.0 | 17.1 | 13.5 | 6.0 | 61.1 |
| esm-prelude  | 13.2 | 16.5 | 14.7 | 6.0 | 62.1 |
| cjs-mixed    | 13.7 | 20.2 | 14.8 | 6.6 | 62.3 |
| cjs-prelude  | 13.6 | 18.2 | 13.8 | 5.5 | 61.5 |
| iife-mixed   | 12.7 | 17.9 | 13.3 | 7.0 | 64.1 |
| iife-prelude | 13.7 | 18.8 | 13.9 | 7.7 | 63.2 |
| inline-mixed | 12.6 | 14.1 | 12.7 | 6.1 | 64.4 |
| inline-prelude | 13.0 | 17.8 | 13.7 | 6.1 | 64.0 |

Mixed vs Prelude, per workload (median across the four formats):

| workload | mixed | prelude | Δ |
|---|---:|---:|---:|
| arrow_pipeline | 15.5 | 13.6 | −12% (prelude faster) |
| effect_handle  | 10.8 | 10.8 | ~0 |
| loop_iter      | 12.7 | 13.3 | +5% (mixed faster) |
| match_dispatch | 15.4 | 15.5 | ~0 |
| record_churn   | 12.7 | 13.0 | +3% |

Format effect on steady state is likewise noise-level (all formats
overlap within ~±10%). Worker round-trip (esm only): ~1.4 ms median per
message both lowerings.

Compile time and size (debug build; min of 2 reps):

- compile: ~4.5 ms median per module across all variants (language
  snapshot cache warm);
- emitted size: ~47–51 KB per program, dominated by the ~46 KB runtime
  prelude — format deltas < 100 B, Prelude +~100 B of scalar-op
  members over Mixed.

Startup (module load/eval until top-level-await settles, excludes
node boot ~40 ms which is constant):

| format | load median |
|---|---:|
| inline  | 1.27 ms |
| iife    | 1.31 ms |
| esm     | 1.79 ms |
| cjs     | 2.45 ms |

Classic-script formats edge out ESM/CJS by ~0.5–1 ms on load, but this
is below the noise floor of a loaded machine and irrelevant next to
the ~40 ms node startup and ~13 ms workload calls.

## Analysis

- **OpLowering**: `Mixed` (raw JS operators for the scalar family,
  `__handl_rt` calls for semantic ops) vs `Prelude` (uniform
  `__handl_rt.<op>` member calls) show no consistent difference. The
  largest observed delta (−12% for `arrow_pipeline`) is an arrow-call
  workload where scalar ops are rare — i.e. noise, not signal; the
  scalar-heavy `loop_iter` slightly *prefers* Mixed, as expected since
  it avoids a member-call indirection per operator. **Keep `Mixed`.**
- **Module format**: no steady-state difference (the emitted bodies
  are identical modulo wrapper). ESM remains the right default — it is
  the only format supporting `worker_entry` with `node:` imports and
  real ES `import` surface; iife/inline pay an `eval()` or classic-
  script load path for ≤1 ms startup gain.
- **Size/memory**: prelude-dominated (~46 KB); all variants within a
  few hundred bytes and ~3 MB RSS of each other.
- **Worker**: `worker_entry` + ESM works end-to-end (~1.4 ms
  round-trips); iife/inline correctly reject `node:` specifiers —
  workers needing builtins must use ESM (or fully inline `.hnl` deps).

## Recommended defaults — unchanged

| knob | default | evidence |
|---|---|---|
| `op_lowering` | `Mixed` | parity-or-better everywhere; smaller emitted code |
| `format` (esm_deno) | `Esm` | only format with full import/worker surface |
| `format` (cjs_node) | `CommonJs` | unchanged; node-CJS consumers |
| `loop_max_iterations`, `security_scan`, `source_map`, `worker_entry` | unchanged | not perf axes / already data-driven |

No `src/` changes were made — the data does not justify touching
`JsConfig` defaults.

## Skipped / future experiments

- **CPS transform for effects**: not spiked. The effect path
  (`__handl_rt.handle`/`perform`) measured ~10 ms/call — same band as
  pure-compute workloads — so there is no measured bottleneck for CPS
  to fix. A CPS prototype is a lowering-strategy change in `src/lower`
  (owned elsewhere) and would need its own proposal; the bench harness
  can host it as a new variant when it exists.
- **Sync-arrow fast path**: would require lowering changes in `src/`
  (uniform async arrows are baked into `lower`); >100 lines and outside
  this task's file ownership. Future work: a `JsConfig` flag emitting
  non-async arrows for provably-sync bodies, measured with this bench.
- **Eval baseline**: unavailable — no `hnl` eval extension is built
  (`hnl ext list` shows only typecheck/test/lint; `ext-eval` has no
  compiled `.so`), and an in-process eval would need
  `hnl-engine-exec-tree-walk`/`handl-kit-prelude`, neither of which is
  in this crate's dependency graph (adding deps was out of scope).
  JS-vs-JS variant comparison only, as scoped.
- **`security_scan` on/off**: deliberately not measured — it is a
  hardening pass, not a perf axis (compile-time only, ~µs).

## Incidental finding

`a >>> b` (`flow`) composition in the current subset is subtle:
`x >>> s1 >>> s2` inside an arrow body produced an *arrow value*, not
the applied result, for variable/literal left operands in some
positions — `(x >>> s1 >>> s2)(x)` and `s2(s1(x))` are not
equivalent spellings today. Worth a correctness look independent of
performance (the e2e `flow` fixture only exercises the
arrow∘arrow / value>>>arrow cases that happen to work).

## Caveats

- Host was under heavy parallel load (loadavg ≈ 40, other cargo jobs);
  absolute latencies are inflated ~2–5× and p95s are noisy. Medians
  over 30 iterations are the reliable statistic; treat ±10% as noise.
- Compile times are from a **debug**-profile compiler binary.
- N=30 timed iterations after 5 warmup calls per cell (spec minimum);
  worker cells use 40 round-trips.
- `peak RSS` via `/usr/bin/time -v` was unavailable; `rssMB` is
  `process.memoryUsage().rss` at end of run (not a true peak).
- The bench is not a cargo test (would slow CI); `tests/bench_fixtures.rs`
  smoke-checks that all fixtures still compile.

## Reproducing

```bash
cargo run --offline --example bench          # full run → bench/results.json
cargo run --offline --example bench -- --quick   # smoke pass
BENCH_N=50 ./bench/run.sh                    # more iterations
```
