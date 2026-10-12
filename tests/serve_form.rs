//! `req.form()`, `req.form_all()`, `req.valid_body`.

use lur::runtime::RuntimeConfig;
use lur::serve::{RawRequest, Response, Server};

fn serve_with(source: &str, max_body: Option<usize>) -> Server {
    Server::load(
        source,
        RuntimeConfig {
            pool_size: 1,
            max_body,
            ..Default::default()
        },
    )
    .expect("app loads")
}

fn serve(source: &str) -> Server {
    serve_with(source, None)
}

fn post_ct(s: &Server, path: &str, ct: Option<&str>, body: &str) -> Response {
    let headers = ct
        .map(|c| vec![("Content-Type".to_owned(), c.to_owned())])
        .unwrap_or_default();
    s.dispatch_raw(&RawRequest {
        method: "POST".into(),
        path: path.into(),
        headers,
        body: body.as_bytes().to_vec(),
        ..Default::default()
    })
    .expect("dispatch ok")
}

fn form(s: &Server, body: &str) -> Response {
    post_ct(s, "/f", Some("application/x-www-form-urlencoded"), body)
}

fn text(r: &Response) -> String {
    String::from_utf8(r.body.clone()).unwrap()
}

const DUMP: &str = "local function dump(t)\n\
    local keys = {}\n\
    for k in pairs(t) do table.insert(keys, k) end\n\
    table.sort(keys)\n\
    local out = {}\n\
    for _, k in ipairs(keys) do\n\
        local v = t[k]\n\
        if type(v) == 'table' then v = '[' .. table.concat(v, ',') .. ']' end\n\
        table.insert(out, k .. '=' .. v)\n\
    end\n\
    return table.concat(out, ';')\n\
end\n";

fn dump_app() -> Server {
    serve(&format!(
        "{DUMP}\
         lur.serve.http('POST', '/f', function(req) return {{ body = dump(req.form()) }} end)\n\
         lur.serve.http('POST', '/all', function(req) return {{ body = dump(req.form_all()) }} end)"
    ))
}

#[test]
fn form_decodes_plus_percent_and_last_value() {
    let s = dump_app();
    assert_eq!(text(&form(&s, "a=b+c&d=%20x&e=%2B")), "a=b c;d= x;e=+");
    // Repeated keys: last wins.
    assert_eq!(text(&form(&s, "k=1&k=2&k=3")), "k=3");
    // Key without `=` is "", empty segments skipped.
    assert_eq!(text(&form(&s, "flag&&x=1&")), "flag=;x=1");
    // `%` garbage is kept verbatim.
    assert_eq!(text(&form(&s, "a=100%&b=%zz&c=%4")), "a=100%;b=%zz;c=%4");
    // Keys are decoded too.
    assert_eq!(text(&form(&s, "a%20b=1")), "a b=1");
}

#[test]
fn form_all_collects_every_value() {
    let s = dump_app();
    let r = post_ct(
        &s,
        "/all",
        Some("application/x-www-form-urlencoded"),
        "k=1&k=2&j&k=3",
    );
    assert_eq!(text(&r), "j=[];k=[1,2,3]");
}

#[test]
fn form_empty_body_is_empty_table() {
    let s = dump_app();
    assert_eq!(text(&form(&s, "")), "");
}

#[test]
fn form_accepts_charset_and_uppercase_content_type() {
    let s = dump_app();
    for ct in [
        "application/x-www-form-urlencoded; charset=utf-8",
        "APPLICATION/X-WWW-FORM-URLENCODED",
        "Application/X-Www-Form-Urlencoded ;charset=UTF-8",
    ] {
        let r = post_ct(&s, "/f", Some(ct), "a=1");
        assert_eq!(r.status, 200, "{ct}");
        assert_eq!(text(&r), "a=1", "{ct}");
    }
}

#[test]
fn form_invalid_utf8_is_replaced_not_fatal() {
    let s = serve(
        "lur.serve.http('POST', '/f', function(req)\n\
         \tlocal f = req.form()\n\
         \treturn { body = #f.a .. ':' .. tostring(f.b) }\n\
         end)",
    );
    let r = s
        .dispatch_raw(&RawRequest {
            method: "POST".into(),
            path: "/f".into(),
            headers: vec![(
                "content-type".into(),
                "application/x-www-form-urlencoded".into(),
            )],
            body: b"a=\xff\xfe&b=ok".to_vec(),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(r.status, 200);
    // Two invalid bytes become two U+FFFD (3 bytes each).
    assert_eq!(text(&r), "6:ok");
}

#[test]
fn form_wrong_content_type_is_a_script_error() {
    let s = dump_app();
    for (ct, expect) in [
        (Some("application/json"), "application/json"),
        (Some("text/plain"), "text/plain"),
        (None, "missing"),
    ] {
        let e = s
            .dispatch_raw(&RawRequest {
                method: "POST".into(),
                path: "/f".into(),
                headers: ct
                    .map(|c| vec![("content-type".to_owned(), c.to_owned())])
                    .unwrap_or_default(),
                body: b"a=1".to_vec(),
                ..Default::default()
            })
            .expect_err("handler fails")
            .to_string();
        assert!(e.contains("req.form()"), "{e}");
        assert!(e.contains(expect), "{e}");
    }
}

#[test]
fn form_after_read_is_unavailable() {
    let s = serve(
        "lur.serve.http('POST', '/f', function(req)\n\
         \treq.read(1)\n\
         \tlocal ok, e = pcall(req.form)\n\
         \tlocal ok2, e2 = pcall(req.form_all)\n\
         \tlocal ok3, e3 = pcall(req.valid_body, lur.validate.object({}))\n\
         \treturn { body = tostring(ok) .. tostring(ok2) .. tostring(ok3) .. tostring(e) .. tostring(e3) }\n\
         end)",
    );
    let r = form(&s, "a=1&b=2");
    let t = text(&r);
    assert!(t.starts_with("falsefalsefalse"), "{t}");
    assert!(t.contains("unavailable after req.read(n)"), "{t}");
}

#[test]
fn form_does_not_touch_req_body() {
    let s = serve(
        "lur.serve.http('POST', '/f', function(req)\n\
         \tlocal f = req.form()\n\
         \treturn { body = req.body .. '|' .. f.a }\n\
         end)",
    );
    assert_eq!(text(&form(&s, "a=1")), "a=1|1");
}

const VALID: &str = "local v = lur.validate\n\
    local schema = v.object({\n\
        name = v.str(),\n\
        age = v.int({ min = 0 }),\n\
        tags = v.optional(v.list(v.str())),\n\
    })\n\
    lur.serve.http('POST', '/v', function(req)\n\
        local b = req.valid_body(schema)\n\
        return { body = b.name .. '|' .. type(b.age) .. b.age .. '|' .. table.concat(b.tags or {}, ',') }\n\
    end)\n\
    lur.serve.http('POST', '/v404', function(req)\n\
        req.valid_body(schema, { fail_status = 422 })\n\
        return { body = 'unreachable' }\n\
    end)\n\
    lur.serve.http('POST', '/opt', function(req)\n\
        local b = req.valid_body(v.optional(v.object({ a = v.int() })))\n\
        return { body = b and tostring(b.a) or 'nil' }\n\
    end)\n\
    lur.serve.http('POST', '/bad', function(req)\n\
        local ok, e = pcall(req.valid_body, schema, { fail_status = 200 })\n\
        local ok2, e2 = pcall(req.valid_body, schema, 5)\n\
        return { body = tostring(ok) .. tostring(ok2) }\n\
    end)\n";

#[test]
fn valid_body_json_ok_and_structured_content_types() {
    let s = serve(VALID);
    for ct in [
        "application/json",
        "application/json; charset=utf-8",
        "Application/JSON",
        "application/vnd.api+json",
    ] {
        let r = post_ct(
            &s,
            "/v",
            Some(ct),
            r#"{"name":"ann","age":30,"tags":["a","b"],"junk":1}"#,
        );
        assert_eq!(r.status, 200, "{ct}: {}", text(&r));
        assert_eq!(text(&r), "ann|number30|a,b", "{ct}");
    }
}

#[test]
fn valid_body_malformed_json_is_400_regardless_of_fail_status() {
    let s = serve(VALID);
    for path in ["/v", "/v404"] {
        let r = post_ct(&s, path, Some("application/json"), "{not json");
        assert_eq!(r.status, 400, "{path}");
        assert_eq!(text(&r), r#"{"error":"invalid JSON body"}"#);
    }
}

#[test]
fn valid_body_empty_json_is_400_unless_schema_accepts_nil() {
    let s = serve(VALID);
    for body in ["", "  \n"] {
        let r = post_ct(&s, "/v404", Some("application/json"), body);
        assert_eq!(r.status, 400);
        assert_eq!(text(&r), r#"{"error":"invalid JSON body"}"#);
    }
    let r = post_ct(&s, "/opt", Some("application/json"), "");
    assert_eq!(r.status, 200, "{}", text(&r));
    assert_eq!(text(&r), "nil");
    let r = post_ct(&s, "/opt", Some("application/json"), r#"{"a":4}"#);
    assert_eq!(text(&r), "4");
}

#[test]
fn valid_body_form_with_list_and_int_coercion() {
    let s = serve(VALID);
    let r = post_ct(
        &s,
        "/v",
        Some("application/x-www-form-urlencoded; charset=UTF-8"),
        "name=Ann+Lee&age=41&tags=a&tags=b%20c&name=Bob",
    );
    assert_eq!(r.status, 200, "{}", text(&r));
    assert_eq!(text(&r), "Bob|number41|a,b c");
    // Absent optional list.
    let r = post_ct(
        &s,
        "/v",
        Some("application/x-www-form-urlencoded"),
        "name=x&age=1",
    );
    assert_eq!(text(&r), "x|number1|");
}

#[test]
fn valid_body_unsupported_content_types_are_415() {
    let s = serve(VALID);
    for ct in [
        Some("text/plain"),
        None,
        Some("multipart/form-data; boundary=xyz"),
    ] {
        let r = post_ct(&s, "/v404", ct, "name=a");
        assert_eq!(r.status, 415, "{ct:?}");
        let j: serde_json::Value = serde_json::from_slice(&r.body).unwrap();
        assert_eq!(j["error"], "unsupported content type");
    }
}

#[test]
fn valid_body_failure_shape_and_fail_status() {
    let s = serve(VALID);
    let r = post_ct(
        &s,
        "/v404",
        Some("application/json"),
        r#"{"name":"a","age":-1}"#,
    );
    assert_eq!(r.status, 422);
    let j: serde_json::Value = serde_json::from_slice(&r.body).unwrap();
    assert_eq!(j["error"], "validation failed");
    let issues = j["data"].as_array().unwrap();
    assert!(issues.iter().any(|i| i["path"] == "age"), "{j}");

    // Default status.
    let r = post_ct(&s, "/v", Some("application/json"), r#"{"name":"a"}"#);
    assert_eq!(r.status, 400);

    // A form string that is not an int fails validation.
    let r = post_ct(
        &s,
        "/v",
        Some("application/x-www-form-urlencoded"),
        "name=a&age=old",
    );
    assert_eq!(r.status, 400);
}

#[test]
fn valid_body_bad_arguments_are_ordinary_errors() {
    let s = serve(VALID);
    let r = post_ct(&s, "/bad", Some("application/json"), "{}");
    assert_eq!(r.status, 200);
    assert_eq!(text(&r), "falsefalse");
}

#[test]
fn form_and_valid_body_respect_max_body() {
    let s = serve_with(
        "lur.serve.http('POST', '/f', function(req) return { body = 'ran' } end)",
        Some(4),
    );
    let r = post_ct(
        &s,
        "/f",
        Some("application/x-www-form-urlencoded"),
        "a=1&b=2&c=3",
    );
    assert_eq!(r.status, 413);
}

fn post_headers(s: &Server, path: &str, headers: Vec<(&str, &str)>, body: &str) -> Response {
    s.dispatch_raw(&RawRequest {
        method: "POST".into(),
        path: path.into(),
        headers: headers
            .into_iter()
            .map(|(k, v)| (k.to_owned(), v.to_owned()))
            .collect(),
        body: body.as_bytes().to_vec(),
        ..Default::default()
    })
    .expect("dispatch ok")
}

#[test]
fn valid_body_json_null_is_absent_for_optional_and_default() {
    let s = serve(
        "local v = lur.validate\n\
         lur.serve.http('POST', '/n', function(req)\n\
         \tlocal b = req.valid_body(v.object({\n\
         \t\tname = v.optional(v.str()),\n\
         \t\tn = v.int({ default = 5 }),\n\
         \t\tneed = v.str(),\n\
         \t}))\n\
         \treturn { body = tostring(b.name) .. '|' .. b.n .. '|' .. b.need }\n\
         end)\n\
         lur.serve.http('POST', '/root', function(req)\n\
         \tlocal b = req.valid_body(v.optional(v.object({ a = v.int() })))\n\
         \treturn { body = tostring(b) }\n\
         end)\n\
         lur.serve.http('POST', '/rootreq', function(req)\n\
         \treq.valid_body(v.object({ a = v.int() }))\n\
         end)",
    );
    let j = Some("application/json");
    let r = post_ct(&s, "/n", j, r#"{"name":null,"n":null,"need":"x"}"#);
    assert_eq!(r.status, 200, "{}", text(&r));
    assert_eq!(text(&r), "nil|5|x");
    // Non-optional null: "is required".
    let r = post_ct(&s, "/n", j, r#"{"need":null}"#);
    assert_eq!(r.status, 400);
    let body: serde_json::Value = serde_json::from_slice(&r.body).unwrap();
    assert_eq!(body["data"][0]["path"], "need");
    assert_eq!(body["data"][0]["message"], "is required");
    // Top level.
    let r = post_ct(&s, "/root", j, "null");
    assert_eq!(r.status, 200);
    assert_eq!(text(&r), "nil");
    assert_eq!(post_ct(&s, "/rootreq", j, "null").status, 400);
}

#[test]
fn valid_body_only_json_and_plus_json_types_are_json() {
    let s = serve(VALID);
    for ct in ["text/json", "application/x-json", "application/jsonp"] {
        let r = post_ct(&s, "/v", Some(ct), r#"{"name":"a","age":1}"#);
        assert_eq!(r.status, 415, "{ct}");
    }
}

#[test]
fn missing_content_type_415_has_no_data() {
    let s = serve(VALID);
    let r = post_ct(&s, "/v", None, "x");
    assert_eq!(r.status, 415);
    assert_eq!(text(&r), r#"{"error":"unsupported content type"}"#);
}

#[test]
fn duplicate_content_type_header_last_one_wins() {
    let s = serve(VALID);
    let r = post_headers(
        &s,
        "/v",
        vec![
            ("Content-Type", "text/plain"),
            ("content-type", "application/json"),
        ],
        r#"{"name":"a","age":1}"#,
    );
    assert_eq!(r.status, 200, "{}", text(&r));
    let r = post_headers(
        &s,
        "/v",
        vec![
            ("Content-Type", "application/json"),
            ("content-type", "text/plain"),
        ],
        r#"{"name":"a","age":1}"#,
    );
    assert_eq!(r.status, 415);
}

#[test]
fn form_semicolon_is_not_a_separator_and_percent_00_is_a_nul_byte() {
    let s = serve(
        "lur.serve.http('POST', '/f', function(req)\n\
         \tlocal f = req.form()\n\
         \tlocal keys = 0\n\
         \tfor _ in pairs(f) do keys = keys + 1 end\n\
         \treturn { body = keys .. '|' .. f.a .. '|' .. #f.z .. '|' .. f.z:byte(2) }\n\
         end)",
    );
    let r = form(&s, "a=1;b=2&z=x%00y");
    assert_eq!(r.status, 200, "{}", text(&r));
    assert_eq!(text(&r), "2|1;b=2|3|0");
}
