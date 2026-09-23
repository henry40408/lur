//! `lur.kv` — string keys, raw-byte values in the backend's internal `lur_kv`
//! table (spec §6). Atomic ops rely on the backend's own atomicity.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use mlua::{Error, Function, Lua, Table, Value};

use crate::capabilities::argcheck;
use crate::capabilities::storage::Shared;
use crate::runtime::RunError;

/// Set while this VM runs a `kv.update` transform, so a nested `lur.kv` call
/// errors instead of blocking on the transaction's write lock. Per VM, not
/// per thread: server VMs share worker threads and resume on any of them.
type InUpdate = Arc<AtomicBool>;

fn reject_kv_reentry(in_update: &AtomicBool, fname: &str) -> mlua::Result<()> {
    if in_update.load(Ordering::Relaxed) {
        return Err(Error::runtime(format!(
            "{fname}: cannot re-enter lur.kv from inside update()"
        )));
    }
    Ok(())
}

/// Sets the flag, restoring the prior value on drop — including
/// cancellation, so the flag can't stay stuck on a pooled VM.
struct KvUpdateGuard {
    flag: InUpdate,
    prev: bool,
}

impl KvUpdateGuard {
    fn enter(flag: InUpdate) -> Self {
        let prev = flag.swap(true, Ordering::Relaxed);
        KvUpdateGuard { flag, prev }
    }
}

impl Drop for KvUpdateGuard {
    fn drop(&mut self) {
        self.flag.store(self.prev, Ordering::Relaxed);
    }
}

pub(crate) fn install(lua: &Lua, lur: &Table, shared: &Shared) -> Result<(), RunError> {
    let kv = lua.create_table().map_err(RunError::Init)?;
    let in_update = InUpdate::default();

    {
        let shared = shared.clone();
        let in_update = in_update.clone();
        let get = lua
            .create_async_function(move |lua, key: String| {
                let shared = shared.clone();
                let in_update = in_update.clone();
                async move {
                    reject_kv_reentry(&in_update, "lur.kv.get")?;
                    let backend = shared.ensure().await?;
                    backend.kv_get(&lua, key).await
                }
            })
            .map_err(RunError::Init)?;
        kv.set("get", get).map_err(RunError::Init)?;
    }
    {
        let shared = shared.clone();
        let in_update = in_update.clone();
        let set = lua
            .create_async_function(move |_, (key, value): (String, mlua::LuaString)| {
                let shared = shared.clone();
                let in_update = in_update.clone();
                async move {
                    reject_kv_reentry(&in_update, "lur.kv.set")?;
                    let backend = shared.ensure().await?;
                    backend.kv_set(key, value.as_bytes().to_vec()).await
                }
            })
            .map_err(RunError::Init)?;
        kv.set("set", set).map_err(RunError::Init)?;
    }
    {
        let shared = shared.clone();
        let in_update = in_update.clone();
        let delete = lua
            .create_async_function(move |_, key: String| {
                let shared = shared.clone();
                let in_update = in_update.clone();
                async move {
                    reject_kv_reentry(&in_update, "lur.kv.delete")?;
                    let backend = shared.ensure().await?;
                    backend.kv_delete(key).await
                }
            })
            .map_err(RunError::Init)?;
        kv.set("delete", delete).map_err(RunError::Init)?;
    }
    {
        let shared = shared.clone();
        let in_update = in_update.clone();
        let add = lua
            .create_async_function(move |_, (key, value): (String, mlua::LuaString)| {
                let shared = shared.clone();
                let in_update = in_update.clone();
                async move {
                    reject_kv_reentry(&in_update, "lur.kv.add")?;
                    let backend = shared.ensure().await?;
                    backend.kv_add(key, value.as_bytes().to_vec()).await
                }
            })
            .map_err(RunError::Init)?;
        kv.set("add", add).map_err(RunError::Init)?;
    }
    {
        let shared = shared.clone();
        let in_update = in_update.clone();
        let cas = lua
            .create_async_function(
                move |_,
                      (key, expected, new): (
                    String,
                    Option<mlua::LuaString>,
                    Option<mlua::LuaString>,
                )| {
                    let shared = shared.clone();
                    let in_update = in_update.clone();
                    async move {
                        reject_kv_reentry(&in_update, "lur.kv.cas")?;
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
        let in_update = in_update.clone();
        let incr = lua
            .create_async_function(move |_, (key, n): (String, Value)| {
                let shared = shared.clone();
                let in_update = in_update.clone();
                async move {
                    reject_kv_reentry(&in_update, "lur.kv.incr")?;
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
        let in_update = in_update.clone();
        let decr = lua
            .create_async_function(move |_, (key, n): (String, Value)| {
                let shared = shared.clone();
                let in_update = in_update.clone();
                async move {
                    reject_kv_reentry(&in_update, "lur.kv.decr")?;
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
        let in_update = in_update.clone();
        let update = lua
            .create_async_function(move |lua, (key, func): (String, Function)| {
                let shared = shared.clone();
                let in_update = in_update.clone();
                async move {
                    reject_kv_reentry(&in_update, "lur.kv.update")?;
                    let backend = shared.ensure().await?;
                    // Guard only the transform, not the tx's own I/O, so sibling
                    // lur.async kv calls aren't rejected as re-entry.
                    let wrapped = lua.create_async_function(move |_, cur: Value| {
                        let func = func.clone();
                        let in_update = in_update.clone();
                        async move {
                            let _guard = KvUpdateGuard::enter(in_update);
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

    #[test]
    fn kv_update_guard_restores_flag_on_cancellation() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let flag = InUpdate::default();
            let parked = async {
                let _guard = KvUpdateGuard::enter(flag.clone());
                assert!(flag.load(Ordering::Relaxed), "flag set inside guard");
                std::future::pending::<()>().await;
            };
            // Polls once (entering the guard), then drops it mid-await.
            let _ = tokio::time::timeout(Duration::ZERO, parked).await;
            assert!(
                !flag.load(Ordering::Relaxed),
                "flag must be restored after the guarded future is cancelled"
            );
        });
    }

    #[test]
    fn parked_transform_does_not_block_another_vm_on_the_same_thread() {
        // Server VMs share tokio worker threads; one VM's transform parked on
        // an await must not make another VM's kv calls look like re-entry.
        let dir = tempfile::tempdir().unwrap();
        let config = crate::runtime::RuntimeConfig {
            db_path: Some(dir.path().join("kv.db")),
            ..Default::default()
        };
        let (a, _) = crate::runtime::build_lua(&config, None).unwrap();
        let (b, _) = crate::runtime::build_lua(&config, None).unwrap();
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let updating = a
                .load("lur.kv.update('a', function(_) lur.async.sleep(200); return '1' end)")
                .exec_async();
            let reading = b
                .load("lur.async.sleep(50); lur.kv.set('b', '2'); return lur.kv.get('b')")
                .eval_async::<String>();
            let (updated, read) = tokio::join!(updating, reading);
            updated.expect("update succeeds");
            assert_eq!(read.expect("other VM's kv calls succeed"), "2");
        });
    }
}
