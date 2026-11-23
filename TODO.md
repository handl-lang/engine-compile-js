# TODO — compile-js

- [ ] T-002 P1: fill 5 missing prelude ops — record\_merge, assert, flow, ctor\_name, ctor\_fields (+lowerer arms where needed) · pri:high · owner:devin · created:2026-11-23 · epic:compile-js-prod
- [ ] T-003 P2: contract/data decls — signature exports emit nothing; data decls lower to real ctors + match integration · pri:high · owner:devin · created:2026-11-23 · epic:compile-js-prod
- [ ] T-004 P3: worker-entry convention — self-contained bundle (prelude+deps+entry on js.on message); transfer-list support · pri:high · owner:devin · created:2026-11-23 · epic:compile-js-prod
- [ ] T-005 P4: non-ESM module formats — module compile for iife/inline/cjs; external imports hard-error; export mapping · pri:high · owner:devin · created:2026-11-23 · epic:compile-js-prod
- [ ] T-006 P5: effect semantics parity — handle/rewrite/path/equiv stubs; ADR first: port eval semantics vs honest hard-error subset · pri:high · owner:devin · created:2026-11-23 · epic:compile-js-prod
- [ ] T-001 P0: Node e2e harness — compile fixtures, run under node, assert output; skip w/o node; ci.sh requires it · status:in-progress · pri:high · owner:devin · created:2026-11-23 · epic:compile-js-prod
