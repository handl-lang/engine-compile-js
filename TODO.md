# TODO — compile-js

- [ ] T-011 GAP: untested surface — https: imports in real browser worker, real-browser (non-shim) worker e2e, auto-reply transfer suppression docs · owner:devin · created:2026-11-23 · epic:compile-js-prod
- [ ] T-012 GAP: docs — README capability matrix, worker-entry convention guide, honest limitations list (effect subset, export data, upstream divergences) · owner:devin · created:2026-11-23 · epic:compile-js-prod
- [ ] T-015 DOCS: docs/ semantic-gap rationale (why runtime layer: async arrows, marker records, no delimited continuations) + README.md overview section · pri:high · owner:devin · created:2026-11-23 · links:P-002 · epic:compile-js-prod
- [ ] T-021 UPSTREAM kernel: Term::Handle/TermData::Handle need acc field for lossless lowering (kernel/crates/handl-calculus/src/term.rs:150-157) · pri:high · owner:devin · created:2026-11-23 · epic:compile-js-prod
- [ ] T-022 UPSTREAM host: 'acc' needs contextual keyword in parser/lexer (lexer\_rules.yaml:226-230 reserved-word breakage); then perform\_acc\_state\_threading goes green · pri:high · owner:devin · created:2026-11-23 · epic:compile-js-prod
- [ ] T-023 UPSTREAM lib-loader: src/index.hnl lacks export data handling (bootstrap fallback covers); loader.rs bootstrap mirror diverges — update for parity · pri:high · owner:devin · created:2026-11-23 · epic:compile-js-prod
- [ ] T-020 T-020 JS-side: wire 'export data …' through engine-compile-js module.rs (upstream module\_prep fixed in host@4c16fbb5) + e2e fixture · status:in-progress · pri:high · owner:devin · created:2026-11-23 · epic:compile-js-prod
