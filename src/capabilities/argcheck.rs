//! Shared argument extraction that preserves mlua's coercions but raises a
//! lur-voiced message on a type mismatch:
//! `lur.<cap>.<fn>: argument #<n> must be <expected>, got <actual>`.

use mlua::{Error, FromLua, Lua, Value};

/// Convert `value` (the `n`-th argument to `fname`) to `T`. Coercion is
/// identical to mlua's default (e.g. a number is still accepted where a string
/// is wanted); only the failure message is customized.
#[allow(dead_code)] // consumed by capability migration tasks
pub(crate) fn arg<T: FromLua>(
    lua: &Lua,
    value: Value,
    fname: &str,
    n: usize,
    expected: &str,
) -> mlua::Result<T> {
    let got = value.type_name();
    #[allow(clippy::map_err_ignore)]
    // original error discarded intentionally; we rewrite the message
    T::from_lua(value, lua).map_err(|_e| {
        Error::runtime(format!(
            "{fname}: argument #{n} must be {expected}, got {got}"
        ))
    })
}

#[cfg(test)]
mod tests {
    use super::arg;
    use mlua::{Lua, Value};

    #[test]
    fn good_value_converts() {
        let lua = Lua::new();
        let s: mlua::String = arg(&lua, Value::Integer(5), "lur.x.y", 1, "string").unwrap();
        // mlua coerces a number to a string — behavior preserved.
        assert_eq!(s.to_str().unwrap(), "5");
    }

    #[test]
    fn wrong_type_raises_lur_voiced_message() {
        let lua = Lua::new();
        let tbl = Value::Table(lua.create_table().unwrap());
        let err = arg::<mlua::String>(&lua, tbl, "lur.crypto.sha256", 1, "string").unwrap_err();
        assert_eq!(
            err.to_string(),
            "runtime error: lur.crypto.sha256: argument #1 must be string, got table"
        );
    }

    #[test]
    fn nil_to_optional_is_none_not_error() {
        let lua = Lua::new();
        let got: Option<i64> = arg(&lua, Value::Nil, "lur.x.y", 1, "number").unwrap();
        assert_eq!(got, None);
    }
}
