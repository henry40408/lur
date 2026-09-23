//! Argument extraction with mlua's coercions but lur-voiced errors:
//! `lur.<cap>.<fn>: argument #<n> must be <expected>, got <actual>`.

use mlua::{Error, FromLua, Lua, Value};

/// Optional integer argument; whole-number floats in i64 range are accepted.
pub(crate) fn integer_arg(value: Value, fname: &str, n: usize) -> mlua::Result<Option<i64>> {
    match value {
        Value::Nil => Ok(None),
        Value::Integer(i) => Ok(Some(i)),
        Value::Number(f) => {
            if f.fract() != 0.0 {
                return Err(mlua::Error::RuntimeError(format!(
                    "{fname}: argument #{n} must be integer, got float"
                )));
            }
            // `i64::MAX as f64` rounds up to 2^63, which is out of range.
            if f < i64::MIN as f64 || f >= i64::MAX as f64 {
                return Err(mlua::Error::RuntimeError(format!(
                    "{fname}: argument #{n} out of integer range"
                )));
            }
            Ok(Some(f as i64))
        }
        other => Err(mlua::Error::RuntimeError(format!(
            "{fname}: argument #{n} must be integer, got {}",
            other.type_name()
        ))),
    }
}

/// Convert argument `n` of `fname` to `T` with mlua's coercions (a number still
/// passes as a string); only the error message changes.
pub(crate) fn arg<T: FromLua>(
    lua: &Lua,
    value: Value,
    fname: &str,
    n: usize,
    expected: &str,
) -> mlua::Result<T> {
    let got = value.type_name();
    #[allow(clippy::map_err_ignore)]
    // The original error is replaced on purpose.
    T::from_lua(value, lua).map_err(|_e| {
        Error::runtime(format!(
            "{fname}: argument #{n} must be {expected}, got {got}"
        ))
    })
}

#[cfg(test)]
mod tests {
    use super::{arg, integer_arg};
    use mlua::{Lua, Value};

    #[test]
    fn good_value_converts() {
        let lua = Lua::new();
        let s: mlua::LuaString = arg(&lua, Value::Integer(5), "lur.x.y", 1, "string").unwrap();
        // mlua coerces a number to a string — behavior preserved.
        assert_eq!(s.to_str().unwrap(), "5");
    }

    #[test]
    fn wrong_type_raises_lur_voiced_message() {
        let lua = Lua::new();
        let tbl = Value::Table(lua.create_table().unwrap());
        let err = arg::<mlua::LuaString>(&lua, tbl, "lur.crypto.sha256", 1, "string").unwrap_err();
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

    #[test]
    fn integer_arg_accepts_integer() {
        assert_eq!(
            integer_arg(Value::Integer(3), "lur.x.y", 1).unwrap(),
            Some(3)
        );
    }

    #[test]
    fn integer_arg_accepts_whole_float() {
        assert_eq!(
            integer_arg(Value::Number(2.0), "lur.x.y", 1).unwrap(),
            Some(2)
        );
    }

    #[test]
    fn integer_arg_rejects_fractional_float() {
        let err = integer_arg(Value::Number(1.5), "lur.x.y", 1).unwrap_err();
        assert!(
            err.to_string().contains("must be integer"),
            "unexpected message: {err}"
        );
    }

    #[test]
    fn integer_arg_nil_is_none() {
        assert_eq!(integer_arg(Value::Nil, "lur.x.y", 1).unwrap(), None);
    }

    #[test]
    fn integer_arg_rejects_two_pow_63() {
        // 2^63 == `i64::MAX as f64`; it must not saturate to i64::MAX.
        let err = integer_arg(Value::Number(2f64.powi(63)), "lur.x.y", 1).unwrap_err();
        assert!(err.to_string().contains("out of integer range"), "{err}");
    }
}
