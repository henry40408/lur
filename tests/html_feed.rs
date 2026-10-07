use lur::runtime::Runtime;

/// Run a script that asserts on its own; any Lua error fails the test.
fn run(src: &str) {
    Runtime::new()
        .expect("runtime builds")
        .run(src)
        .expect("script ran without error");
}

#[test]
fn html_select_text_attr() {
    run(r#"
local doc = lur.html.parse([[<ul><li class="a"><a href="/x">One</a></li><li><a href="/y">Two &amp; Co</a></li></ul>]])
local links = doc:select("li a")
assert(#links == 2, "two links")
assert(links[1]:text() == "One")
assert(links[2]:text() == "Two & Co", "entities decoded")
assert(links[1]:attr("href") == "/x")
assert(links[1]:attr("nope") == nil)
assert(links[1]:tag() == "a")
assert(doc:select_one("li.a"):attr("class") == "a")
assert(doc:select_one("table") == nil)
assert(#doc:select("table") == 0)
"#);
}

#[test]
fn html_nested_select_and_navigation() {
    run(r#"
local doc = lur.html.parse("<table><tr><td>1</td><td>2</td></tr><tr><td>3</td><td>4</td></tr></table>")
local rows = doc:select("tr")
assert(#rows == 2)
assert(rows[2]:select("td")[1]:text() == "3", "scoped to the row")
assert(#rows[1]:children() == 2)
assert(rows[1]:children()[2]:parent():tag() == "tr")
assert(rows[1]:select_one("td"):html() == "<td>1</td>")
assert(rows[1]:inner_html() == "<td>1</td><td>2</td>")
local attrs = lur.html.parse('<p id="i" data-x="y">z</p>'):select_one("p"):attrs()
assert(attrs.id == "i" and attrs["data-x"] == "y")
"#);
}

#[test]
fn html_survives_cache_eviction() {
    // More live documents than cache slots: nodes must re-parse transparently.
    run(r#"
local nodes = {}
for i = 1, 20 do
  nodes[i] = lur.html.parse("<div><b>" .. i .. "</b></div>"):select_one("b")
end
for i = 1, 20 do
  assert(nodes[i]:text() == tostring(i), "node " .. i)
end
"#);
}

#[test]
fn html_rejects_bad_input() {
    run(r#"
local ok, err = pcall(function() return lur.html.parse("<p>x</p>"):select("p[") end)
assert(not ok and tostring(err):find("invalid selector"), tostring(err))
local ok2, err2 = pcall(lur.html.parse, {})
assert(not ok2 and tostring(err2):find("argument #1 must be string"), tostring(err2))
-- invalid UTF-8 is replaced, not rejected
assert(lur.html.parse("<p>\255</p>"):select_one("p"):text() == "\u{FFFD}")
"#);
}

#[test]
fn feed_rss_basic() {
    run(r#"
local xml = lur.feed.rss(
  { title = "T & U", link = "https://e.com", feed_url = "https://e.com/f.xml", language = "zh-TW" },
  { { title = "<One>", link = "https://e.com/1", date = 0, author = "me", categories = { "a", "b" },
      content = "<p>hi</p>", summary = "sum",
      enclosure = { url = "https://e.com/a.mp3", type = "audio/mpeg", length = 5 } } }
)
local function has(s) assert(xml:find(s, 1, true), "missing: " .. s .. "\n" .. xml) end
has('<?xml version="1.0" encoding="UTF-8"?>')
has("<title>T &amp; U</title>")
has("<title>&lt;One&gt;</title>")
has('<guid isPermaLink="true">https://e.com/1</guid>')
has("<pubDate>Thu, 1 Jan 1970 00:00:00 +0000</pubDate>")
has("<category>b</category>")
has("<description>sum</description>")
has("<content:encoded>&lt;p&gt;hi&lt;/p&gt;</content:encoded>")
has('<enclosure url="https://e.com/a.mp3" length="5" type="audio/mpeg"/>')
has('<atom:link href="https://e.com/f.xml" rel="self"')
"#);
}

#[test]
fn feed_atom_basic() {
    run(r#"
local xml = lur.feed.atom(
  { title = "T", link = "https://e.com" },
  { { title = "One", guid = "tag:e,1", date = 1700000000000, content = "<p>x</p>" } }
)
local function has(s) assert(xml:find(s, 1, true), "missing: " .. s .. "\n" .. xml) end
has('<feed xmlns="http://www.w3.org/2005/Atom">')
has("<id>https://e.com</id>")
has("<id>tag:e,1</id>")
has("<updated>2023-11-14T22:13:20Z</updated>")
has('<content type="html">&lt;p&gt;x&lt;/p&gt;</content>')
"#);
}

#[test]
fn feed_json_round_trips() {
    run(r#"
local s = lur.feed.json(
  { title = "T", link = "https://e.com" },
  { { title = "One", link = "https://e.com/1", date = 0, content = "<p>x</p>", author = "me", categories = { "t" } } }
)
local f = lur.json.decode(s)
assert(f.version == "https://jsonfeed.org/version/1.1")
assert(f.home_page_url == "https://e.com")
local it = f.items[1]
assert(it.id == "https://e.com/1" and it.url == it.id)
assert(it.date_published == "1970-01-01T00:00:00Z")
assert(it.content_html == "<p>x</p>" and it.authors[1].name == "me" and it.tags[1] == "t")
"#);
}

#[test]
fn feed_strips_illegal_xml_chars() {
    run(r#"
local xml = lur.feed.rss({ title = "T", link = "l" }, { { title = "a\1b\11c", link = "l" } })
assert(xml:find("<title>abc</title>", 1, true), xml)
"#);
}

#[test]
fn feed_validates_input() {
    run(r#"
local function fails(f, needle)
  local ok, err = pcall(f)
  assert(not ok and tostring(err):find(needle, 1, true), tostring(err))
end
fails(function() lur.feed.rss({ link = "l" }, {}) end, "meta.title is required")
fails(function() lur.feed.rss({ title = "t" }, {}) end, "meta.link is required")
fails(function() lur.feed.atom({ title = "t" }, {}) end, "meta.id or meta.link")
fails(function() lur.feed.rss({ title = "t", link = "l" }, { { link = "x" } }) end, "items[1].title is required")
fails(function() lur.feed.rss({ title = "t", link = "l" }, { { title = 1.5 } }) end, "")
fails(function() lur.feed.rss({ title = "t", link = "l" }, { { title = "x", date = 1.5 } }) end, "items[1].date")
fails(function() lur.feed.atom({ title = "t", link = "l" }, { { title = "x" } }) end, "needs guid or link")
fails(function() lur.feed.json({ title = "t" }, { { title = "x", categories = { 1 } } }) end, "categories")
"#);
}
