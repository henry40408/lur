//! `coroutine.resume`/`coroutine.wrap` are replaced (to track resume depth
//! for `lur.state.fetch` single-flight); scripts must not be able to tell.
//! Each snippet runs on lur and on a plain sandboxed Luau VM, both as an
//! async chunk named `@script` (lur's top level is a coroutine too), and must
//! produce the same `out`.

use lur::runtime::Runtime;

/// `out` from `snippet` on a plain sandboxed Luau VM.
fn builtin(snippet: &str) -> String {
    let lua = mlua::Lua::new();
    lua.sandbox(true).expect("sandbox");
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");
    rt.block_on(
        lua.load(format!("{snippet}\nreturn out"))
            .set_name("@script")
            .eval_async::<String>(),
    )
    .unwrap_or_else(|e| panic!("builtin failed: {e}"))
}

/// Assert lur computes the same `out` (same line numbers: the check is
/// appended after the snippet).
fn same(snippet: &str) {
    let want = builtin(snippet);
    assert!(!want.contains("]==]"), "{want}");
    Runtime::new()
        .expect("runtime builds")
        .run(&format!(
            "{snippet}\nassert(out == [==[{want}]==], '\\nlur:     ' .. tostring(out) .. '\\nbuiltin: {}')",
            want.replace('\\', "\\\\").replace('\'', "\\'").replace('\n', "\\n")
        ))
        .unwrap_or_else(|e| panic!("{snippet}\n{e}"));
}

/// `pcall(f)` rendered as text.
const SHOW: &str = "local function show(...) local t = table.pack(...) \
     for i = 1, t.n do t[i] = tostring(t[i]) end return table.concat(t, '|') end";

#[test]
fn argument_errors_match() {
    for call in [
        "coroutine.resume()",
        "coroutine.resume(1)",
        "coroutine.resume(nil, 1)",
        "coroutine.wrap()",
        "coroutine.wrap('f')",
    ] {
        same(&format!(
            "{SHOW}\nlocal out = show(pcall(function() return {call} end))"
        ));
    }
}

#[test]
fn resume_results_and_states_match() {
    same(&format!(
        "{SHOW}\n\
         local log = {{}}\n\
         local parent\n\
         local co = coroutine.create(function(a, b)\n\
           table.insert(log, show(coroutine.running() ~= nil, coroutine.isyieldable(), coroutine.status(parent)))\n\
           table.insert(log, show(coroutine.resume(parent)))\n\
           table.insert(log, show(coroutine.resume(coroutine.running())))\n\
           local c, d = coroutine.yield(a + b, 'two')\n\
           return c .. d\n\
         end)\n\
         parent = coroutine.running()\n\
         table.insert(log, show(coroutine.isyieldable(), coroutine.status(co)))\n\
         table.insert(log, show(coroutine.resume(co, 1, 2)))\n\
         table.insert(log, show(coroutine.status(co)))\n\
         table.insert(log, show(coroutine.resume(co, 'x', 'y')))\n\
         table.insert(log, show(coroutine.status(co), coroutine.resume(co)))\n\
         table.insert(log, show(coroutine.resume(coroutine.create(function() end))))\n\
         local out = table.concat(log, '\\n')"
    ));
}

#[test]
fn errors_through_resume_and_wrap_match() {
    same(&format!(
        "{SHOW}\n\
         local log = {{}}\n\
         local t = {{}}\n\
         for _, e in ipairs({{ \"'boom'\", \"'boom', 0\", '42', 't', 'nil' }}) do\n\
           local f = function() error(e == 't' and t or e) end\n\
           local ok, err = coroutine.resume(coroutine.create(function() error(e == 't' and t or (e == 'nil' and nil or e)) end))\n\
           table.insert(log, show(ok, err == t, type(err), type(err) ~= 'table' and err or ''))\n\
           local g = coroutine.wrap(function() error(e == 't' and t or (e == 'nil' and nil or e)) end)\n\
           local ok2, err2 = pcall(function() return g() end)\n\
           table.insert(log, show(ok2, err2 == t, type(err2), type(err2) ~= 'table' and err2 or ''))\n\
         end\n\
         local g = coroutine.wrap(function() return 1 end) g()\n\
         table.insert(log, show(pcall(function() return g() end)))\n\
         table.insert(log, show(pcall(g)))\n\
         local out = table.concat(log, '\\n')"
    ));
}

#[test]
fn wrap_generators_and_yield_across_pcall_match() {
    same(&format!(
        "{SHOW}\n\
         local log = {{}}\n\
         for v in coroutine.wrap(function() for i = 1, 3 do coroutine.yield(i, i * i) end end) do\n\
           table.insert(log, v)\n\
         end\n\
         local g = coroutine.wrap(function(...)\n\
           local ok, v = pcall(function(...) return coroutine.yield(...) end, ...)\n\
           return show(ok, v, coroutine.isyieldable())\n\
         end)\n\
         table.insert(log, show(g('a', 'b')))\n\
         table.insert(log, show(g('c')))\n\
         local out = table.concat(log, '\\n')"
    ));
}

#[test]
fn tracebacks_and_debug_info_match() {
    same(&format!(
        "{SHOW}\n\
         local log = {{}}\n\
         local co = coroutine.create(function()\n\
           table.insert(log, debug.traceback('in co'))\n\
           table.insert(log, show(debug.info(1, 'sln')))\n\
           coroutine.yield()\n\
         end)\n\
         local function outer() coroutine.resume(co) end\n\
         outer()\n\
         local g = coroutine.wrap(function() table.insert(log, debug.traceback('in wrap')) end)\n\
         g()\n\
         table.insert(log, debug.traceback(co, 'of co'))\n\
         table.insert(log, show(debug.info(coroutine.resume, 'sn')))\n\
         table.insert(log, show(debug.info(coroutine.wrap, 'sn')))\n\
         table.insert(log, show(debug.info(coroutine.wrap(print), 'sn')))\n\
         local ok, e = pcall(function() local x = nil; coroutine.wrap(function() return x.y end)() end)\n\
         table.insert(log, e)\n\
         local out = table.concat(log, '\\n')"
    ));
}

#[test]
fn the_coroutine_library_stays_read_only() {
    same(
        "local log = {}\n\
         for _, k in ipairs({ 'resume', 'wrap', 'create' }) do\n\
           local ok, e = pcall(function() coroutine[k] = print end)\n\
           table.insert(log, tostring(ok) .. ' ' .. tostring(e))\n\
         end\n\
         table.insert(log, tostring(table.isfrozen(coroutine)))\n\
         table.insert(log, tostring(pcall(rawset, coroutine, 'resume', print)))\n\
         local out = table.concat(log, '\\n')",
    );
}
