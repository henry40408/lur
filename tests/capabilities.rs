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
fn base64_round_trips_and_rejects_garbage() {
    run("assert(lur.base64.encode('hi') == 'aGk=', 'encode')\n\
         assert(lur.base64.decode('aGk=') == 'hi', 'decode')\n\
         local ok = pcall(function() return lur.base64.decode('!!!') end)\n\
         assert(ok == false, 'invalid base64 must be rejected')");
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
fn crypto_hex_round_trips_and_rejects_bad_input() {
    run("local raw = string.char(0xde, 0xad, 0xbe, 0xef)\n\
         assert(lur.crypto.hex.encode(raw) == 'deadbeef', 'encode is lowercase hex')\n\
         assert(lur.crypto.hex.decode('DEADBEEF') == raw, 'decode accepts uppercase')\n\
         assert(pcall(function() return lur.crypto.hex.decode('abc') end) == false,\n\
         \t'odd length must be rejected')\n\
         assert(pcall(function() return lur.crypto.hex.decode('zz') end) == false,\n\
         \t'non-hex must be rejected')");
}

#[test]
fn crypto_hashes_match_known_vectors() {
    run("local hex = lur.crypto.hex.encode\n\
         assert(hex(lur.crypto.sha256('abc')) ==\n\
         \t'ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad', 'sha256')\n\
         assert(hex(lur.crypto.sha256('')) ==\n\
         \t'e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855', 'sha256 empty')\n\
         assert(hex(lur.crypto.sha512('abc')) ==\n\
         \t'ddaf35a193617abacc417349ae20413112e6fa4e89a97ea20a9eeee64b55d39a' ..\n\
         \t'2192992a274fc1a836ba3c23a3feebbd454d4423643ce80e2a9ac94fa54ca49f', 'sha512')\n\
         assert(hex(lur.crypto.sha1('abc')) ==\n\
         \t'a9993e364706816aba3e25717850c26c9cd0d89d', 'sha1')\n\
         assert(hex(lur.crypto.md5('abc')) ==\n\
         \t'900150983cd24fb0d6963f7d28e17f72', 'md5')");
}

#[test]
fn crypto_hmac_matches_rfc_vectors() {
    run("local hex = lur.crypto.hex.encode\n\
         local key, msg = 'Jefe', 'what do ya want for nothing?'\n\
         assert(hex(lur.crypto.hmac_sha256(key, msg)) ==\n\
         \t'5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843', 'hmac_sha256')\n\
         assert(hex(lur.crypto.hmac_sha1(key, msg)) ==\n\
         \t'effcdf6ae5eb2fa2d27416d5f184df9c259a7c79', 'hmac_sha1')\n\
         assert(#lur.crypto.hmac_sha512(key, msg) == 64, 'hmac_sha512 is 64 bytes')");
}

#[test]
fn crypto_constant_eq_compares_bytes() {
    run(
        "assert(lur.crypto.constant_eq('abc', 'abc') == true, 'equal')\n\
         assert(lur.crypto.constant_eq('abc', 'abd') == false, 'differ same length')\n\
         assert(lur.crypto.constant_eq('ab', 'abc') == false, 'differ length')\n\
         assert(lur.crypto.constant_eq('', '') == true, 'empty equal')",
    );
}

#[test]
fn crypto_random_bytes_length_and_bounds() {
    run("local a = lur.crypto.random_bytes(16)\n\
         assert(#a == 16, 'returns n bytes')\n\
         local b = lur.crypto.random_bytes(16)\n\
         assert(a ~= b, 'two draws differ')\n\
         assert(pcall(function() return lur.crypto.random_bytes(0) end) == false, 'n=0 rejected')\n\
         assert(pcall(function() return lur.crypto.random_bytes(-1) end) == false, 'negative rejected')");
}

#[test]
fn crypto_verifies_a_webhook_signature_end_to_end() {
    run(
        "local secret, body = 'Jefe', 'what do ya want for nothing?'\n\
         local mac = lur.crypto.hmac_sha256(secret, body)\n\
         local got = lur.crypto.hex.encode(mac)\n\
         local want = '5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843'\n\
         assert(lur.crypto.constant_eq(got, want), 'signature must verify')",
    );
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

#[test]
fn cookie_parse_basic_and_multiple() {
    run("local c = lur.cookie.parse('sid=abc; theme=dark')\n\
         assert(c.sid == 'abc', 'sid')\n\
         assert(c.theme == 'dark', 'theme')");
}

#[test]
fn cookie_parse_trims_whitespace_including_tabs() {
    run(
        "local c = lur.cookie.parse('  a=1 ;' .. string.char(9) .. 'b=2' .. string.char(9))\n\
         assert(c.a == '1', 'a trimmed')\n\
         assert(c.b == '2', 'b trimmed')",
    );
}

#[test]
fn cookie_parse_is_lenient_and_keeps_inner_equals() {
    run(
        "assert(next(lur.cookie.parse('')) == nil, 'empty -> empty table')\n\
         local c = lur.cookie.parse('garbage; x=1')\n\
         assert(c.x == '1' and c.garbage == nil, 'segment without = is skipped')\n\
         assert(lur.cookie.parse('=novalue; y=2').y == '2', 'empty name skipped')\n\
         assert(lur.cookie.parse('k=1; k=2').k == '2', 'later duplicate wins')\n\
         assert(lur.cookie.parse('t=a=b').t == 'a=b', 'value keeps inner =')",
    );
}

#[test]
fn cookie_serialize_bare_and_single_attributes() {
    run("local s = lur.cookie.serialize\n\
         assert(s('sid', 'abc') == 'sid=abc', 'bare')\n\
         assert(s('a', 'b', {domain='example.com'}) == 'a=b; Domain=example.com', 'domain')\n\
         assert(s('a', 'b', {path='/'}) == 'a=b; Path=/', 'path')\n\
         assert(s('a', 'b', {max_age=3600}) == 'a=b; Max-Age=3600', 'max_age')\n\
         assert(s('a', 'b', {max_age=0}) == 'a=b; Max-Age=0', 'max_age zero')\n\
         assert(s('a', 'b', {max_age=-1}) == 'a=b; Max-Age=-1', 'max_age negative')\n\
         assert(s('a', 'b', {http_only=true}) == 'a=b; HttpOnly', 'http_only')\n\
         assert(s('a', 'b', {secure=true}) == 'a=b; Secure', 'secure')\n\
         assert(s('a', 'b', {same_site='lax'}) == 'a=b; SameSite=Lax', 'same_site canon')");
}

#[test]
fn cookie_serialize_false_omits_and_order_is_fixed() {
    run("local s = lur.cookie.serialize\n\
         assert(s('a', 'b', {secure=false, http_only=false}) == 'a=b', 'false omits flags')\n\
         local full = s('sid', 'abc', {\n\
           domain='example.com', path='/', max_age=3600,\n\
           expires='Mon, 29 Jun 2026 12:53:14 GMT',\n\
           http_only=true, secure=true, same_site='Strict' })\n\
         assert(full == 'sid=abc; Domain=example.com; Path=/; Max-Age=3600; '\n\
           .. 'Expires=Mon, 29 Jun 2026 12:53:14 GMT; HttpOnly; Secure; SameSite=Strict',\n\
           'fixed attribute order')");
}

#[test]
fn cookie_serialize_same_site_none_requires_secure() {
    run("local s = lur.cookie.serialize\n\
         assert(pcall(function() return s('a','b',{same_site='None'}) end) == false,\n\
           'None without secure raises')\n\
         assert(s('a','b',{same_site='None', secure=true}) == 'a=b; Secure; SameSite=None',\n\
           'None with secure ok')\n\
         assert(pcall(function() return s('a','b',{same_site='bogus'}) end) == false,\n\
           'unknown same_site raises')");
}

#[test]
fn cookie_serialize_rejects_invalid_inputs() {
    run("local s = lur.cookie.serialize\n\
         assert(pcall(function() return s('a b', 'v') end) == false, 'name with space')\n\
         assert(pcall(function() return s('a=b', 'v') end) == false, 'name with =')\n\
         assert(pcall(function() return s('', 'v') end) == false, 'empty name')\n\
         assert(pcall(function() return s('a', 'b;c') end) == false, 'value with ;')\n\
         assert(pcall(function() return s('a', 'b' .. string.char(10) .. 'c') end) == false,\n\
           'value with LF')\n\
         assert(pcall(function() return s('a', 'b', {max_age=1.5}) end) == false,\n\
           'non-integer max_age')\n\
         assert(pcall(function() return s('a', 'b' .. string.char(13) .. 'c') end) == false,\n\
           'value with CR')\n\
         assert(pcall(function() return s('a', 'b', {domain='x' .. string.char(13) .. 'y'}) end) == false,\n\
           'domain with CR')\n\
         assert(pcall(function() return s('a', 'b', {path='x' .. string.char(10) .. 'y'}) end) == false,\n\
           'path with LF')\n\
         assert(pcall(function() return s('a', 'b', {expires='x' .. string.char(13) .. string.char(10) .. 'y'}) end) == false,\n\
           'expires with CRLF')\n\
         assert(pcall(function() return s('a', 'b', {max_age=9223372036854775808}) end) == false,\n\
           'max_age = 2^63 out of range')");
}

#[test]
fn cookie_serialize_allows_high_bytes() {
    run("local s = lur.cookie.serialize\n\
         local result = s('a', string.char(0xe2))\n\
         assert(result == 'a=' .. string.char(0xe2), 'high byte should pass through')");
}
