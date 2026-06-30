use lur::runtime::{Runtime, RuntimeConfig};

/// End to end: the rendered one-shot output carries a snippet and the file line.
#[test]
fn rendered_runtime_error_has_snippet() {
    let cfg = RuntimeConfig {
        chunk_name: Some("app.lua".to_owned()),
        ..Default::default()
    };
    let rt = Runtime::with_config(cfg).expect("runtime builds");
    let err = rt
        .run("local x = nil\nprint(x.y)\n")
        .expect_err("script raises");
    let out = lur::diagnostics::render(
        "local x = nil\nprint(x.y)\n",
        "app.lua",
        &err.to_string(),
        false,
    );
    assert!(out.contains("--> app.lua:2"), "{out}");
    assert!(out.contains("2 | print(x.y)"), "{out}");
}

/// A runtime error reports the configured chunk name, not lur's Rust source.
#[test]
fn named_runtime_reports_script_path_not_internals() {
    let cfg = RuntimeConfig {
        chunk_name: Some("app.lua".to_owned()),
        ..Default::default()
    };
    let rt = Runtime::with_config(cfg).expect("runtime builds");
    let err = rt
        .run("local x = nil\nprint(x.y)\n")
        .expect_err("script raises");
    let msg = err.to_string();
    assert!(msg.contains("app.lua:2"), "names the script line: {msg}");
    assert!(!msg.contains("src/runtime.rs"), "no internal path: {msg}");
}

/// A nameless runtime falls back to "script", never the Rust call site.
#[test]
fn nameless_runtime_falls_back_to_script() {
    let rt = Runtime::new().expect("runtime builds");
    let err = rt.run("error('boom')\n").expect_err("script raises");
    let msg = err.to_string();
    assert!(msg.contains("script:1"), "uses the generic name: {msg}");
    assert!(!msg.contains("src/runtime.rs"), "no internal path: {msg}");
}
