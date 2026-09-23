//! `lur.serve.*` — collects route/cron registrations into a [`Registry`] while
//! `app.lua` runs (spec §3). Raises in one-shot mode, where there is none.

use std::sync::{Arc, Mutex};

use mlua::{Function, Lua, Table, Value};

use crate::runtime::RunError;

pub struct Registration {
    /// Upper-cased; `"ANY"` matches every method.
    pub method: String,
    /// Literal segments or `:name` params.
    pub path: String,
    pub handler: Function,
}

pub struct CronRegistration {
    /// 6-field cron spec (`sec min hour dom mon dow`).
    pub spec: String,
    /// Defaults to `cron[<spec>]`.
    pub name: String,
    /// Allow overlapping runs; default is single-flight.
    pub overlap: bool,
    /// Overrides the global per-event timeout.
    pub timeout_ms: Option<u64>,
    pub handler: Function,
}

/// `Arc<Mutex>` because the `send` feature requires `Send` closures.
#[derive(Clone, Default)]
pub struct Registry {
    routes: Arc<Mutex<Vec<Registration>>>,
    crons: Arc<Mutex<Vec<CronRegistration>>>,
}

impl Registry {
    pub fn take(&self) -> Vec<Registration> {
        std::mem::take(&mut self.routes.lock().expect("registry mutex poisoned"))
    }

    pub fn take_crons(&self) -> Vec<CronRegistration> {
        std::mem::take(&mut self.crons.lock().expect("registry mutex poisoned"))
    }
}

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
