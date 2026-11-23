+++
id = "I-001"
title = "0-arg shim value-position emits referent not call (js_error() reads as bare member)"
status = "done"
created = "2026-11-23"
priority = "med"
done = "2026-11-23"
+++

# 0-arg shim value-position emits referent not call (js_error() reads as bare member)

Status: **open** · Created: 2026-11-23

## Description

What is wrong and steps to reproduce.

## Checklist

- [ ] Action item

## Resolution

Fixed in src/lower/primitive.rs: BoundRef::Shim with zero args in value position now emits __handl_rt.<member>() (a call) instead of the bare member referent; scheme position (x >>> op) keeps the __appOrValue referent reading. Arrow-lower collapses f() and bare f to the same Op{name, []}, so the call reading was chosen — it matches eval's zero-arg foreign-op invocation and all real uses (js_error() in lib-js worker/fetch-js/concurrent-js). Tests: module_js_zero_arg_shim_value_position_calls (src/tests.rs) + e2e_js_error_zero_arg_value_position_calls (tests/e2e.rs).
