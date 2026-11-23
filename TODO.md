# TODO — compile-js

- [ ] T-011 GAP: untested surface — https: imports in real browser worker, real-browser (non-shim) worker e2e, auto-reply transfer suppression docs · owner:devin · created:2026-11-23 · epic:compile-js-prod
- [ ] T-012 GAP: docs — README capability matrix, worker-entry convention guide, honest limitations list (effect subset, export data, upstream divergences) · owner:devin · created:2026-11-23 · epic:compile-js-prod
- [ ] T-015 DOCS: docs/ semantic-gap rationale (why runtime layer: async arrows, marker records, no delimited continuations) + README.md overview section · pri:high · owner:devin · created:2026-11-23 · links:P-002 · epic:compile-js-prod
- [ ] T-007 GAP: upstream handl-kit-arrow-lower divergences — >>> Seq-vs-compose (comp(5)=10 vs eval 12), {x} field-pun unsupported; fix in host repo or canonical AST normalization · status:in-progress · owner:devin · created:2026-11-23 · epic:compile-js-prod
- [ ] T-009 GAP: 'export data …' surface form — parser/module\_prep produces 'export \<var data>' residue; needs upstream parse fix then emit support · status:in-progress · owner:devin · created:2026-11-23 · epic:compile-js-prod
- [ ] T-019 BUG: >>> flow composition subtle miscompile — 'x >>> s1 >>> s2' yields arrow value where s2(s1(x)) expected in some positions (found by T-017 bench); investigate lower/control.rs flow path · status:in-progress · pri:high · owner:devin · created:2026-11-23 · epic:compile-js-prod
