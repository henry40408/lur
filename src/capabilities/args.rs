//! `lur.args` — the script's parsed argument vector (spec §4).
//!
//! `lur.args.flags.NAME` holds `--name value` / `--name=value` (a bare `--flag`
//! is the boolean `true`); `lur.args.positional` is the array of non-flag
//! arguments in order.

use mlua::{Lua, Table, Value};

use crate::runtime::RunError;

/// Install `lur.args` built from the raw script argv.
pub fn install(lua: &Lua, lur: &Table, argv: &[String]) -> Result<(), RunError> {
    let flags = lua.create_table().map_err(RunError::Init)?;
    let positional = lua.create_table().map_err(RunError::Init)?;
    let mut next_index = 1;

    let mut i = 0;
    while i < argv.len() {
        let arg = &argv[i];
        if let Some(rest) = arg.strip_prefix("--") {
            if let Some((name, value)) = rest.split_once('=') {
                flags.set(name, value).map_err(RunError::Init)?;
            } else if argv.get(i + 1).is_some_and(|n| !n.starts_with("--")) {
                flags
                    .set(rest, argv[i + 1].as_str())
                    .map_err(RunError::Init)?;
                i += 1;
            } else {
                flags.set(rest, true).map_err(RunError::Init)?;
            }
        } else {
            positional
                .set(next_index, arg.as_str())
                .map_err(RunError::Init)?;
            next_index += 1;
        }
        i += 1;
    }

    let args = lua.create_table().map_err(RunError::Init)?;
    args.set("flags", flags).map_err(RunError::Init)?;
    args.set("positional", positional).map_err(RunError::Init)?;
    lur.set("args", Value::Table(args))
        .map_err(RunError::Init)?;
    Ok(())
}
