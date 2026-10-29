//! `lur.serve.*` — collects route/cron registrations into a [`Registry`] while
//! `app.lua` runs. Raises in one-shot mode, where there is none.
//!
//! Also `lur.serve.abort`: raises a structured `{ kind = "abort", status,
//! message, data }` table. mlua flattens a table error to a string when it
//! crosses `call_async`, so the server cannot read the status off the caught
//! error; instead `abort` stashes its response in an [`AbortSlot`] shared with
//! the pooled VM, and `serve::call_handler` consults it when the handler fails.

use std::sync::{Arc, Mutex};

use mlua::{Function, Lua, Table, Value};

use crate::capabilities::{argcheck, json, raise};
use crate::runtime::RunError;

/// The response an `abort` asks for, kept until the handler's outcome is known.
pub struct PendingAbort {
    pub status: u16,
    pub message: String,
    /// The JSON body, encoded at `abort` time.
    pub body: Vec<u8>,
}

/// Per-VM abort state. `in_request` is true only while an HTTP handler runs.
/// `call_handler` resets it at the start of every run, so an abort caught by
/// `pcall` cannot leak into a later request on the pooled VM.
#[derive(Default)]
pub struct AbortState {
    pub in_request: bool,
    pub pending: Option<PendingAbort>,
}

pub type AbortSlot = Arc<Mutex<AbortState>>;

/// Per-VM helpers for `abort` and `req.valid_*`; kept in `lua` app data.
#[derive(Clone)]
pub(crate) struct ServeCtx {
    pub slot: AbortSlot,
    pub factory: Function,
    pub meta: Table,
    /// `lur.validate.try` as installed. `lur.*` is read-only, so this is defense
    /// in depth: validation results stay unforgeable even if that regresses.
    pub try_fn: Function,
}

/// Status must be an integer in `400..=599`.
pub(crate) fn status_arg(value: &Value, fname: &str, what: &str) -> mlua::Result<u16> {
    let n = match value {
        Value::Integer(i) => Some(*i),
        Value::Number(f) => argcheck::whole_f64_to_i64(*f),
        _ => None,
    };
    match n.and_then(|n| u16::try_from(n).ok()) {
        Some(n) if (400..=599).contains(&n) => Ok(n),
        _ => Err(mlua::Error::runtime(format!(
            "{fname}: {what} must be an integer in 400..=599, got {}",
            match value {
                Value::Integer(i) => i.to_string(),
                Value::Number(f) => f.to_string(),
                other => other.type_name().to_owned(),
            }
        ))),
    }
}

/// What `lur.serve.abort` was asked to send.
pub(crate) struct Abort<'a> {
    pub status: u16,
    pub message: &'a str,
    pub data: Value,
}

/// Stash the response and build the error table to raise. `data` must be
/// JSON-encodable (checked here, so a bad value is an ordinary error at the
/// call site rather than a surprise when the response is written).
pub(crate) fn abort_table(lua: &Lua, ctx: &ServeCtx, abort: Abort<'_>) -> mlua::Result<Table> {
    let Abort {
        status,
        message,
        data,
    } = abort;
    let in_request = ctx.slot.lock().expect("abort slot poisoned").in_request;
    if !in_request {
        return Err(mlua::Error::runtime(
            "lur.serve.abort: no request to abort (only valid inside an HTTP handler)",
        ));
    }
    // Encode before taking the lock again; `data` can be large.
    let mut body = serde_json::Map::new();
    body.insert(
        "error".into(),
        serde_json::Value::String(message.to_owned()),
    );
    if !data.is_nil() {
        let encoded = json::lua_to_json(&data).map_err(|e| {
            mlua::Error::runtime(format!("lur.serve.abort: data is not JSON-encodable: {e}"))
        })?;
        body.insert("data".into(), encoded);
    }
    let body = serde_json::to_vec(&serde_json::Value::Object(body))
        .map_err(|e| mlua::Error::runtime(format!("lur.serve.abort: {e}")))?;
    ctx.slot.lock().expect("abort slot poisoned").pending = Some(PendingAbort {
        status,
        message: message.to_owned(),
        body,
    });

    let err = lua.create_table()?;
    err.set("kind", "abort")?;
    err.set("status", status)?;
    err.set("message", message)?;
    if !data.is_nil() {
        err.set("data", data)?;
    }
    err.set_metatable(Some(ctx.meta.clone()))?;
    Ok(err)
}

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
    abort: AbortSlot,
}

impl Registry {
    /// The VM's abort slot, for the server to reset and consult per run.
    pub fn abort_slot(&self) -> AbortSlot {
        Arc::clone(&self.abort)
    }

    pub fn take(&self) -> Vec<Registration> {
        std::mem::take(&mut self.routes.lock().expect("registry mutex poisoned"))
    }

    pub fn take_crons(&self) -> Vec<CronRegistration> {
        std::mem::take(&mut self.crons.lock().expect("registry mutex poisoned"))
    }
}

/// `factory` is the VM's [`raise::factory`].
pub fn install(
    lua: &Lua,
    lur: &Table,
    factory: &Function,
    registry: Option<&Registry>,
) -> Result<(), RunError> {
    let serve = lua.create_table()?;
    serve.set("abort", abort_fn(lua, lur, factory, registry)?)?;
    serve.set("http", http_fn(lua, registry)?)?;
    serve.set("cron", cron_fn(lua, registry)?)?;
    lur.set("serve", serve)?;
    Ok(())
}

/// Stands in for `lur.serve.<name>` in one-shot mode, where there is no
/// registry to record into.
fn unavailable_fn(lua: &Lua, name: &str) -> mlua::Result<Function> {
    let message = format!("lur.serve.{name} is only available under `lur serve`");
    lua.create_function(move |_, _args: (Value, Value, Value)| -> mlua::Result<()> {
        Err(mlua::Error::RuntimeError(message.clone()))
    })
}

fn abort_fn(
    lua: &Lua,
    lur: &Table,
    factory: &Function,
    registry: Option<&Registry>,
) -> mlua::Result<Function> {
    let Some(registry) = registry else {
        return unavailable_fn(lua, "abort");
    };
    let ctx = ServeCtx {
        slot: registry.abort_slot(),
        factory: factory.clone(),
        meta: raise::error_meta(lua)?,
        try_fn: lur
            .get::<Table>("validate")
            .and_then(|v| v.get::<Function>("try"))?,
    };
    let inner = {
        let ctx = ctx.clone();
        lua.create_function(move |lua, (status, message, data): (Value, Value, Value)| {
            const F: &str = "lur.serve.abort";
            let status = status_arg(&status, F, "argument #1 (status)")?;
            let message: mlua::LuaString = argcheck::arg(
                lua,
                message,
                argcheck::ArgSpec {
                    fname: F,
                    n: 2,
                    expected: "string",
                },
            )?;
            let message = message.to_str().map_err(|e| {
                mlua::Error::runtime(format!("{F}: argument #2 must be valid UTF-8: {e}"))
            })?;
            let abort = Abort {
                status,
                message: &message,
                data,
            };
            let err = abort_table(lua, &ctx, abort)?;
            Ok((false, Value::Table(err)))
        })?
    };
    let wrapped = raise::raising(&ctx.factory, inner)?;
    lua.set_app_data(ctx);
    Ok(wrapped)
}

fn http_fn(lua: &Lua, registry: Option<&Registry>) -> mlua::Result<Function> {
    let Some(registry) = registry else {
        return unavailable_fn(lua, "http");
    };
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

fn cron_fn(lua: &Lua, registry: Option<&Registry>) -> mlua::Result<Function> {
    let Some(registry) = registry else {
        return unavailable_fn(lua, "cron");
    };
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
