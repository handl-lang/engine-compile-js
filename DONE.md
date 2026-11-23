# Done

- T-001 P0: Node e2e harness — compile fixtures, run under node, assert output; skip w/o node; ci.sh requires it · pri:high · owner:devin · created:2026-11-23 · epic:compile-js-prod · done:2026-11-23
- T-002 P1: fill 5 missing prelude ops — record\_merge, assert, flow, ctor\_name, ctor\_fields (+lowerer arms where needed) · pri:high · owner:devin · created:2026-11-23 · epic:compile-js-prod · done:2026-11-23
- T-003 P2: contract/data decls — signature exports emit nothing; data decls lower to real ctors + match integration · pri:high · owner:devin · created:2026-11-23 · epic:compile-js-prod · done:2026-11-23
- T-004 P3: worker-entry convention — self-contained bundle (prelude+deps+entry on js.on message); transfer-list support · pri:high · owner:devin · created:2026-11-23 · epic:compile-js-prod · done:2026-11-23
- T-005 P4: non-ESM module formats — module compile for iife/inline/cjs; external imports hard-error; export mapping · pri:high · owner:devin · created:2026-11-23 · epic:compile-js-prod · done:2026-11-23
- T-006 P5: effect semantics parity — handle/rewrite/path/equiv stubs; ADR first: port eval semantics vs honest hard-error subset · pri:high · owner:devin · created:2026-11-23 · epic:compile-js-prod · done:2026-11-23
- T-016 CONFIG: audit hardcoded lowering choices → JsConfig knobs (op strategy, async policy, data encoding, formats, worker\_entry, sourcemaps, security, target) · pri:high · owner:devin · created:2026-11-23 · links:P-002 · epic:compile-js-prod · done:2026-11-23
- T-014 GAP: real-browser e2e via Playwright — worker\_entry bundle in Chromium/Firefox WebWorker, js.on message round-trip, transfer list, https: import smoke · owner:devin · created:2026-11-23 · epic:compile-js-prod · done:2026-11-23
- T-010 GAP: data decls nested in expression position (inside do {}) — splice\_decl\_blocks flattening works but not e2e-covered; add fixture · owner:devin · created:2026-11-23 · epic:compile-js-prod · done:2026-11-23
- T-013 GAP: hardening — SecurityScanner hostile-input corpus expansion, source-map e2e assertion, fuzz/property fixtures for emitted JS · owner:devin · created:2026-11-23 · epic:compile-js-prod · done:2026-11-23
