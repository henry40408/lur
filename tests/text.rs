//! `lur.text.replacer`: Go `strings.NewReplacer` semantics, `longest` mode,
//! `opts.key` sharing and the memory cap.

use std::sync::Arc;

use lur::capabilities::text::ReplacerStore;
use lur::runtime::{Runtime, RuntimeConfig};

fn run(src: &str) {
    Runtime::new()
        .expect("runtime builds")
        .run(src)
        .expect("script ran without error");
}

#[test]
fn first_match_wins_in_argument_order() {
    run(r#"
        local R = lur.text.replacer
        assert(R({ { "a", "1" }, { "aa", "2" } }):replace("aaa") == "111")
        assert(R({ { "aa", "2" }, { "a", "1" } }):replace("aaa") == "21")
        assert(R({ { "a", "1" }, { "a", "2" } }):replace("aa") == "11", "first duplicate wins")
        assert(R({ { "a", "b" }, { "b", "a" } }):replace("abba") == "baab", "no rescan")
        assert(R({ { "x", "y" } }):replace("") == "")
        assert(R({}):replace("abc") == "abc")
    "#);
}

#[test]
fn empty_old_inserts_between_bytes() {
    run(r#"
        local R = lur.text.replacer
        assert(R({ { "", "-" } }):replace("ab") == "-a-b-")
        assert(R({ { "", "-" } }):replace("") == "-")
    "#);
}

#[test]
fn longest_mode_prefers_the_longest_old() {
    run(r#"
        local pairs_ = { { "a", "1" }, { "ab", "2" }, { "abc", "3" } }
        assert(lur.text.replacer(pairs_):replace("abc") == "1bc")
        assert(lur.text.replacer(pairs_, { match = "longest" }):replace("abc") == "3")
        assert(lur.text.replacer(pairs_, { match = "longest" }):replace("abx ab") == "2x 2")
        -- an empty old matches only where nothing longer does
        local r = lur.text.replacer({ { "", "-" }, { "ab", "X" } }, { match = "longest" })
        assert(r:replace("abc") == "X-c-")
    "#);
}

#[test]
fn bytes_pass_through_and_numbers_coerce() {
    run(r#"
        local r = lur.text.replacer({ { "a", "b" }, { 1, 2 } })
        assert(r:replace("\xFFa\xFE1") == "\xFFb\xFE2")
        assert(r:replace(11) == "22")
    "#);
}

#[test]
fn traditional_conversion_example() {
    run(r#"
        local r = lur.text.replacer({ { "头发", "頭髮" }, { "发", "發" }, { "头", "頭" } },
            { match = "longest" })
        assert(r:replace("头发发") == "頭髮發")
    "#);
}

#[test]
fn bad_arguments_raise() {
    run(r#"
        local R = lur.text.replacer
        local function bad(pat, ...)
            local ok, err = pcall(R, ...)
            assert(not ok, "expected an error")
            assert(tostring(err):find(pat, 1, true), tostring(err))
        end
        bad("argument #1 must be table", "x")
        bad("pairs[1] must be {old, new}", { "a" })
        bad("pairs[2] must be {old, new}", { { "a", "b" }, { "c" } })
        bad("pairs[1] old must be string", { { {}, "b" } })
        bad("pairs[1] new must be string", { { "a", true } })
        bad("opts.match", {}, { match = "shortest" })
        bad("opts.key", {}, { key = "" })
        bad("unknown option", {}, { matches = "first" })
        bad("argument #2 must be table", {}, "first")
        local ok, err = pcall(function() return R({}):replace({}) end)
        assert(not ok and tostring(err):find("argument #1 must be string", 1, true), tostring(err))
    "#);
}

#[test]
fn the_text_table_is_frozen() {
    run(r"
        assert(table.isfrozen(lur.text))
        assert(not pcall(function() lur.text.replacer = 1 end))
    ");
}

#[test]
fn a_key_shares_one_matcher_and_checks_the_shape() {
    run(r#"
        local a = lur.text.replacer({ { "a", "1" } }, { key = "k" })
        -- later pairs are not used: the first definition wins
        local b = lur.text.replacer({ { "z", "9" } }, { key = "k" })
        assert(b:replace("az") == "1z")
        local ok, err = pcall(lur.text.replacer, { { "a", "1" }, { "b", "2" } }, { key = "k" })
        assert(not ok and tostring(err):find("already used", 1, true), tostring(err))
        local ok2 = pcall(lur.text.replacer, { { "a", "1" } }, { key = "k", match = "longest" })
        assert(not ok2, "match mismatch raises")
        assert(a:replace("a") == "1")
    "#);
}

#[test]
fn the_memory_cap_raises_and_release_frees_room() {
    let config = RuntimeConfig {
        replacers: Arc::new(ReplacerStore::with_limit(4096)),
        ..RuntimeConfig::default()
    };
    Runtime::with_config(config)
        .expect("runtime builds")
        .run(
            r#"
            local function big(n)
                local t = {}
                for i = 1, n do t[i] = { "k" .. i, "v" .. i } end
                return t
            end
            local ok, err = pcall(lur.text.replacer, big(5000))
            assert(not ok and tostring(err):find("--replacer-max-bytes", 1, true), tostring(err))
            -- small ones fit, and dropping them (GC) returns the budget
            for _ = 1, 50 do
                local r = lur.text.replacer(big(10))
                assert(r:replace("k1") == "v1")
            end
        "#,
        )
        .expect("script ran without error");
}

#[test]
fn replace_output_is_bounded_by_the_vm_memory_cap() {
    let config = RuntimeConfig {
        memory_limit: 32 * 1024 * 1024,
        ..RuntimeConfig::default()
    };
    Runtime::with_config(config)
        .expect("runtime builds")
        .run(
            r#"
            local r = lur.text.replacer({ { "", string.rep("x", 1048576) } })
            local ok, err = pcall(r.replace, r, string.rep("a", 1048576))
            assert(not ok and tostring(err):find("memory limit", 1, true), tostring(err))
        "#,
        )
        .expect("script ran without error");
}

#[test]
fn an_oversized_dictionary_is_refused_while_building() {
    let config = RuntimeConfig {
        replacers: Arc::new(ReplacerStore::with_limit(64 * 1024)),
        ..RuntimeConfig::default()
    };
    Runtime::with_config(config)
        .expect("runtime builds")
        .run(
            r#"
            local pairs_ = {}
            for i = 1, 200 do pairs_[i] = { i .. string.rep("A", 5000), "x" } end
            local ok, err = pcall(lur.text.replacer, pairs_)
            assert(not ok and tostring(err):find("--replacer-max-bytes", 1, true), tostring(err))
        "#,
        )
        .expect("script ran without error");
}

#[test]
fn first_mode_does_not_walk_a_long_unmatched_branch() {
    // {a}, {a*L b}: every position would otherwise walk the whole `a` run.
    run(r#"
        local L = 100000
        local r = lur.text.replacer({ { "a", "x" }, { string.rep("a", L) .. "b", "y" } })
        local t = os.clock()
        assert(r:replace(string.rep("a", L)) == string.rep("x", L))
        assert(os.clock() - t < 2, "quadratic walk")
    "#);
}
