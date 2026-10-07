//! `lur.charset`: legacy-encoding decode/encode.

use lur::runtime::Runtime;

/// Run a script that asserts on its own; any Lua error fails the test.
fn run(src: &str) {
    Runtime::new()
        .expect("runtime builds")
        .run(src)
        .expect("script ran without error");
}

#[test]
fn decodes_by_label() {
    run(r"
        assert(lur.charset.decode('\xA7\x41\xA6\x6E', 'big5') == '你好', 'big5')
        assert(lur.charset.decode('\xC4\xE3\xBA\xC3', 'GBK') == '你好', 'label is case-insensitive')
        assert(lur.charset.decode('\x93\xFA\x96\x7B\x8C\xEA', 'shift_jis') == '日本語', 'sjis')
    ");
}

#[test]
fn a_content_type_value_works_as_the_hint() {
    run(r#"
        local big5 = '\xA7\x41\xA6\x6E'
        assert(lur.charset.decode(big5, 'text/html; charset=Big5') == '你好')
        assert(lur.charset.decode(big5, 'text/html; charset="big5"; x=1') == '你好', 'quoted')
    "#);
}

#[test]
fn sniffs_a_meta_declaration_when_there_is_no_hint() {
    run(r#"
        local body = '\xA7\x41\xA6\x6E'
        local short = '<meta charset="big5">' .. body
        assert(lur.charset.decode(short):find('你好', 1, true), 'meta charset')
        local equiv = '<meta http-equiv="Content-Type" content="text/html; charset=BIG5">' .. body
        assert(lur.charset.decode(equiv):find('你好', 1, true), 'http-equiv')
        -- a Content-Type without a charset falls back to the document
        assert(lur.charset.decode(short, 'text/html'):find('你好', 1, true), 'media type only')
    "#);
}

#[test]
fn an_explicit_hint_beats_the_meta_tag_and_a_bom_beats_both() {
    run(r#"
        local gbk = '<meta charset="big5">\xC4\xE3\xBA\xC3'
        assert(lur.charset.decode(gbk, 'gbk'):find('你好', 1, true), 'hint wins over meta')
        assert(lur.charset.decode('\xEF\xBB\xBFhi', 'big5') == 'hi', 'BOM wins, and is stripped')
    "#);
}

#[test]
fn defaults_to_utf8_and_replaces_invalid_bytes() {
    run(r"
        assert(lur.charset.decode('héllo') == 'héllo', 'utf-8 passes through')
        assert(lur.charset.decode('a\xFFb') == 'a\u{FFFD}b', 'invalid byte replaced')
        assert(lur.charset.decode('') == '')
    ");
}

#[test]
fn utf16_is_decoded_but_a_meta_cannot_declare_it() {
    run(r#"
        assert(lur.charset.decode('h\0i\0', 'utf-16le') == 'hi')
        assert(lur.charset.decode('<meta charset="utf-16">é') == '<meta charset="utf-16">é')
    "#);
}

#[test]
fn encode_round_trips_and_reports_unmappable_characters() {
    run(r"
        assert(lur.charset.encode('你好', 'big5') == '\xA7\x41\xA6\x6E', 'big5')
        assert(lur.charset.decode(lur.charset.encode('日本語', 'shift_jis'), 'shift_jis') == '日本語')
        assert(lur.charset.encode('a😀b', 'big5') == 'a&#128512;b', 'unmappable becomes a reference')
    ");
}

#[test]
fn bad_input_raises() {
    run(r"
        local function msg(f, ...) local ok, e = pcall(f, ...); assert(not ok); return tostring(e) end
        assert(msg(lur.charset.decode, 'x', 'no-such-charset'):find('unknown charset', 1, true))
        assert(msg(lur.charset.decode, 'x', 'text/html; charset=nope'):find('unknown charset', 1, true))
        assert(msg(lur.charset.encode, 'x', 'no-such-charset'):find('unknown charset', 1, true))
        assert(msg(lur.charset.encode, 'x', 'utf-16le'):find('cannot encode', 1, true))
        assert(msg(lur.charset.encode, '\xFF', 'big5'):find('not valid UTF-8', 1, true))
        assert(msg(lur.charset.decode, {}):find('must be string', 1, true))
    ");
}

#[test]
fn feeds_straight_into_lur_html() {
    run(r#"
        local page = '<meta charset="big5"><p>\xA7\x41\xA6\x6E</p>'
        local doc = lur.html.parse(lur.charset.decode(page))
        assert(doc:select('p')[1]:text() == '你好')
    "#);
}
