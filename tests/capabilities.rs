use lur::runtime::Runtime;

/// Run a script that asserts on its own; any Lua error fails the test.
fn run(src: &str) {
    Runtime::new()
        .expect("runtime builds")
        .run(src)
        .expect("script ran without error");
}

#[test]
fn lur_null_is_a_distinct_non_nil_singleton() {
    run("assert(lur.null ~= nil, 'lur.null must not be nil')\n\
         assert(lur.null == lur.null, 'lur.null must equal itself')\n\
         assert(lur.null ~= false, 'lur.null is its own value')");
}

#[test]
fn json_encode_objects_and_arrays() {
    run("assert(lur.json.encode({a=1}) == '{\"a\":1}', 'object')\n\
         assert(lur.json.encode({1, 2, 3}) == '[1,2,3]', 'array')");
}

#[test]
fn json_encode_null_and_scalars() {
    run("assert(lur.json.encode(lur.null) == 'null', 'null')\n\
         assert(lur.json.encode(true) == 'true', 'bool')\n\
         assert(lur.json.encode('hi') == '\"hi\"', 'string')\n\
         assert(lur.json.encode(1.5) == '1.5', 'float')");
}

#[test]
fn json_decode_basic_and_null() {
    run(
        "local t = lur.json.decode('{\"a\":1,\"b\":[2,3],\"c\":null}')\n\
         assert(t.a == 1, 'a')\n\
         assert(t.b[1] == 2 and t.b[2] == 3, 'array')\n\
         assert(t.c == lur.null, 'null maps to lur.null')",
    );
}

#[test]
fn json_round_trips_through_decode_encode() {
    // Keys already in sorted order so the comparison is order-stable.
    run("local s = '{\"x\":[1,2,3],\"y\":\"hi\"}'\n\
         assert(lur.json.encode(lur.json.decode(s)) == s, 'round-trip')");
}

#[test]
fn json_encode_rejects_non_utf8_string() {
    // \255 is invalid UTF-8 — must error at the JSON boundary (§4).
    run(
        "assert(type(lur.json.encode) == 'function', 'encode must exist')\n\
         local ok = pcall(function() return lur.json.encode('\\255') end)\n\
         assert(ok == false, 'non-UTF-8 must be rejected')",
    );
}
