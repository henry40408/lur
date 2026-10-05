//! `lur.xml`: parse XML and walk it with slash paths.

use lur::runtime::Runtime;

/// Run a script that asserts on its own; any Lua error fails the test.
fn run(src: &str) {
    Runtime::new()
        .expect("runtime builds")
        .run(src)
        .expect("script ran without error");
}

/// Run a script that must fail; returns the error text.
fn fails(src: &str) -> String {
    Runtime::new()
        .expect("runtime builds")
        .run(src)
        .expect_err("script should have raised")
        .to_string()
}

const RSS: &str = r#"
local doc = lur.xml.parse([==[<?xml version="1.0"?>
<rss version="2.0" xmlns:dc="http://purl.org/dc/elements/1.1/">
  <channel>
    <title>Blog</title>
    <item><title>One</title><link>https://e.com/1</link><dc:creator>Ann</dc:creator></item>
    <item><title><![CDATA[Two & <more>]]></title><link>https://e.com/2</link></item>
  </channel>
</rss>]==])
"#;

#[test]
fn walks_an_rss_feed() {
    run(&format!(
        r#"{RSS}
        local items = doc:select("rss/channel/item")
        assert(#items == 2)
        assert(items[1]:select_one("title"):text() == "One")
        assert(items[1]:select_one("dc:creator"):text() == "Ann")
        assert(items[2]:select_one("title"):text() == "Two & <more>", 'cdata')
        assert(items[2]:select_one("dc:creator") == nil)
        assert(doc:select_one("rss"):attr("version") == "2.0")
        assert(doc:select_one("rss"):attrs()["xmlns:dc"] == "http://purl.org/dc/elements/1.1/")
        assert(items[1]:tag() == "item")
        assert(doc:tag() == nil)
        assert(items[1]:parent():tag() == "channel")
        assert(doc:select_one("rss"):parent() == nil)
        assert(#doc:select_one("rss/channel"):children() == 3)
        "#
    ));
}

#[test]
fn path_axes_and_wildcards() {
    run(&format!(
        r#"{RSS}
        assert(#doc:select("//item") == 2)
        assert(#doc:select("//title") == 3, 'descendant, document order')
        assert(doc:select("//title")[1]:text() == "Blog")
        assert(#doc:select("rss//title") == 3)
        assert(#doc:select("rss/*") == 1)
        assert(#doc:select("rss/channel/*") == 3)
        assert(#doc:select("//item/*") == 5)
        assert(#doc:select("nope") == 0)
        local channel = doc:select_one("//channel")
        assert(#channel:select("item") == 2, 'relative to a node')
        assert(#channel:select("//title") == 3)
        assert(doc:select_one("//link"):text() == "https://e.com/1")
        "#
    ));
}

#[test]
fn descendant_results_are_deduplicated() {
    run(r"
        local doc = lur.xml.parse('<a><a><b/></a></a>')
        assert(#doc:select('//a//b') == 1)
    ");
}

#[test]
fn text_joins_mixed_content_and_atom_attributes() {
    run(r#"
        local doc = lur.xml.parse([[<feed xmlns="http://www.w3.org/2005/Atom">
          <entry><link rel="alternate" href="https://e.com/a?x=1&amp;y=2"/><p>a<b>b</b>c</p></entry>
        </feed>]])
        local link = doc:select_one("feed/entry/link")
        assert(link:attr("href") == "https://e.com/a?x=1&y=2")
        assert(link:attr("missing") == nil)
        assert(link:text() == "")
        assert(doc:select_one("//p"):text() == "abc")
    "#);
}

#[test]
fn entities() {
    run(r#"
        local doc = lur.xml.parse([[<a t="&quot;q&quot; &#65;&#x42; &nbsp;">&lt;&gt;&amp;&apos;&quot; &#x4F60;&#22909; &nbsp;</a>]])
        local a = doc:select_one("a")
        assert(a:text() == "<>&'\" 你好 &nbsp;", 'unknown entity kept literally: ' .. a:text())
        assert(a:attr("t") == "\"q\" AB &nbsp;", a:attr("t"))
    "#);
}

#[test]
fn declared_encoding_is_honored() {
    run(r#"
        local body = '<?xml version="1.0" encoding="Big5"?><a>\xA7\x41\xA6\x6E</a>'
        assert(lur.xml.parse(body):select_one("a"):text() == "你好")
        local utf8 = '<?xml version="1.0" encoding="UTF-8"?><a>你好</a>'
        assert(lur.xml.parse(utf8):select_one("a"):text() == "你好")
        local bad = lur.xml.parse('<a>\xFF</a>')
        assert(bad:select_one("a"):text() == "\u{FFFD}", 'invalid UTF-8 is replaced')
    "#);
}

#[test]
fn doctype_comments_and_pis_are_ignored() {
    run(r#"
        local doc = lur.xml.parse([==[<?xml version="1.0"?>
        <!DOCTYPE a [<!ENTITY x "boom">]>
        <!-- hi --><?pi data?><a>t<!-- c -->u&x;</a>]==])
        assert(doc:select_one("a"):text() == "tu&x;", 'custom entities are not expanded')
    "#);
}

#[test]
fn malformed_xml_raises_with_a_position() {
    let e = fails("lur.xml.parse('<a>\\n<b></a>')");
    assert!(e.contains("lur.xml.parse"), "{e}");
    assert!(e.contains("line 2"), "{e}");
    assert!(fails("lur.xml.parse('<a><b>')").contains("unclosed element <b>"));
    assert!(fails("lur.xml.parse('')").contains("no root element"));
    assert!(fails("lur.xml.parse('just text')").contains("no root element"));
    assert!(fails("lur.xml.parse('<a/><b/>')").contains("more than one root"));
    assert!(
        fails("lur.xml.parse('<?xml version=\"1.0\" encoding=\"nope\"?><a/>')").contains("nope")
    );
}

#[test]
fn invalid_paths_raise() {
    for path in ["", "/a", "a/", "a//", "a///b", "a/ b", "//"] {
        let e = fails(&format!("lur.xml.parse('<a/>'):select({path:?})"));
        assert!(e.contains("lur.xml.select: invalid path"), "{path:?}: {e}");
    }
}

#[test]
fn argument_types_are_checked() {
    assert!(fails("lur.xml.parse({})").contains("lur.xml.parse"));
    assert!(fails("lur.xml.parse('<a/>'):select({})").contains("lur.xml.select"));
    assert!(fails("lur.xml.parse('<a/>'):select_one({})").contains("lur.xml.select_one"));
    assert!(fails("lur.xml.parse('<a/>'):attr(nil)").contains("lur.xml.attr"));
}

#[test]
fn deep_nesting_does_not_overflow() {
    run(r"
        local doc = lur.xml.parse(('<a>'):rep(20000) .. 'x' .. ('</a>'):rep(20000))
        assert(#doc:select('//a') == 20000)
        assert(doc:select_one('a'):text() == 'x')
    ");
}
