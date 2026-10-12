//! `lur.validate`: schema constructors, coercion rules, error value shape.

use lur::runtime::Runtime;

/// Run a script that asserts on its own; any Lua error fails the test.
fn run(src: &str) {
    Runtime::new()
        .expect("runtime builds")
        .run(src)
        .expect("script ran without error");
}

/// Shared Lua prelude: `fails(schema, value)` returns the issues list.
const PRELUDE: &str = r#"
local v = lur.validate
local function fails(schema, value)
  local ok, err = v.try(schema, value)
  assert(not ok, "expected failure")
  return err.issues
end
local function msg(schema, value)
  local issues = fails(schema, value)
  return issues[1].path .. "|" .. issues[1].message
end
local function good(schema, value)
  local ok, out = v.try(schema, value)
  assert(ok, "expected success: " .. tostring(out))
  return out
end
"#;

fn run_with_prelude(src: &str) {
    run(&format!("{PRELUDE}\n{src}"));
}

#[test]
fn str_basics() {
    run_with_prelude(
        r#"
local s = v.str({ min_len = 2, max_len = 4 })
assert(good(s, "abc") == "abc")
assert(msg(s, "a") == "|must be at least 2 characters")
assert(msg(s, "abcde") == "|must be at most 4 characters")
assert(msg(s, 5) == "|must be a string", "no number coercion")
assert(msg(s, nil) == "|is required")
assert(good(v.str(), "") == "", "empty string is a valid string")
assert(msg(v.str({ min_len = 1 }), "") == "|must be at least 1 characters")
assert(good(v.str({ default = "x" }), nil) == "x")
assert(good(v.str({ default = "x" }), "") == "", "str does not treat '' as missing")
-- characters, not bytes
assert(good(v.str({ max_len = 2 }), "日本") == "日本")
"#,
    );
}

#[test]
fn str_pattern_uses_lua_patterns() {
    run_with_prelude(
        r#"
local s = v.str({ pattern = "^%d+$" })
assert(good(s, "123") == "123")
assert(msg(s, "12a") == "|has an invalid format")
-- unanchored like string.find
assert(good(v.str({ pattern = "%d" }), "a1b") == "a1b")
local ok, err = pcall(v.str, { pattern = "[a-" })
assert(not ok and tostring(err):find("lur.validate.str: opts.pattern", 1, true), tostring(err))
"#,
    );
}

#[test]
fn int_coercion_edges() {
    run_with_prelude(
        r#"
local i = v.int()
assert(good(i, 5) == 5)
assert(good(i, "5") == 5)
assert(good(i, "-12") == -12)
assert(good(i, "007") == 7)
assert(good(i, 3.0) == 3)
for _, bad in ipairs({ "1.5", "1e3", "0x10", "+1", " 1", "1 ", "abc", "-", "--1" }) do
  assert(not v.try(i, bad), "should reject " .. bad)
end
assert(msg(i, 1.5) == "|must be an integer")
assert(msg(i, 0 / 0) == "|must be an integer")
assert(msg(i, math.huge):find("must be an integer"), "inf")
assert(msg(i, true) == "|must be an integer")
assert(msg(i, nil) == "|is required")
assert(msg(i, "") == "|is required", "empty string is missing")
-- 2^53 boundary
assert(good(i, 2 ^ 53) == 2 ^ 53)
assert(not v.try(i, 2 ^ 53 + 2))
assert(not v.try(i, "9007199254740993"))
assert(good(i, "9007199254740992") == 2 ^ 53)
assert(not v.try(i, "99999999999999999999999"))
"#,
    );
}

#[test]
fn int_range_default_clamp() {
    run_with_prelude(
        r#"
local lim = v.int({ min = 1, max = 100, default = 20 })
assert(good(lim, nil) == 20)
assert(good(lim, "") == 20, "?limit= falls back to default")
assert(good(lim, "50") == 50)
assert(msg(lim, 101) == "|must be <= 100")
assert(msg(lim, "0") == "|must be >= 1")
local c = v.int({ min = 1, max = 10, clamp = true })
assert(good(c, 99) == 10)
assert(good(c, "-5") == 1)
assert(good(c, 5) == 5)
assert(msg(c, "x"):find("must be an integer"))
"#,
    );
}

#[test]
fn number_rules() {
    run_with_prelude(
        r#"
local n = v.number({ min = 0, max = 1 })
assert(good(n, 0.5) == 0.5)
assert(good(n, "0.25") == 0.25)
assert(good(n, ".5") == 0.5)
assert(good(n, "1e0") == 1)
assert(msg(n, 2) == "|must be <= 1")
assert(msg(n, -0.1) == "|must be >= 0")
for _, bad in ipairs({ "inf", "nan", "0x1", "+1", " 1", "1e999", "" }) do
  assert(not v.try(n, bad), "should reject '" .. bad .. "'")
end
assert(not v.try(v.number(), 0 / 0))
assert(not v.try(v.number(), math.huge))
assert(not v.try(v.number(), -math.huge))
assert(good(v.number({ default = 1.5 }), "") == 1.5)
assert(good(v.number({ min = 0, max = 1, clamp = true }), "7.5") == 1)
"#,
    );
}

#[test]
fn bool_rules() {
    run_with_prelude(
        r#"
local b = v.bool()
assert(good(b, true) == true)
assert(good(b, false) == false)
assert(good(b, "true") == true)
assert(good(b, "false") == false)
for _, bad in ipairs({ "TRUE", "1", "yes", 1, 0, {} }) do
  assert(not v.try(b, bad), "should reject " .. tostring(bad))
end
assert(msg(b, nil) == "|is required")
assert(msg(b, "") == "|is required")
assert(good(v.bool({ default = true }), nil) == true)
assert(good(v.bool({ default = false }), "") == false)
assert(good(v.bool({ default = true }), false) == false, "default only fills missing")
"#,
    );
}

#[test]
fn one_of_is_strict() {
    run_with_prelude(
        r#"
local s = v.one_of({ "asc", "desc" })
assert(good(s, "asc") == "asc")
assert(msg(s, "ASC"):find('must be one of: "asc", "desc"', 1, true))
assert(not v.try(s, 1))
local mixed = v.one_of({ 1, "1", true })
assert(good(mixed, 1) == 1)
assert(good(mixed, "1") == "1")
assert(good(mixed, true) == true)
assert(not v.try(mixed, "true"))
assert(not v.try(mixed, 2))
assert(msg(mixed, nil) == "|is required")
assert(not pcall(v.one_of, {}))
assert(not pcall(v.one_of, { {} }))
"#,
    );
}

#[test]
fn list_validates_each_item_with_paths() {
    run_with_prelude(
        r#"
local l = v.list(v.int({ min = 0 }), { min_items = 1, max_items = 4 })
local out = good(l, { 1, "2", 3 })
assert(#out == 3 and out[2] == 2)
local issues = fails(l, { 1, "x", -1 })
assert(#issues == 2, "all issues collected")
assert(issues[1].path == "[2]" and issues[2].path == "[3]")
assert(msg(l, {}) == "|must have at least 1 items")
assert(msg(l, { 1, 2, 3, 4, 5 }) == "|must have at most 4 items")
assert(msg(l, "abc") == "|must be a list")
assert(msg(l, { a = 1 }) == "|must be a list")
assert(msg(l, { 1, nil, 3 }) == "|must be a list")
assert(msg(l, nil) == "|is required")
-- result is a new table
local input = { 1, 2 }
local res = good(l, input)
assert(res ~= input)
"#,
    );
}

#[test]
fn object_paths_defaults_and_unknown_keys() {
    run_with_prelude(
        r#"
local o = v.object({
  name = v.str({ min_len = 1 }),
  age = v.optional(v.int({ min = 0 })),
  tags = v.list(v.str()),
  addr = v.object({ zip = v.str({ pattern = "^%d%d%d%d%d$" }) }),
})
local input = { name = "a", tags = { "x" }, addr = { zip = "12345", extra = 1 }, junk = true }
local out = good(o, input)
assert(out ~= input and out.addr ~= input.addr)
assert(out.name == "a" and out.age == nil and out.tags[1] == "x")
assert(out.junk == nil, "unknown keys dropped")
assert(out.addr.extra == nil)
assert(input.junk == true, "input untouched")

local ok, err = v.try(o, { name = "", tags = { "a", 2 }, addr = { zip = "x" } })
assert(not ok)
local paths = {}
for _, i in ipairs(err.issues) do paths[#paths + 1] = i.path end
assert(table.concat(paths, ",") == "addr.zip,name,tags[2]", table.concat(paths, ","))

assert(msg(o, nil) == "|is required")
assert(msg(o, 5) == "|must be an object")
local ok2, err2 = v.try(v.object({ a = v.object({ b = v.list(v.object({ c = v.int() })) }) }),
  { a = { b = { {}, { c = 1 }, { c = "z" } } } })
assert(not ok2)
assert(err2.issues[1].path == "a.b[1].c" and err2.issues[2].path == "a.b[3].c")
"#,
    );
}

#[test]
fn object_strict_rejects_unknown_keys() {
    run_with_prelude(
        r#"
local o = v.object({ a = v.int() }, { strict = true })
assert(good(o, { a = 1 }).a == 1)
local issues = fails(o, { a = 1, b = 2, c = 3 })
assert(#issues == 2)
assert(issues[1].path == "b" or issues[1].path == "c")
assert(issues[1].message == "is not a known key")
local nested = v.object({ x = o })
assert(fails(nested, { x = { a = 1, zz = 1 } })[1].path == "x.zz")
-- odd keys are escaped, not interpreted
local odd = fails(v.object({}, { strict = true }), { ["a.b c"] = 1 })
assert(odd[1].path == '["a.b c"]', odd[1].path)
"#,
    );
}

#[test]
fn optional_and_missing_semantics() {
    run_with_prelude(
        r#"
local o = v.object({ n = v.optional(v.int()), s = v.optional(v.str()), d = v.str({ default = "dflt" }) })
local out = good(o, { n = "" })
assert(out.n == nil, "'' is missing for int")
assert(out.s == nil)
assert(out.d == "dflt")
local out2 = good(o, { s = "", n = "4" })
assert(out2.s == "" and out2.n == 4, "'' is a real string")
assert(good(v.optional(v.int()), nil) == nil)
assert(not v.try(v.optional(v.int()), "x"))
"#,
    );
}

#[test]
fn function_validators() {
    run_with_prelude(
        r#"
local even = function(x)
  if type(x) ~= "number" or x % 2 ~= 0 then error("must be even") end
  return x * 10
end
assert(good(even, 4) == 40)
local issues = fails(even, 3)
assert(issues[1].message == "must be even", issues[1].message)
assert(fails(function() error("with level 0", 0) end, 1)[1].message == "with level 0")
assert(fails(function() error({}) end, 1)[1].message ~= "")
-- inside composites, with a path
local o = v.object({ n = even, l = v.list(even) })
local ok, err = v.try(o, { n = 1, l = { 2, 3 } })
assert(not ok)
assert(err.issues[1].path == "l[2]" and err.issues[2].path == "n")
assert(good(o, { n = 2, l = { 2 } }).n == 20)
-- functions see nil for a missing field
local req = v.object({ x = function(val) assert(val ~= nil, "x missing") return val end })
assert(fails(req, {})[1].message:find("x missing"))
-- a validator may call lur.* (async-capable)
local slept = function(x) lur.async.sleep(1) return x end
assert(good(slept, 7) == 7)
-- optional(function) skips nil
assert(good(v.optional(function() error("never") end), nil) == nil)
"#,
    );
}

#[test]
fn parse_and_try_agree() {
    run_with_prelude(
        r#"
local schema = v.object({ a = v.int({ max = 3 }), b = v.str() })
local ok, e1 = v.try(schema, { a = 9 })
assert(not ok)
local pok, e2 = pcall(v.parse, schema, { a = 9 })
assert(not pok)
assert(e1.message == e2.message)
assert(#e1.issues == #e2.issues and e1.issues[1].path == e2.issues[1].path)
assert(e2.kind == "validation")
local out = v.parse(schema, { a = "2", b = "x" })
assert(out.a == 2 and out.b == "x")
local ok3, out3 = v.try(schema, { a = 2, b = "x" })
assert(ok3 and out3.a == 2)
"#,
    );
}

#[test]
fn error_value_shape() {
    run_with_prelude(
        r#"
local ok, err = pcall(v.parse, v.object({ limit = v.int({ max = 100 }) }), { limit = 500 })
assert(not ok)
assert(type(err) == "table")
assert(err.kind == "validation")
assert(err.message == "validation failed: limit: must be <= 100", err.message)
assert(tostring(err) == err.message)
assert(#err.issues == 1)
assert(err.issues[1].path == "limit" and err.issues[1].message == "must be <= 100")
-- root issues have an empty path and no prefix in the message
local _, root = pcall(v.parse, v.int(), "x")
assert(root.issues[1].path == "")
assert(root.message:find("^validation failed: must be an integer"))
-- json-encodable
local enc = lur.json.encode(err)
assert(enc:find('"kind":"validation"', 1, true))
-- issues beyond the cap are cut off but flagged
local many = {}
for i = 1, 500 do many[i] = "x" end
local _, big = v.try(v.list(v.int()), many)
assert(#big.issues == 100 and big.truncated == true)
"#,
    );
}

#[test]
fn error_is_a_table_under_pcall_but_a_string_across_async_tasks() {
    run_with_prelude(
        r#"
local schema = v.int()
local function bad() return v.parse(schema, "x") end
-- nested pcall keeps the table
local ok, err = pcall(function() return pcall(bad) end)
assert(ok)
local ok2, e2 = pcall(bad)
assert(not ok2 and type(e2) == "table" and e2.kind == "validation")
-- a pcall inside the task keeps it too
local inside = lur.async.all({ function()
  local ok, e = pcall(bad)
  return type(e) == "table" and e.kind
end })
assert(inside[1] == "validation")

-- documented limitation: raised inside a lur.async task, the error reaches
-- the combinator's caller as its message string, not a table
local ok3, e3 = pcall(lur.async.all, { bad, function() return 1 end })
assert(not ok3 and type(e3) ~= "table", type(e3))
assert(tostring(e3):find('validation failed: must be an integer (got "x")', 1, true), tostring(e3))
local ok5, e5 = pcall(lur.async.map, { "1", "x" }, function(s) return v.parse(schema, s) end)
assert(not ok5 and type(e5) ~= "table" and tostring(e5):find("validation failed", 1, true))
-- try() has no such problem: it returns the table
local results = lur.async.all({ function() return select(2, v.try(schema, "x")) end })
assert(type(results[1]) == "table" and results[1].kind == "validation")

-- success path unchanged
local r = lur.async.all({ function() return 1 end, function() return 2 end })
assert(r[1] == 1 and r[2] == 2)
local m = lur.async.map({ "1", "2" }, function(s) return v.parse(schema, s) end)
assert(m[1] == 1 and m[2] == 2)
"#,
    );
}

#[test]
fn error_metatable_is_locked() {
    run_with_prelude(
        r#"
local _, err = v.try(v.int(), "x")
assert(getmetatable(err) == false, "metatable is hidden")
assert(not pcall(setmetatable, err, {}))
local function steal() return getmetatable(err).__tostring end
assert(not pcall(function() getmetatable(err).__tostring = function() return "pwned" end end))
-- every later error still renders normally
local _, err2 = v.try(v.int(), "y")
assert(tostring(err2):find("^validation failed"), tostring(err2))
-- the error table itself is an ordinary table
err.extra = 1
assert(err.extra == 1)
"#,
    );
}

#[test]
fn optional_with_default_still_fills() {
    run_with_prelude(
        r#"
local o = v.object({
  limit = v.optional(v.int({ default = 20 })),
  name = v.optional(v.str({ default = "anon" })),
  flag = v.optional(v.bool({ default = true })),
  plain = v.optional(v.int()),
})
local out = good(o, {})
assert(out.limit == 20 and out.name == "anon" and out.flag == true and out.plain == nil)
local out2 = good(o, { limit = "", flag = "" })
assert(out2.limit == 20 and out2.flag == true)
assert(good(o, { limit = "7" }).limit == 7)
"#,
    );
}

#[test]
fn function_validator_table_without_tostring_gets_fixed_message() {
    run_with_prelude(
        r#"
local m = fails(function() error({ code = 1 }) end, 1)[1].message
assert(m == "validation function failed", m)
assert(not m:find("table: 0x", 1, true))
"#,
    );
}

#[test]
fn depth_limit_and_cyclic_input() {
    run_with_prelude(
        r#"
-- schema nesting beyond the limit is an argument error
local s = v.int()
for _ = 1, 63 do s = v.list(s) end
assert(v.try(s, {}))
local ok, err = pcall(v.list, s)
assert(not ok and tostring(err):find("lur.validate.list: schema is nested deeper than 64", 1, true), tostring(err))

-- cyclic input terminates (traversal is bounded by the schema)
local cyc = { name = "x" }
cyc.self = cyc
local o = v.object({ name = v.str(), self = v.optional(v.object({ name = v.str() }, { strict = true })) })
local ok2, e2 = v.try(o, cyc)
assert(not ok2) -- the strict nested object has an unknown key `self`
assert(e2.issues[1].path == "self.self")
local loose = v.object({ self = v.object({ self = v.object({ name = v.str() }) }) })
assert(v.try(loose, cyc))
local cyclist = {}
cyclist[1] = cyclist
local ok3, e3 = v.try(v.list(v.list(v.int())), cyclist)
assert(not ok3 and e3.issues[1].path == "[1][1]")
"#,
    );
}

#[test]
fn argument_errors_are_script_errors() {
    run_with_prelude(
        r#"
local function raises(fn, ...)
  local ok, err = pcall(fn, ...)
  assert(not ok, "expected a raise")
  return tostring(err)
end
local function has(s, needle) assert(s:find(needle, 1, true), s) end
has(raises(v.int, "x"), "lur.validate.int: argument #1 must be table, got string")
has(raises(v.int, { min = "a" }), "lur.validate.int: opts.min must be an integer within +/-2^53, got string")
has(raises(v.int, { min = 5, max = 3 }), "lur.validate.int: opts.min (5) must not exceed opts.max (3)")
has(raises(v.int, { nope = 1 }), 'lur.validate.int: unknown option "nope"')
has(raises(v.int, { clamp = 1 }), "lur.validate.int: opts.clamp must be boolean, got")
has(raises(v.int, { min = 1.5 }), "opts.min")
has(raises(v.number, { max = 0 / 0 }), "lur.validate.number: opts.max must be a finite number")
has(raises(v.number, { min = 2, max = 1 }), "lur.validate.number: opts.min")
has(raises(v.str, { min_len = -1 }), "lur.validate.str: opts.min_len must be >= 0")
has(raises(v.str, { min_len = 3, max_len = 1 }), "lur.validate.str: opts.min_len (3) must not exceed")
has(raises(v.str, { pattern = 5 }), "lur.validate.str: opts.pattern must be string, got")
has(raises(v.bool, { default = "x" }), "lur.validate.bool: opts.default must be boolean")
has(raises(v.list, 5), "lur.validate.list: argument #1 must be a schema or function, got number")
has(raises(v.list, v.int(), { min_items = 3, max_items = 1 }), "lur.validate.list: opts.min_items")
has(raises(v.object, 5), "lur.validate.object: argument #1 must be table")
has(raises(v.object, { a = 5 }), "lur.validate.object: field 'a' must be a schema or function")
has(raises(v.object, { [1] = v.int() }), "lur.validate.object: field names must be strings")
has(raises(v.object, {}, { strict = "y" }), "lur.validate.object: opts.strict must be boolean")
has(raises(v.optional), "lur.validate.optional: argument #1 must be a schema or function, got nil")
has(raises(v.parse, 5, 1), "lur.validate.parse: argument #1 must be a schema or function, got number")
has(raises(v.try, {}, 1), "lur.validate.try: argument #1 must be a schema or function, got table")
"#,
    );
}

#[test]
fn echoed_input_is_truncated() {
    run_with_prelude(
        r#"
local big = string.rep("a", 10000)
local m = msg(v.int(), big)
assert(#m < 100, #m)
assert(m:find("…", 1, true))
local m2 = msg(v.one_of({ big }), "zzz")
assert(#m2 < 200, #m2)
-- keys echoed in strict mode paths are bounded too
local p = fails(v.object({}, { strict = true }), { [string.rep("k", 5000)] = 1 })[1].path
assert(#p < 100, #p)
"#,
    );
}

#[test]
fn schemas_are_reusable_and_input_untouched() {
    run_with_prelude(
        r#"
local s = v.object({ n = v.int({ default = 1 }), l = v.list(v.str()) })
local a = good(s, { l = { "x" } })
local b = good(s, { n = "5", l = {} })
assert(a.n == 1 and b.n == 5 and a ~= b)
local input = { n = "7", l = { "q" } }
good(s, input)
assert(input.n == "7", "coercion does not mutate input")
-- try returns nil value for an optional root
local ok, out = v.try(v.optional(v.int()), nil)
assert(ok and out == nil)
"#,
    );
}

#[test]
fn null_is_absent_for_fields_and_root() {
    run_with_prelude(
        r#"
local n = lur.null
local s = v.object({
  name = v.optional(v.str()),
  age = v.int({ default = 7 }),
  inner = v.optional(v.object({ a = v.optional(v.int()), b = v.str({ default = "d" }) })),
  must = v.str(),
})
local out = good(s, { name = n, age = n, inner = { a = n, b = n }, must = "x" })
assert(out.name == nil and out.age == 7 and out.inner.a == nil and out.inner.b == "d")
assert(good(s, { inner = n, must = "x" }).inner == nil)
-- non-optional field given null: required, not a type error
assert(msg(s, { must = n }) == "must|is required")
-- root
local ok, o = v.try(v.optional(v.str()), n)
assert(ok and o == nil)
assert(msg(v.str(), n) == "|is required")
assert(good(v.str({ default = "z" }), n) == "z")
-- list items: null is a normal failure, not absence
assert(msg(v.list(v.str()), { "a", n }) == "[2]|must be a string")
assert(msg(v.list(v.optional(v.str())), { n }) == "[1]|must be a string")
-- function validators still see the raw null
local seen
good(v.object({ f = function(x) seen = x return x end }), { f = n })
assert(seen == n)
"#,
    );
}

mod cli {
    use assert_cmd::Command;
    use predicates::prelude::*;

    fn run_script(src: &str) -> assert_cmd::assert::Assert {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("app.lua");
        std::fs::write(&path, src).unwrap();
        let cfg = tempfile::tempdir().unwrap();
        Command::cargo_bin("lur")
            .expect("binary builds")
            .env("XDG_CONFIG_HOME", cfg.path())
            .env("NO_COLOR", "1")
            .arg(&path)
            .assert()
    }

    #[test]
    fn uncaught_validation_error_prints_a_readable_diagnostic() {
        run_script(
            "local q = { limit = '500', tags = { 'a', 7 } }\n\
             lur.validate.parse(lur.validate.object({\n\
               limit = lur.validate.int({ max = 100 }),\n\
               tags = lur.validate.list(lur.validate.str()),\n\
             }), q)\n",
        )
        .code(1)
        .stderr(predicate::str::contains(
            "validation failed: limit: must be <= 100; tags[2]: must be a string",
        ))
        .stderr(predicate::str::contains("table: 0x").not());
    }

    #[test]
    fn uncaught_validation_error_inside_async_task_is_readable() {
        run_script(
            "lur.async.all({ function() return lur.validate.parse(lur.validate.int(), 'x') end })\n",
        )
        .code(1)
        .stderr(predicate::str::contains(
            "validation failed: must be an integer",
        ));
    }
}
