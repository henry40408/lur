//! `lur.kv` — key/value storage over the shared `lur_kv(key TEXT, value BLOB)`
//! table (spec §6). Keys are strings, values raw bytes. Atomic operations
//! (add/cas/incr/decr/update) use `SQLite`'s own atomicity; see the design spec.

use std::cell::Cell;

use mlua::{Error, Function, Lua, Table, Value};

use crate::capabilities::argcheck;
use crate::capabilities::storage::Shared;
use crate::runtime::RunError;

thread_local! {
    /// Set while a kv.update transform runs, so a nested lur.kv/lur.db call on
    /// the same stack raises a clear error instead of deadlocking on the pinned
    /// transaction connection.
    static IN_KV_UPDATE: Cell<bool> = const { Cell::new(false) };
}

fn reject_kv_reentry(fname: &str) -> mlua::Result<()> {
    if IN_KV_UPDATE.with(Cell::get) {
        return Err(Error::runtime(format!(
            "{fname}: cannot re-enter lur.kv from inside update()"
        )));
    }
    Ok(())
}

/// RAII guard for `IN_KV_UPDATE`. `enter` sets the flag and remembers the prior
/// value; `Drop` restores it — so a transform that returns, errors, or is
/// cancelled (its future dropped mid-await) always restores the flag instead of
/// leaving it stuck `true` and poisoning later kv/db calls on the pooled VM.
struct KvUpdateGuard(bool);

impl KvUpdateGuard {
    fn enter() -> Self {
        KvUpdateGuard(IN_KV_UPDATE.with(|f| f.replace(true)))
    }
}

impl Drop for KvUpdateGuard {
    fn drop(&mut self) {
        IN_KV_UPDATE.with(|f| f.set(self.0));
    }
}

/// Install `lur.kv` sharing `db`'s lazily-opened backend.
pub(crate) fn install(lua: &Lua, lur: &Table, shared: &Shared) -> Result<(), RunError> {
    let kv = lua.create_table().map_err(RunError::Init)?;

    {
        let shared = shared.clone();
        let get = lua
            .create_async_function(move |lua, key: String| {
                let shared = shared.clone();
                async move {
                    reject_kv_reentry("lur.kv.get")?;
                    let backend = shared.ensure().await?;
                    backend.kv_get(&lua, key).await
                }
            })
            .map_err(RunError::Init)?;
        kv.set("get", get).map_err(RunError::Init)?;
    }
    {
        let shared = shared.clone();
        let set = lua
            .create_async_function(move |_, (key, value): (String, mlua::LuaString)| {
                let shared = shared.clone();
                async move {
                    reject_kv_reentry("lur.kv.set")?;
                    let backend = shared.ensure().await?;
                    backend.kv_set(key, value.as_bytes().to_vec()).await
                }
            })
            .map_err(RunError::Init)?;
        kv.set("set", set).map_err(RunError::Init)?;
    }
    {
        let shared = shared.clone();
        let delete = lua
            .create_async_function(move |_, key: String| {
                let shared = shared.clone();
                async move {
                    reject_kv_reentry("lur.kv.delete")?;
                    let backend = shared.ensure().await?;
                    backend.kv_delete(key).await
                }
            })
            .map_err(RunError::Init)?;
        kv.set("delete", delete).map_err(RunError::Init)?;
    }
    {
        let shared = shared.clone();
        let add = lua
            .create_async_function(move |_, (key, value): (String, mlua::LuaString)| {
                let shared = shared.clone();
                async move {
                    reject_kv_reentry("lur.kv.add")?;
                    let backend = shared.ensure().await?;
                    backend.kv_add(key, value.as_bytes().to_vec()).await
                }
            })
            .map_err(RunError::Init)?;
        kv.set("add", add).map_err(RunError::Init)?;
    }
    {
        let shared = shared.clone();
        let cas = lua
            .create_async_function(
                move |_,
                      (key, expected, new): (
                    String,
                    Option<mlua::LuaString>,
                    Option<mlua::LuaString>,
                )| {
                    let shared = shared.clone();
                    async move {
                        reject_kv_reentry("lur.kv.cas")?;
                        let backend = shared.ensure().await?;
                        let exp = expected.map(|s| s.as_bytes().to_vec());
                        let neu = new.map(|s| s.as_bytes().to_vec());
                        backend.kv_cas(key, exp, neu).await
                    }
                },
            )
            .map_err(RunError::Init)?;
        kv.set("cas", cas).map_err(RunError::Init)?;
    }
    {
        let shared = shared.clone();
        let incr = lua
            .create_async_function(move |_, (key, n): (String, Value)| {
                let shared = shared.clone();
                async move {
                    reject_kv_reentry("lur.kv.incr")?;
                    let n = argcheck::integer_arg(n, "lur.kv.incr", 2)?;
                    let backend = shared.ensure().await?;
                    backend.kv_incr("lur.kv.incr", key, n.unwrap_or(1)).await
                }
            })
            .map_err(RunError::Init)?;
        kv.set("incr", incr).map_err(RunError::Init)?;
    }
    {
        let shared = shared.clone();
        let decr = lua
            .create_async_function(move |_, (key, n): (String, Value)| {
                let shared = shared.clone();
                async move {
                    reject_kv_reentry("lur.kv.decr")?;
                    let n = argcheck::integer_arg(n, "lur.kv.decr", 2)?;
                    let delta = n
                        .unwrap_or(1)
                        .checked_neg()
                        .ok_or_else(|| Error::runtime("lur.kv.decr: step too large"))?;
                    let backend = shared.ensure().await?;
                    backend.kv_incr("lur.kv.decr", key, delta).await
                }
            })
            .map_err(RunError::Init)?;
        kv.set("decr", decr).map_err(RunError::Init)?;
    }

    {
        let shared = shared.clone();
        let update = lua
            .create_async_function(move |lua, (key, func): (String, Function)| {
                let shared = shared.clone();
                async move {
                    reject_kv_reentry("lur.kv.update")?;
                    let backend = shared.ensure().await?;
                    // Hold IN_KV_UPDATE only around the user transform, not
                    // across the transaction's I/O awaits (begin/read/write/
                    // commit), so sibling lur.async kv/db calls interleaved
                    // while this update is parked on DB I/O are not spuriously
                    // rejected as re-entry (matches pre-seam guard timing).
                    let wrapped = lua.create_async_function(move |_, cur: Value| {
                        let func = func.clone();
                        async move {
                            // Guard restores IN_KV_UPDATE on every exit path,
                            // including cancellation (future dropped mid-await).
                            let _guard = KvUpdateGuard::enter();
                            func.call_async::<Value>(cur).await
                        }
                    })?;
                    backend.kv_update(&lua, key, wrapped).await
                }
            })
            .map_err(RunError::Init)?;
        kv.set("update", update).map_err(RunError::Init)?;
    }

    lur.set("kv", kv).map_err(RunError::Init)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    // A guard entered inside a future that is then cancelled mid-await must
    // still restore IN_KV_UPDATE to false — otherwise the flag poisons every
    // later kv/db call on the pooled VM (backlog item 0).
    #[test]
    fn kv_update_guard_restores_flag_on_cancellation() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            assert!(!IN_KV_UPDATE.with(Cell::get), "flag starts clear");
            let parked = async {
                let _guard = KvUpdateGuard::enter();
                assert!(IN_KV_UPDATE.with(Cell::get), "flag set inside guard");
                std::future::pending::<()>().await;
            };
            // The zero-duration timeout polls `parked` once (entering the guard),
            // then fires and drops it while parked — exactly the cancellation path.
            let _ = tokio::time::timeout(Duration::ZERO, parked).await;
            assert!(
                !IN_KV_UPDATE.with(Cell::get),
                "flag must be restored after the guarded future is cancelled"
            );
        });
    }
}
