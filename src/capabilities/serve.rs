//! `lur.serve.*` — handler registration for server mode (spec §3).
//!
//! Running `app.lua` only *collects* registrations: each `lur.serve.http`
//! call pushes a route into the host-side [`Registry`], which the server then
//! owns. In one-shot mode the registry is absent and the calls raise a clear
//! "only under `lur serve`" error.

use std::cell::RefCell;
use std::rc::Rc;

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

/// Host-side collector that `lur.serve.http` writes into. Single-threaded
/// (`Rc`/`RefCell`) because a VM and its handlers never cross threads.
#[derive(Clone, Default)]
pub struct Registry {
    routes: Rc<RefCell<Vec<Registration>>>,
}

impl Registry {
    /// Drain the collected registrations (consumes them out of the registry).
    pub fn take(&self) -> Vec<Registration> {
        std::mem::take(&mut self.routes.borrow_mut())
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
                    registry.routes.borrow_mut().push(Registration {
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
    lur.set("serve", serve).map_err(RunError::Init)?;
    Ok(())
}
