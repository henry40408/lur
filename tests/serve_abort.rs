//! `lur.serve.abort`, `req.valid_query`, `req.valid_params`.

use lur::runtime::RuntimeConfig;
use lur::serve::{RawRequest, Response, Server};

fn serve(source: &str) -> Server {
    Server::load(
        source,
        RuntimeConfig {
            pool_size: 1,
            ..Default::default()
        },
    )
    .expect("app loads")
}

fn get(s: &Server, path: &str, query: &str) -> Response {
    s.dispatch_raw(&RawRequest {
        method: "GET".into(),
        path: path.into(),
        query: query.into(),
        ..Default::default()
    })
    .expect("dispatch ok")
}

fn get_err(s: &Server, path: &str, query: &str) -> String {
    s.dispatch_raw(&RawRequest {
        method: "GET".into(),
        path: path.into(),
        query: query.into(),
        ..Default::default()
    })
    .expect_err("handler fails")
    .to_string()
}

fn body(r: &Response) -> String {
    String::from_utf8(r.body.clone()).unwrap()
}

fn content_type(r: &Response) -> Option<String> {
    r.headers
        .iter()
        .find(|(n, _)| n == "content-type")
        .map(|(_, v)| v.to_str().unwrap().to_owned())
}

#[test]
fn abort_responds_with_status_and_json_body() {
    let s = serve(
        "lur.serve.http('GET', '/a', function(req)\n\
         \tlur.serve.abort(403, 'nope', { reason = 'banned' })\n\
         end)",
    );
    let r = get(&s, "/a", "");
    assert_eq!(r.status, 403);
    assert_eq!(content_type(&r).as_deref(), Some("application/json"));
    assert_eq!(body(&r), r#"{"data":{"reason":"banned"},"error":"nope"}"#);
}

#[test]
fn abort_without_data_omits_the_key() {
    let s = serve("lur.serve.http('GET', '/a', function() lur.serve.abort(404, 'gone') end)");
    let r = get(&s, "/a", "");
    assert_eq!(r.status, 404);
    assert_eq!(body(&r), r#"{"error":"gone"}"#);
}

#[test]
fn abort_is_catchable_with_pcall() {
    let s = serve(
        "lur.serve.http('GET', '/a', function()\n\
         \tlocal ok, e = pcall(lur.serve.abort, 409, 'conflict', { n = 1 })\n\
         \treturn { body = tostring(ok) .. ' ' .. e.kind .. ' ' .. e.status .. ' ' .. e.message\n\
         \t\t.. ' ' .. e.data.n .. ' ' .. tostring(e) }\n\
         end)",
    );
    let r = get(&s, "/a", "");
    assert_eq!(r.status, 200);
    assert_eq!(body(&r), "false abort 409 conflict 1 conflict");
}

#[test]
fn abort_rejects_bad_arguments() {
    let s = serve(
        "lur.serve.http('GET', '/a', function(req)\n\
         \tlocal out = {}\n\
         \tfor _, args in ipairs({ {200, 'x'}, {399, 'x'}, {600, 'x'}, {404.5, 'x'}, {'404', 'x'}, {404}, {404, {}} }) do\n\
         \t\tlocal ok, e = pcall(lur.serve.abort, table.unpack(args))\n\
         \t\ttable.insert(out, tostring(ok) .. ':' .. tostring(e):match('^[^\\n]*') .. '|' .. type(e))\n\
         \tend\n\
         \treturn { body = table.concat(out, '\\n') }\n\
         end)",
    );
    let r = get(&s, "/a", "");
    let text = body(&r);
    assert_eq!(text.lines().count(), 7, "{text}");
    for line in text.lines() {
        assert!(line.starts_with("false:"), "{line}");
        assert!(line.contains("lur.serve.abort"), "{line}");
    }
    assert!(text.contains("400..=599"), "{text}");
}

#[test]
fn abort_with_non_json_data_is_an_ordinary_error() {
    let s = serve("lur.serve.http('GET', '/a', function() lur.serve.abort(400, 'x', print) end)");
    let e = get_err(&s, "/a", "");
    assert!(e.contains("not JSON-encodable"), "{e}");
}

#[test]
fn caught_abort_then_normal_return_is_a_normal_response() {
    let s = serve(
        "lur.serve.http('GET', '/a', function()\n\
         \tpcall(lur.serve.abort, 400, 'swallowed')\n\
         \treturn { status = 201, body = 'fine' }\n\
         end)",
    );
    let r = get(&s, "/a", "");
    assert_eq!(r.status, 201);
    assert_eq!(body(&r), "fine");
}

#[test]
fn caught_abort_does_not_turn_a_later_failure_into_a_4xx() {
    let s = serve(
        "lur.serve.http('GET', '/a', function()\n\
         \tpcall(lur.serve.abort, 400, 'swallowed')\n\
         \terror('real failure')\n\
         end)",
    );
    let e = get_err(&s, "/a", "");
    assert!(e.contains("real failure"), "{e}");
}

#[test]
fn abort_does_not_leak_across_requests_on_one_vm() {
    // pool-size 1: every request reuses the same VM and abort slot.
    let s = serve(
        "lur.serve.http('GET', '/swallow', function()\n\
         \tpcall(lur.serve.abort, 418, 'teapot')\n\
         \treturn { body = 'ok' }\n\
         end)\n\
         lur.serve.http('GET', '/boom', function() error('teapot') end)\n\
         lur.serve.http('GET', '/abort', function() lur.serve.abort(401, 'auth') end)\n\
         lur.serve.http('GET', '/ok', function() return { body = 'ok' } end)",
    );
    assert_eq!(get(&s, "/swallow", "").status, 200);
    // Same message as the swallowed abort, but a different run: still a 500.
    let e = get_err(&s, "/boom", "");
    assert!(e.contains("teapot"), "{e}");
    assert_eq!(get(&s, "/abort", "").status, 401);
    assert_eq!(get(&s, "/ok", "").status, 200);
    assert_eq!(get(&s, "/abort", "").status, 401);
}

#[test]
fn abort_inside_async_task_ends_as_an_error_not_a_table() {
    let s = serve(
        "lur.serve.http('GET', '/a', function()\n\
         \tlocal ok, e = pcall(lur.async.all, { function() lur.serve.abort(400, 'in task') end })\n\
         \treturn { body = type(e) .. ':' .. tostring(e) }\n\
         end)\n\
         lur.serve.http('GET', '/raw', function()\n\
         \tlur.async.all({ function() lur.serve.abort(400, 'in task') end })\n\
         end)",
    );
    // Known limitation: the combinator's caller does not get the table.
    let r = get(&s, "/a", "");
    assert!(body(&r).contains("in task"), "{}", body(&r));
    assert!(!body(&r).starts_with("table:"), "{}", body(&r));
    // Uncaught, the stash still identifies it by message, so it stays a 400.
    let r = get(&s, "/raw", "");
    assert_eq!(r.status, 400);
    assert_eq!(body(&r), r#"{"error":"in task"}"#);
}

#[test]
fn abort_in_one_shot_is_an_error() {
    let rt = lur::runtime::Runtime::new().expect("runtime builds");
    let e = rt
        .run("lur.serve.abort(400, 'x')")
        .expect_err("no request to abort");
    assert!(e.to_string().contains("only available under"), "{e}");
}

#[test]
fn abort_in_cron_is_an_ordinary_error() {
    let dir = tempfile::tempdir().unwrap();
    let s = Server::load(
        "lur.serve.cron('* * * * * *', function()\n\
         \tlocal ok, e = pcall(lur.serve.abort, 404, 'x')\n\
         \tlur.kv.set('r', tostring(ok) .. ':' .. tostring(e))\n\
         end, { name = 'tick' })\n\
         lur.serve.http('GET', '/r', function() return { body = lur.kv.get('r') } end)",
        RuntimeConfig {
            db_path: Some(dir.path().join("c.db")),
            pool_size: 1,
            ..Default::default()
        },
    )
    .expect("app loads");
    s.fire_cron("tick").unwrap();
    let r = get(&s, "/r", "");
    let text = body(&r);
    assert!(text.starts_with("false:"), "{text}");
    assert!(text.contains("no request to abort"), "{text}");
}

#[test]
fn valid_query_converts_clamps_and_drops_unknown_keys() {
    let s = serve(
        "local v = lur.validate\n\
         local schema = v.object({\n\
         \tlimit = v.int({ min = 1, max = 100, default = 20 }),\n\
         \ttags = v.list(v.str()),\n\
         \tq = v.str(),\n\
         })\n\
         lur.serve.http('GET', '/s', function(req)\n\
         \tlocal got = req.valid_query(schema)\n\
         \tlocal extra = 0\n\
         \tfor _ in pairs(got) do extra = extra + 1 end\n\
         \treturn { body = got.q .. '|' .. got.limit .. '|' .. table.concat(got.tags, ',') .. '|' .. extra }\n\
         end)",
    );
    let r = get(&s, "/s", "q=hi&tags=a&tags=b&junk=1");
    assert_eq!(r.status, 200, "{}", body(&r));
    assert_eq!(body(&r), "hi|20|a,b|3");
    // Non-list fields take the last value.
    let r = get(&s, "/s", "q=a&q=b&limit=7&tags=x");
    assert_eq!(body(&r), "b|7|x|3");
}

#[test]
fn valid_query_failure_is_a_400_json_body() {
    let s = serve(
        "local v = lur.validate\n\
         lur.serve.http('GET', '/s', function(req)\n\
         \tlocal got = req.valid_query(v.object({\n\
         \t\tlimit = v.int({ max = 100 }),\n\
         \t\tq = v.str(),\n\
         \t}))\n\
         \treturn { body = 'unreachable' }\n\
         end)",
    );
    let r = get(&s, "/s", "limit=101");
    assert_eq!(r.status, 400);
    assert_eq!(content_type(&r).as_deref(), Some("application/json"));
    let json: serde_json::Value = serde_json::from_slice(&r.body).unwrap();
    assert_eq!(json["error"], "validation failed");
    let issues = json["data"].as_array().unwrap();
    assert!(
        issues
            .iter()
            .any(|i| i["path"] == "limit" && i["message"] == "must be <= 100"),
        "{json}"
    );
    assert!(
        issues
            .iter()
            .any(|i| i["path"] == "q" && i["message"] == "is required"),
        "{json}"
    );
}

#[test]
fn valid_params_uses_fail_status() {
    let s = serve(
        "local v = lur.validate\n\
         lur.serve.http('GET', '/u/:id', function(req)\n\
         \tlocal p = req.valid_params(v.object({ id = v.int({ min = 1 }) }), { fail_status = 404 })\n\
         \treturn { body = tostring(p.id) }\n\
         end)",
    );
    assert_eq!(body(&get(&s, "/u/42", "")), "42");
    let r = get(&s, "/u/abc", "");
    assert_eq!(r.status, 404);
    assert!(
        body(&r).contains(r#""error":"validation failed""#),
        "{}",
        body(&r)
    );
}

#[test]
fn valid_fail_status_is_checked_at_call_time() {
    let s = serve(
        "local v = lur.validate\n\
         lur.serve.http('GET', '/s', function(req)\n\
         \tlocal out = {}\n\
         \tfor _, st in ipairs({ 200, 700, 'x', 404.5 }) do\n\
         \t\tlocal ok, e = pcall(req.valid_query, v.object({}), { fail_status = st })\n\
         \t\ttable.insert(out, tostring(ok) .. ':' .. tostring(e):match('^[^\\n]*'))\n\
         \tend\n\
         \treturn { body = table.concat(out, '\\n') }\n\
         end)",
    );
    let text = body(&get(&s, "/s", ""));
    assert_eq!(text.lines().count(), 4, "{text}");
    for line in text.lines() {
        assert!(line.starts_with("false:"), "{line}");
        assert!(line.contains("opts.fail_status"), "{line}");
    }
}

#[test]
fn valid_query_error_is_catchable_and_carries_issues() {
    let s = serve(
        "local v = lur.validate\n\
         lur.serve.http('GET', '/s', function(req)\n\
         \tlocal ok, e = pcall(req.valid_query, v.object({ n = v.int() }), { fail_status = 422 })\n\
         \treturn { body = e.kind .. ' ' .. e.status .. ' ' .. e.message .. ' ' .. e.data[1].path }\n\
         end)",
    );
    assert_eq!(body(&get(&s, "/s", "n=x")), "abort 422 validation failed n");
}

#[test]
fn valid_real_errors_propagate_unchanged() {
    let s = serve(
        "local v = lur.validate\n\
         lur.serve.http('GET', '/fn', function(req)\n\
         \treq.valid_query(v.object({ n = function(x) error('validator blew up') end }))\n\
         end)\n\
         lur.serve.http('GET', '/bad', function(req)\n\
         \treq.valid_query(42)\n\
         end)",
    );
    // By lur.validate's contract an erroring validator function is an issue.
    let r = get(&s, "/fn", "n=1");
    assert_eq!(r.status, 400);
    assert!(body(&r).contains("validator blew up"), "{}", body(&r));
    let e = get_err(&s, "/bad", "");
    assert!(e.contains("must be a schema or function"), "{e}");
}

#[test]
fn valid_query_with_non_object_schema_gets_req_query() {
    let s = serve(
        "lur.serve.http('GET', '/s', function(req)\n\
         \tlocal q = req.valid_query(function(t) return t.a end)\n\
         \treturn { body = tostring(q) }\n\
         end)",
    );
    assert_eq!(body(&get(&s, "/s", "a=1&a=2")), "2");
}

#[test]
fn valid_timeout_in_validator_is_not_a_4xx() {
    let s = Server::load(
        "lur.serve.http('GET', '/slow', function(req)\n\
         \treq.valid_query(lur.validate.object({ n = function(x) while true do end end }))\n\
         end)",
        RuntimeConfig {
            per_event_timeout: Some(std::time::Duration::from_millis(100)),
            pool_size: 1,
            ..Default::default()
        },
    )
    .expect("app loads");
    assert_eq!(get(&s, "/slow", "n=1").status, 503);
}
