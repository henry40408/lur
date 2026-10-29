//! `lur.args`: `flags.NAME` from `--name value` / `--name=value` (bare
//! `--flag` → `true`), `positional` for the rest.

use mlua::{Lua, Table, Value};

use crate::runtime::RunError;

pub fn install(lua: &Lua, lur: &Table, argv: &[String]) -> Result<(), RunError> {
    let flags = lua.create_table()?;
    let positional = lua.create_table()?;
    let mut next_index = 1;

    let mut i = 0;
    while i < argv.len() {
        let arg = &argv[i];
        if let Some(rest) = arg.strip_prefix("--") {
            if let Some((name, value)) = rest.split_once('=') {
                flags.set(name, value)?;
            } else if argv.get(i + 1).is_some_and(|n| !n.starts_with("--")) {
                flags.set(rest, argv[i + 1].as_str())?;
                i += 1;
            } else {
                flags.set(rest, true)?;
            }
        } else {
            positional.set(next_index, arg.as_str())?;
            next_index += 1;
        }
        i += 1;
    }

    flags.set_readonly(true);
    positional.set_readonly(true);
    let args = lua.create_table()?;
    args.set("flags", flags)?;
    args.set("positional", positional)?;
    lur.set("args", Value::Table(args))?;
    Ok(())
}
