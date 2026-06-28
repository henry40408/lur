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
