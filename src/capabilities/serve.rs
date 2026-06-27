//! `lur.serve.*` — handler registration for server mode (spec §3).
//!
//! Running `app.lua` only *collects* registrations: each `lur.serve.http`
//! call pushes a route into the host-side [`Registry`], which the server then
//! owns. In one-shot mode the registry is absent and the calls raise a clear
//! "only under `lur serve`" error.

use std::sync::{Arc, Mutex};

use mlua::{Function, Lua, Table, Value};

use crate::runtime::RunError;

/// One declared route collected during `app.lua` warm-up.
pub struct Registration {
    /// HTTP method, upper-cased (`"ANY"` matches every method).
    pub method: String,
    /// Route path (exact match in v1; `:param` segments come later).
    pub path: String,
    /// The Lua handler closure, VM-bound.
    pub handler: Function,
}

/// One declared cron job collected during `app.lua` warm-up.
pub struct CronRegistration {
    /// 6-field cron spec (`sec min hour dom mon dow`).
    pub spec: String,
    /// Job name (for logs and single-flight tracking); auto-derived if omitted.
    pub name: String,
    /// When `true`, overlapping runs are allowed; default is single-flight.
    pub overlap: bool,
    /// Per-run timeout in milliseconds, overriding the global per-event budget.
    pub timeout_ms: Option<u64>,
    /// The Lua handler closure, VM-bound.
    pub handler: Function,
}

/// Host-side collector that `lur.serve.*` writes into. `Arc`/`Mutex` because the
/// `send` feature requires registration closures to be `Send` (a pooled VM can
/// be checked out across worker threads).
#[derive(Clone, Default)]
pub struct Registry {
    routes: Arc<Mutex<Vec<Registration>>>,
    crons: Arc<Mutex<Vec<CronRegistration>>>,
}

impl Registry {
    /// Drain the collected HTTP route registrations.
    pub fn take(&self) -> Vec<Registration> {
        std::mem::take(&mut self.routes.lock().expect("registry mutex poisoned"))
    }

    /// Drain the collected cron registrations.
    pub fn take_crons(&self) -> Vec<CronRegistration> {
        std::mem::take(&mut self.crons.lock().expect("registry mutex poisoned"))
    }
}

/// Install `lur.serve`. With a `registry` the calls collect routes; without
/// one (one-shot mode) they raise a registration error.
pub fn install(lua: &Lua, lur: &Table, registry: Option<&Registry>) -> Result<(), RunError> {
    let serve = lua.create_table().map_err(RunError::Init)?;

    let http = match registry {
        Some(registry) => {
            let registry = registry.clone();
            lua.create_function(
                move |_, (method, path, handler): (String, String, Function)| {
                    registry
                        .routes
                        .lock()
                        .expect("registry mutex poisoned")
                        .push(Registration {
                            method: method.to_uppercase(),
                            path,
                            handler,
                        });
                    Ok(())
                },
            )
        }
        None => lua.create_function(|_, _args: (Value, Value, Value)| -> mlua::Result<()> {
            Err(mlua::Error::RuntimeError(
                "lur.serve.http is only available under `lur serve`".into(),
            ))
        }),
    }
    .map_err(RunError::Init)?;

    serve.set("http", http).map_err(RunError::Init)?;

    let cron = match registry {
        Some(registry) => {
            let registry = registry.clone();
            lua.create_function(
                move |_, (spec, handler, opts): (String, Function, Option<Table>)| {
                    let (name, overlap, timeout_ms) = match opts {
                        Some(opts) => (
                            opts.get::<Option<String>>("name")?,
                            opts.get::<Option<bool>>("overlap")?.unwrap_or(false),
                            opts.get::<Option<u64>>("timeout")?,
                        ),
                        None => (None, false, None),
                    };
                    let name = name.unwrap_or_else(|| format!("cron[{spec}]"));
                    registry
                        .crons
                        .lock()
                        .expect("registry mutex poisoned")
                        .push(CronRegistration {
                            spec,
                            name,
                            overlap,
                            timeout_ms,
                            handler,
                        });
                    Ok(())
                },
            )
        }
        None => lua.create_function(|_, _args: (Value, Value, Value)| -> mlua::Result<()> {
            Err(mlua::Error::RuntimeError(
                "lur.serve.cron is only available under `lur serve`".into(),
            ))
        }),
    }
    .map_err(RunError::Init)?;

    serve.set("cron", cron).map_err(RunError::Init)?;
    lur.set("serve", serve).map_err(RunError::Init)?;
    Ok(())
}
