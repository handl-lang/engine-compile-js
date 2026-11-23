//! Hostile-input corpus for the ADR-0114 Phase-7 security surface:
//! `validate_foreign_descriptor` / `is_safe_for_inlining`
//! (`src/security.rs`) and the post-emit `SecurityScanner`
//! (`src/emit/mod.rs`). These tests pin the *real* reject paths —
//! every case below was checked against the scanner's actual pattern
//! list, not an idealized one.

use handl_plugin_compile_js::{
    ForeignDescriptor, SecurityScanner, is_safe_for_inlining, validate_foreign_descriptor,
};
use serde_json::{Value, json};

fn desc(module: &str, name: &str, arg_types: &[&str], return_type: &str) -> Value {
    json!({
        "module": module,
        "name": name,
        "arg_types": arg_types,
        "return_type": return_type,
    })
}

fn fd(module: &str, name: &str, arg_types: &[&str], return_type: &str) -> ForeignDescriptor {
    ForeignDescriptor {
        module: module.to_string(),
        name: name.to_string(),
        arg_types: arg_types.iter().map(|s| s.to_string()).collect(),
        return_type: return_type.to_string(),
    }
}

// ---------------------------------------------------------------------------
// validate_foreign_descriptor — structural validation
// ---------------------------------------------------------------------------

#[test]
fn foreign_descriptor_rejects_non_object_and_missing_fields() {
    for bad in [
        json!(42),
        json!("console.log"),
        json!([]),
        json!(null),
        json!({"module": "x"}),
        json!({"name": "log"}),
        // Missing `arg_types` / `return_type`.
        json!({"module": "x", "name": "log"}),
        json!({"module": "x", "name": "log", "arg_types": []}),
    ] {
        assert!(
            validate_foreign_descriptor(&bad).is_err(),
            "accepted hostile descriptor: {bad}"
        );
    }
}

#[test]
fn foreign_descriptor_rejects_wrong_field_types() {
    for bad in [
        // Non-string scalars.
        json!({"module": 1, "name": "log", "arg_types": [], "return_type": "void"}),
        json!({"module": "x", "name": null, "arg_types": [], "return_type": "void"}),
        json!({"module": "x", "name": "log", "arg_types": "string", "return_type": "void"}),
        // `arg_types` must be an array *of strings*.
        json!({"module": "x", "name": "log", "arg_types": [1, "ok"], "return_type": "void"}),
        json!({"module": "x", "name": "log", "arg_types": [null], "return_type": "void"}),
        json!({"module": "x", "name": "log", "arg_types": [], "return_type": 7}),
    ] {
        assert!(
            validate_foreign_descriptor(&bad).is_err(),
            "accepted malformed descriptor: {bad}"
        );
    }
}

#[test]
fn foreign_descriptor_accepts_ordinary_host_calls() {
    for ok in [
        desc("console", "log", &["any"], "void"),
        desc("Math", "max", &["number", "number"], "number"),
        desc("fetch-js", "get", &["string"], "promise"),
        // Extra fields are ignored — forward-compatible.
        json!({"module": "x", "name": "f", "arg_types": [], "return_type": "void", "doc": "hi"}),
    ] {
        assert!(
            validate_foreign_descriptor(&ok).is_ok(),
            "rejected benign descriptor: {ok}"
        );
    }
}

// ---------------------------------------------------------------------------
// validate_foreign_descriptor — dangerous-value rejection
// (`dangerous_string`: `<script`, `eval`, `function`, `setimmediate`,
// `settimeout`, `document.write`, `</script` — all case-insensitive
// substring matches over module/name/arg_types/return_type)
// ---------------------------------------------------------------------------

#[test]
fn foreign_descriptor_rejects_eval_and_function_names() {
    for (module, name) in [
        ("x", "eval"),
        ("eval", "log"),
        ("x", "evalScript"), // substring: `eval` inside a longer name
        ("x", "Function"),
        ("x", "new Function"),
        // Case-insensitive.
        ("x", "EVAL"),
        ("x", "function"),
    ] {
        let d = desc(module, name, &[], "void");
        assert!(
            validate_foreign_descriptor(&d).is_err(),
            "accepted dangerous descriptor {module}.{name}"
        );
    }
}

#[test]
fn foreign_descriptor_rejects_timer_and_document_write() {
    for name in ["setTimeout", "SETTIMEOUT", "setImmediate", "document.write"] {
        let d = desc("x", name, &[], "void");
        assert!(
            validate_foreign_descriptor(&d).is_err(),
            "accepted dangerous descriptor name `{name}`"
        );
    }
    // The scan covers every string field, not just module/name.
    let d = desc("x", "f", &["setTimeout"], "void");
    assert!(validate_foreign_descriptor(&d).is_err());
    let d = desc("x", "f", &[], "evalResult");
    assert!(validate_foreign_descriptor(&d).is_err());
}

#[test]
fn foreign_descriptor_rejects_script_breakout_sequences() {
    // `</script>` inside a descriptor string survives `serde_json`
    // escaping and lands verbatim in the emitted JS — inside an
    // inline-HTML `<script>` embed it terminates the block (the
    // scanner's `<script` check does not cover the *closing* tag).
    for field in [
        desc("x</script><script>alert(1)</script>", "f", &[], "void"),
        desc("x", "f</script>", &[], "void"),
    ] {
        assert!(
            validate_foreign_descriptor(&field).is_err(),
            "accepted script-breakout descriptor: {field}"
        );
    }
}

// ---------------------------------------------------------------------------
// is_safe_for_inlining — the direct `globalThis[module][name](...)`
// verdict (non-inlined calls route through `__handl_rt.jsCall` instead)
// ---------------------------------------------------------------------------

#[test]
fn inlining_rejects_non_static_identifiers() {
    for bad in ["", "my mod", "foo;bar", "foo/bar", "foo(bar)", "café"] {
        assert!(
            !is_safe_for_inlining(&fd(bad, "log", &[], "void")),
            "inlined module `{bad}`"
        );
        assert!(
            !is_safe_for_inlining(&fd("x", bad, &[], "void")),
            "inlined name `{bad}`"
        );
    }
}

#[test]
fn inlining_rejects_blocked_substrings_in_any_field() {
    // `blocked_for_inlining` scans module, name, return_type, arg_types.
    assert!(!is_safe_for_inlining(&fd("x", "log", &[], "evalResult")));
    assert!(!is_safe_for_inlining(&fd(
        "x",
        "log",
        &["function"],
        "void"
    )));
    // Sanity: the same shape with clean fields inlines.
    assert!(is_safe_for_inlining(&fd(
        "console",
        "log",
        &["any"],
        "void"
    )));
    // Dotted/kebab module names are static identifiers — allowed.
    assert!(is_safe_for_inlining(&fd(
        "fetch-js",
        "get",
        &["string"],
        "promise"
    )));
    assert!(is_safe_for_inlining(&fd("a.b-c", "f", &[], "void")));
}

#[test]
fn inlining_settimeout_with_string_arg_is_rejected() {
    // `setTimeout("code", ms)` evals a string — the one timer spelling
    // `is_safe_for_inlining` special-cases. Note the belt-and-suspenders:
    // `blocked_for_inlining` already rejects the `settimeout` substring in
    // `fd.name`, so *every* setTimeout descriptor is non-inlinable anyway;
    // the arg-type check matters only for descriptors whose name merely
    // contains "setTimeout"… which is also substring-blocked. The case
    // stays as a regression pin for the special-case itself.
    assert!(!is_safe_for_inlining(&fd(
        "x",
        "setTimeout",
        &["string", "number"],
        "void"
    )));
    assert!(!is_safe_for_inlining(&fd(
        "x",
        "setTimeout",
        &["String"],
        "void"
    )));
    // Non-string arg types are still non-inlinable because the *name*
    // contains `settimeout` — the special case can never make it worse.
    assert!(!is_safe_for_inlining(&fd(
        "x",
        "setTimeout",
        &["function", "number"],
        "void"
    )));
}

// ---------------------------------------------------------------------------
// SecurityScanner::scan — the post-emit substring pass. Documented gaps:
// the scanner only rejects the literal `eval(`/`new Function(`/`Function(`
// call spellings; whitespace variants (`eval (`), member/bracket access
// (`globalThis["eval"](`, `(0,eval)(`), timer-string eval
// (`setTimeout("…")`), dynamic `import(`, and `__proto__`/`constructor`
// keys all pass — the emit layer never produces them (JsAst escapes
// string literals and has no dynamic-import node), so they are noted
// here rather than "fixed" with patterns that would false-positive on
// legitimate string data (see `scanner_eval_substring_in_string_data`).
// ---------------------------------------------------------------------------

#[test]
fn scanner_rejects_eval_and_function_call_patterns() {
    let s = SecurityScanner::new();
    for bad in [
        "eval(",
        "new Function(",
        "Function(",
        // Substring match: `myFunction(` trips `Function(` too.
        "const f = myFunction(x);",
    ] {
        assert!(s.scan(bad).is_err(), "scanner accepted `{bad}`");
    }
}

#[test]
fn scanner_documented_gaps_pass_currently() {
    let s = SecurityScanner::new();
    // Whitespace between callee and paren — valid JS eval spelling the
    // substring check misses.
    assert!(s.scan("eval (x)").is_ok());
    // Indirect/member eval spellings.
    assert!(s.scan(r#"globalThis["eval"](x)"#).is_ok());
    assert!(s.scan("(0,eval)(x)").is_ok());
    // Timer-string and dynamic-import eval channels.
    assert!(s.scan(r#"setTimeout("alert(1)", 0)"#).is_ok());
    assert!(s.scan("import(x)").is_ok());
    // Prototype-pollution spellings are not scanned at all.
    assert!(s.scan("o.__proto__ = x").is_ok());
    assert!(s.scan("o.constructor.prototype.x = 1").is_ok());
}

#[test]
fn scanner_eval_substring_in_string_data_is_a_false_positive() {
    // `"eval("` inside a *string literal* is data, not a call — the
    // scanner cannot tell and rejects the whole emit. This is the
    // documented cost of the substring pass; `JsConfig::security_scan
    // = false` is the escape hatch for such sources.
    let s = SecurityScanner::new();
    assert!(s.scan(r#"const s = "eval(";"#).is_err());
}
