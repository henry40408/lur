use lur::runtime::Runtime;

/// Run a script that asserts on its own; any Lua error fails the test.
fn run(src: &str) {
    Runtime::new()
        .expect("runtime builds")
        .run(src)
        .expect("script ran without error");
}

#[test]
fn url_parse_fields() {
    run(r#"
local u = lur.url.parse("https://user:pw@example.com:8443/a/b?x=1&y=2#frag")
assert(u.scheme == "https" and u.host == "example.com" and u.port == 8443)
assert(u.username == "user" and u.password == "pw")
assert(u.path == "/a/b" and u.query == "x=1&y=2" and u.fragment == "frag")
local d = lur.url.parse("https://example.com:443/")
assert(d.port == nil, "default port is not explicit")
assert(d.query == nil and d.fragment == nil and d.username == nil)
local m = lur.url.parse("mailto:a@b.c")
assert(m.host == nil and m.scheme == "mailto")
"#);
}

#[test]
fn url_parse_rejects_garbage() {
    run(r#"
local ok, err = pcall(lur.url.parse, "not a url")
assert(not ok and tostring(err):find("lur.url.parse"), tostring(err))
local ok2, err2 = pcall(lur.url.parse, {})
assert(not ok2 and tostring(err2):find("argument #1 must be string"), tostring(err2))
"#);
}

#[test]
fn url_join_resolves_relative_links() {
    run(r#"
local base = "https://e.com/blog/post/1?x=1"
assert(lur.url.join(base, "2") == "https://e.com/blog/post/2")
assert(lur.url.join(base, "/about") == "https://e.com/about")
assert(lur.url.join(base, "../tags") == "https://e.com/blog/tags")
assert(lur.url.join(base, "//cdn.e.com/a.png") == "https://cdn.e.com/a.png")
assert(lur.url.join(base, "?page=2") == "https://e.com/blog/post/1?page=2")
assert(lur.url.join(base, "https://other.org/") == "https://other.org/")
assert(lur.url.join("https://e.com/", "文章") == "https://e.com/%E6%96%87%E7%AB%A0")
local ok = pcall(lur.url.join, "relative/base", "x")
assert(not ok, "base must be absolute")
"#);
}

#[test]
fn url_encode_and_decode_query() {
    run(r#"
local q = lur.url.encode_query({ b = "x y", a = 1, tags = { "p", "q&r" }, on = true })
assert(q == "a=1&b=x+y&on=true&tags=p&tags=q%26r", q)
assert(lur.url.encode_query({}) == "")
local d = lur.url.decode_query("?a=1&b=x+y&b=z%26&c")
assert(d.a == "1" and d.b == "z&" and d.c == "", "last duplicate wins, + is space")
local round = lur.url.decode_query(lur.url.encode_query({ k = "日本語 & more" }))
assert(round.k == "日本語 & more")
local ok, err = pcall(lur.url.encode_query, { a = {} , b = function() end })
assert(not ok and tostring(err):find("must be string"), tostring(err))
"#);
}

#[test]
fn time_format_with_timezones() {
    run(r#"
local ms = 1700000000000 -- 2023-11-14T22:13:20Z
assert(lur.time.format_rfc3339(ms) == "2023-11-14T22:13:20.000Z")
assert(lur.time.format_rfc3339(ms, "Asia/Taipei") == "2023-11-15T06:13:20.000+08:00")
assert(lur.time.format_rfc3339(ms, "-05:00") == "2023-11-14T17:13:20.000-05:00")
assert(lur.time.format_rfc3339(ms, "America/New_York") == "2023-11-14T17:13:20.000-05:00")
assert(lur.time.format_rfc2822(0) == "Thu, 1 Jan 1970 00:00:00 +0000")
assert(lur.time.format_rfc2822(ms, "+0800") == "Wed, 15 Nov 2023 06:13:20 +0800")
assert(lur.time.format(ms, "%Y/%m/%d %H:%M", "Asia/Taipei") == "2023/11/15 06:13")
assert(lur.time.format(ms, "%Y") == "2023")
"#);
}

#[test]
fn time_format_rejects_bad_input() {
    run(r#"
local function fails(f, needle)
  local ok, err = pcall(f)
  assert(not ok and tostring(err):find(needle, 1, true), tostring(err))
end
fails(function() lur.time.format_rfc3339(0, "Mars/Base") end, "unknown timezone")
fails(function() lur.time.format(0, "%Q") end, "invalid format")
fails(function() lur.time.format_rfc3339("x") end, "lur.time.format_rfc3339")
fails(function() lur.time.format_rfc3339(1.5) end, "integer")
fails(function() lur.time.format_rfc3339(9e18) end, "out of range")
"#);
}

#[test]
fn time_parse_lenient_with_zone() {
    run(r#"
local T = lur.time
-- formats with an explicit offset ignore tz
assert(T.parse("1970-01-01T00:00:01Z") == 1000)
assert(T.parse("Thu, 01 Jan 1970 00:00:01 GMT") == 1000)
assert(T.parse("Thu, 1 Jan 1970 08:00:01 +0800") == 1000)
-- wall-clock text is read in tz (default UTC)
assert(T.parse("1970-01-01 00:00:01") == 1000)
assert(T.parse("1970-01-01 08:00:01", nil, "Asia/Taipei") == 1000)
assert(T.parse("1970-01-01T08:00", nil, "+08:00") == 0)
assert(T.parse("1970/01/02") == 86400000)
assert(T.parse("1970-01-02", nil, "Asia/Taipei") == 86400000 - 8 * 3600 * 1000)
assert(T.parse("  1970-01-01 00:00:01.250  ") == 1250)
assert(T.parse_rfc2822("Thu, 01 Jan 1970 00:00:01 +0000") == 1000)
"#);
}

#[test]
fn time_parse_with_explicit_format() {
    run(r#"
local T = lur.time
assert(T.parse("05/10/2026 14:30", "%d/%m/%Y %H:%M", "Asia/Taipei") == T.parse("2026-10-05T06:30:00Z"))
assert(T.parse("2026年10月05日", "%Y年%m月%d日", "Asia/Taipei") == T.parse("2026-10-04T16:00:00Z"))
assert(T.parse("2026-10-05 14:30 +0800", "%Y-%m-%d %H:%M %z") == T.parse("2026-10-05T06:30:00Z"))
local ok, err = pcall(T.parse, "garbage", "%Y-%m-%d")
assert(not ok and tostring(err):find("does not match"), tostring(err))
local ok2, err2 = pcall(T.parse, "garbage")
assert(not ok2 and tostring(err2):find("unrecognized date"), tostring(err2))
"#);
}

#[test]
fn time_parse_dst_gap_and_overlap() {
    run(r#"
local T = lur.time
-- 2023-03-12 02:30 does not exist in New York (spring forward)
local ok, err = pcall(T.parse, "2023-03-12 02:30", nil, "America/New_York")
assert(not ok and tostring(err):find("does not exist"), tostring(err))
-- 2023-11-05 01:30 happens twice; the earlier (EDT, -04:00) wins
assert(T.parse("2023-11-05 01:30", nil, "America/New_York") == T.parse("2023-11-05T05:30:00Z"))
"#);
}

#[test]
fn time_round_trips_through_feed() {
    run(r#"
local ms = lur.time.parse("2026-10-05 14:30", nil, "Asia/Taipei")
local xml = lur.feed.rss({ title = "t", link = "l" }, { { title = "x", link = "l", date = ms } })
assert(xml:find("<pubDate>Mon, 5 Oct 2026 06:30:00 +0000</pubDate>", 1, true), xml)
"#);
}
