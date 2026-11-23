+++
id = "P-002"
title = "JS backend strategy evaluation: prelude runtime vs CPS vs inlined ops — empirical defaults + full compiler configurability"
status = "open"
created = "2026-11-23"
owner = "devin"
priority = "high"
epic = "compile-js-prod"
+++

## Context

compile-js lowers Handl arrow calculus to JS via a shared runtime prelude (`__handl_rt`), uniform async arrows, marker-record data encoding, and a runtime op-dispatch layer for effects. This is one point in a design space; alternatives exist (CPS transform for effects, per-op inlining vs prelude calls, sync-arrow fast paths, different data encodings).

## Proposal

1. **Experiments + metrics**: benchmark emitted-JS across representative workloads (arrow-heavy pipelines, record/object churn, match dispatch, worker round-trips, effect handlers). Measure: runtime perf vs eval baseline, emitted code size, compile time, memory. Compare strategy variants where feasible (e.g. prelude-call vs inlined arithmetic already exists; CPS prototype only if cheap to spike).
2. **Empirical defaults**: set JsConfig defaults from measured data, not taste.
3. **Full configurability**: audit every hardcoded lowering choice and expose meaningful knobs on JsConfig — candidates: op lowering strategy (prelude vs inline), async-arrow policy, data encoding tags, module format, worker_entry, source maps, security strictness, target (node/bun/deno/browser).

## Deliverables

- bench/ harness producing reproducible numbers
- docs/strategy-evaluation.md with methodology + results
- JsConfig surface finalized from data
- myco tasks for each experiment
