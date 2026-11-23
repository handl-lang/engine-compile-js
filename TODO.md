# TODO — compile-js

- [ ] T-007 GAP: upstream handl-kit-arrow-lower divergences — >>> Seq-vs-compose (comp(5)=10 vs eval 12), {x} field-pun unsupported; fix in host repo or canonical AST normalization · owner:devin · created:2026-11-23 · epic:compile-js-prod
- [ ] T-008 GAP: effect parity remainder — acc clause, multi-shot resume, carried-handler reinstall, serialized-Term impls (P5b leftovers; verify acc reachability first) · owner:devin · created:2026-11-23 · epic:compile-js-prod
- [ ] T-009 GAP: 'export data …' surface form — parser/module\_prep produces 'export \<var data>' residue; needs upstream parse fix then emit support · owner:devin · created:2026-11-23 · epic:compile-js-prod
- [ ] T-010 GAP: data decls nested in expression position (inside do {}) — splice\_decl\_blocks flattening works but not e2e-covered; add fixture · owner:devin · created:2026-11-23 · epic:compile-js-prod
- [ ] T-011 GAP: untested surface — https: imports in real browser worker, real-browser (non-shim) worker e2e, auto-reply transfer suppression docs · owner:devin · created:2026-11-23 · epic:compile-js-prod
- [ ] T-012 GAP: docs — README capability matrix, worker-entry convention guide, honest limitations list (effect subset, export data, upstream divergences) · owner:devin · created:2026-11-23 · epic:compile-js-prod
- [ ] T-013 GAP: hardening — SecurityScanner hostile-input corpus expansion, source-map e2e assertion, fuzz/property fixtures for emitted JS · owner:devin · created:2026-11-23 · epic:compile-js-prod
- [ ] T-014 GAP: real-browser e2e via Playwright — worker\_entry bundle in Chromium/Firefox WebWorker, js.on message round-trip, transfer list, https: import smoke · owner:devin · created:2026-11-23 · epic:compile-js-prod
- [ ] T-015 DOCS: docs/ semantic-gap rationale (why runtime layer: async arrows, marker records, no delimited continuations) + README.md overview section · pri:high · owner:devin · created:2026-11-23 · links:P-002 · epic:compile-js-prod
- [ ] T-017 EVAL: strategy experiments per P-002 — bench harness, metrics vs eval baseline, docs/strategy-evaluation.md, defaults from data · pri:high · owner:devin · created:2026-11-23 · links:P-002 · epic:compile-js-prod
- [ ] T-016 CONFIG: audit hardcoded lowering choices → JsConfig knobs (op strategy, async policy, data encoding, formats, worker\_entry, sourcemaps, security, target) · status:in-progress · pri:high · owner:devin · created:2026-11-23 · links:P-002 · epic:compile-js-prod
