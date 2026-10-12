//! `coroutine.resume` and `coroutine.wrap`, replaced so that a
//! `lur.state.fetch` leader the script parks in a coroutine hands off as soon
//! as the resume that ran it returns.
//!
//! Each VM counts the Lua-level resumes running on it (`VmLeads::depth`); a
//! flight records that count when it starts. Resumes nest strictly on the
//! host stack: a coroutine that awaits a host call yields mlua's pending
//! sentinel to whoever resumed it, so a Lua-level resume always returns
//! before the VM's own poll does. When the resume at depth `d` returns
//! (yield, pending sentinel, return or error alike), every flight started at
//! depth `>= d` sits in a coroutine nobody is running, and
//! [`VmFlights::resumed`] removes it. With nothing led inside a resume (the
//! usual case) that is one atomic load.
//!
//! The two functions are Luau's own (`lcorolib.cpp`, `coresumey`/`auxwrapy`
//! with `DebugLuauCoroutineFinally` off, as in release builds) re-done over
//! the public C API plus the depth bookkeeping, so error messages, error
//! values, `luaL_where` positions and stack frames match the builtins. A Lua
//! wrapper doubled the cost of a resume and added frames to tracebacks; an
//! mlua Rust function would turn error values into `mlua::Error`s.
//!
//! Safety: these are the only `unsafe` functions in `lur`. They run as Lua C
//! functions, so a Lua error raised in them (argument checks, `lua_error`,
//! out of memory) unwinds their frames: no frame here holds a value with a
//! destructor at such a point. Rust panics never cross into Luau
//! ([`VmFlights::resumed`] runs under `catch_unwind`). The upvalue userdata
//! owns one `Arc<VmFlights>`, released by its destructor, so the pointer the
//! closures read is live for as long as they can be called.

#![allow(unsafe_code)]

use std::ffi::{c_int, c_void};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;
use std::sync::atomic::Ordering;

use mlua::{Function, Lua, Table, ffi};

use super::VmFlights;

/// Luau's `LUAI_MAXCSTACK` as mlua builds it (`mlua-sys`'s vendored build).
const MAX_CSTACK: c_int = 1_000_000;

/// Replace `coroutine.resume` and `coroutine.wrap`. Must run before
/// `sandbox(true)` freezes the `coroutine` table.
pub(super) fn install(lua: &Lua, vm: &Arc<VmFlights>) -> mlua::Result<()> {
    let coroutine: Table = lua.globals().get("coroutine")?;
    let owned = Arc::into_raw(Arc::clone(vm));
    // SAFETY: `owned` moves into a userdata whose destructor releases it;
    // nothing else reads it. Every push is balanced by `exec_raw`, which
    // returns the two closures left on the stack.
    let (resume, wrap): (Function, Function) = unsafe {
        lua.exec_raw((), |l| {
            let slot = ffi::lua_newuserdatadtor(l, size_of::<*const VmFlights>(), release)
                .cast::<*const VmFlights>();
            slot.write(owned);
            ffi::lua_pushvalue(l, -1);
            ffi::lua_pushcclosurek(l, co_resume, c"resume".as_ptr(), 1, None);
            ffi::lua_insert(l, -2);
            ffi::lua_pushcclosurek(l, co_wrap, c"wrap".as_ptr(), 1, None);
        })
    }?;
    coroutine.raw_set("resume", resume)?;
    coroutine.raw_set("wrap", wrap)?;
    Ok(())
}

/// Destructor of the upvalue userdata: release its `Arc<VmFlights>`.
unsafe extern "C" fn release(_: *mut ffi::lua_State, ud: *mut c_void) {
    // SAFETY: written once by `install` from `Arc::into_raw`.
    unsafe { drop(Arc::from_raw(ud.cast::<*const VmFlights>().read())) };
}

/// The `VmFlights` behind the userdata at `idx` (an upvalue).
unsafe fn vm<'a>(l: *mut ffi::lua_State, idx: c_int) -> &'a VmFlights {
    // SAFETY: only our closures carry this userdata, and it keeps the
    // `VmFlights` alive while they can run.
    unsafe {
        &*ffi::lua_touserdata(l, idx)
            .cast::<*const VmFlights>()
            .read()
    }
}

/// `coroutine.resume(co, ...)`: as Luau's `coresumey`.
unsafe extern "C-unwind" fn co_resume(l: *mut ffi::lua_State) -> c_int {
    unsafe {
        let co = ffi::lua_tothread(l, 1);
        if co.is_null() {
            ffi::luaL_typeerror(l, 1, c"thread".as_ptr());
        }
        let narg = ffi::lua_gettop(l) - 1;
        if let Some(nres) = resume(l, co, narg, vm(l, ffi::lua_upvalueindex(1))) {
            ffi::lua_pushboolean(l, 1);
            ffi::lua_insert(l, -(nres + 1));
            nres + 1
        } else {
            ffi::lua_pushboolean(l, 0);
            ffi::lua_insert(l, -2);
            2
        }
    }
}

/// `coroutine.wrap(f)`: as Luau's `cowrap`, returning [`wrapped`].
unsafe extern "C-unwind" fn co_wrap(l: *mut ffi::lua_State) -> c_int {
    unsafe {
        if ffi::lua_type(l, 1) != ffi::LUA_TFUNCTION {
            ffi::luaL_typeerror(l, 1, c"function".as_ptr());
        }
        let co = ffi::lua_newthread(l);
        ffi::lua_xpush(l, co, 1);
        ffi::lua_pushvalue(l, ffi::lua_upvalueindex(1));
        ffi::lua_pushcclosurek(l, wrapped, std::ptr::null(), 2, None);
        1
    }
}

/// A function made by `coroutine.wrap`: as Luau's `auxwrapy`.
unsafe extern "C-unwind" fn wrapped(l: *mut ffi::lua_State) -> c_int {
    unsafe {
        let co = ffi::lua_tothread(l, ffi::lua_upvalueindex(1));
        let narg = ffi::lua_gettop(l);
        if let Some(nres) = resume(l, co, narg, vm(l, ffi::lua_upvalueindex(2))) {
            return nres;
        }
        if ffi::lua_isstring(l, -1) != 0 {
            ffi::luaL_where(l, 1);
            ffi::lua_insert(l, -2);
            ffi::lua_concat(l, 2);
        }
        ffi::lua_error(l)
    }
}

/// Resume `co` with the top `narg` values of `l`, as Luau's `auxresume`.
/// `Some(n)`: it yielded or returned `n` values, now on `l`. `None`: the
/// error value is on top of `l`. Counts the resume in the VM's depth and
/// tells the VM when it returns.
unsafe fn resume(
    l: *mut ffi::lua_State,
    co: *mut ffi::lua_State,
    narg: c_int,
    vm: &VmFlights,
) -> Option<c_int> {
    const STATUS: [&std::ffi::CStr; 5] = [
        c"cannot resume running coroutine",
        c"cannot resume suspended coroutine",
        c"cannot resume normal coroutine",
        c"cannot resume dead coroutine",
        c"cannot resume dead coroutine",
    ];
    unsafe {
        if ffi::lua_status(co) != ffi::LUA_YIELD {
            let status = ffi::lua_costatus(l, co);
            if status != ffi::LUA_COSUS {
                let msg = usize::try_from(status)
                    .ok()
                    .and_then(|i| STATUS.get(i).copied())
                    .unwrap_or(STATUS[3]);
                ffi::lua_pushstring(l, msg.as_ptr());
                return None;
            }
        }
        if narg != 0 {
            if ffi::lua_checkstack(co, narg) == 0 {
                ffi::luaL_error(l, c"too many arguments to resume".as_ptr());
            }
            ffi::lua_xmove(l, co, narg);
        } else if ffi::lua_gettop(co) > MAX_CSTACK {
            ffi::luaL_error(l, c"too many arguments to resume".as_ptr());
        }

        let depth = vm.leads.depth.load(Ordering::Relaxed) + 1;
        vm.leads.depth.store(depth, Ordering::Relaxed);
        let status = ffi::lua_resume_(co, l, narg);
        vm.leads.depth.store(depth - 1, Ordering::Relaxed);
        // A panic here would leave the flights to `end_run`; it must not
        // unwind into Luau.
        let _ = catch_unwind(AssertUnwindSafe(|| vm.resumed(depth)));

        if status == ffi::LUA_OK || status == ffi::LUA_YIELD {
            let nres = ffi::lua_gettop(co);
            if nres + 1 > ffi::LUA_MINSTACK && ffi::lua_checkstack(l, nres + 1) == 0 {
                ffi::luaL_error(l, c"too many results to resume".as_ptr());
            }
            ffi::lua_xmove(co, l, nres);
            Some(nres)
        } else if status == ffi::LUA_BREAK {
            // Only a debugger breaks a coroutine; `lur` installs none.
            ffi::lua_pushstring(
                l,
                c"cannot resume a coroutine stopped at a breakpoint".as_ptr(),
            );
            None
        } else {
            ffi::lua_xmove(co, l, 1);
            None
        }
    }
}
