//! `lur.html.sanitize`: untrusted HTML in, safe HTML out.

use lur::runtime::Runtime;

/// Run a script that asserts on its own; any Lua error fails the test.
fn run(src: &str) {
    Runtime::new()
        .expect("runtime builds")
        .run(src)
        .expect("script ran without error");
}

fn fails(src: &str) -> String {
    Runtime::new()
        .expect("runtime builds")
        .run(src)
        .expect_err("script should have raised")
        .to_string()
}

#[test]
fn removes_scripts_handlers_and_styles() {
    run(r#"
        local out = lur.html.sanitize([[<p onclick="x()">hi<script>alert(1)</script><style>p{}</style><b>b</b></p>]])
        assert(out == "<p>hi<b>b</b></p>", out)
        local img = lur.html.sanitize([[<img src="a.png" onerror="x()" alt="A">]])
        assert(not img:find("onerror") and img:find('src="a.png"', 1, true), img)
    "#);
}

#[test]
fn unsafe_url_schemes_are_dropped() {
    run(r#"
        for _, bad in ipairs({ "javascript:alert(1)", "JaVaScRiPt:alert(1)", "java\tscript:alert(1)", "data:text/html,x", "vbscript:x" }) do
            local out = lur.html.sanitize('<a href="' .. bad .. '">x</a>')
            assert(not out:find("href", 1, true), bad .. " -> " .. out)
        end
        local ok = lur.html.sanitize([[<a href="https://e.com/a?x=1&y=2">x</a>]])
        assert(ok:find('href="https://e.com/a?x=1&amp;y=2"', 1, true), ok)
        assert(ok:find('rel="noopener noreferrer"', 1, true), ok)
        assert(lur.html.sanitize([[<a href="mailto:a@e.com">m</a>]]):find("mailto:a@e.com", 1, true))
        assert(not lur.html.sanitize([[<a href="ftp://e.com/x">f</a>]]):find("href", 1, true))
    "#);
}

#[test]
fn relative_urls_are_resolved_against_base() {
    run(r#"
        local html = [[<a href="/p/1">a</a><img src="../i.png"><a href="//cdn.e.com/x">c</a>]]
        local out = lur.html.sanitize(html, { base = "https://e.com/blog/post/" })
        assert(out:find('href="https://e.com/p/1"', 1, true), out)
        assert(out:find('src="https://e.com/blog/i.png"', 1, true), out)
        assert(out:find('href="https://cdn.e.com/x"', 1, true), out)
        local plain = lur.html.sanitize(html)
        assert(plain:find('href="/p/1"', 1, true), plain)
    "#);
}

#[test]
fn broken_markup_is_repaired_and_text_escaped() {
    run(r#"
        local out = lur.html.sanitize("<p>1 < 2 & <b>bold")
        assert(out == "<p>1 &lt; 2 &amp; <b>bold</b></p>", out)
        assert(lur.html.sanitize("") == "")
    "#);
}

#[test]
fn argument_errors() {
    assert!(fails("lur.html.sanitize({})").contains("lur.html.sanitize"));
    assert!(fails("lur.html.sanitize('x', { base = 'not a url' })").contains("invalid base"));
}
