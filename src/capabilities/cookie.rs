//! `lur.cookie` — parse the `Cookie` request header and build `Set-Cookie`
//! values. Pure-compute capability, no policy gate, in the spirit of
//! `lur.base64`/`lur.crypto`: raw bytes in, raw bytes out, no automatic
//! percent-encoding. `serialize` validates its inputs so a malformed cookie
//! fails loudly rather than corrupting the response.

use mlua::{Error, Lua, Table, Value};

use crate::capabilities::argcheck;
use crate::runtime::RunError;

/// Install the flat `lur.cookie` table (`parse` + `serialize`).
pub fn install(lua: &Lua, lur: &Table) -> Result<(), RunError> {
    let cookie = lua.create_table().map_err(RunError::Init)?;

    install_parse(lua, &cookie)?;
    install_serialize(lua, &cookie)?;

    lur.set("cookie", cookie).map_err(RunError::Init)?;
    Ok(())
}

/// Trim leading/trailing optional whitespace (space / tab) from a byte slice.
fn trim_ows(mut s: &[u8]) -> &[u8] {
    while let [first, rest @ ..] = s {
        if *first == b' ' || *first == b'\t' {
            s = rest;
        } else {
            break;
        }
    }
    while let [rest @ .., last] = s {
        if *last == b' ' || *last == b'\t' {
            s = rest;
        } else {
            break;
        }
    }
    s
}

/// Split a `Cookie` header value into (name, value) byte pairs using the
/// lenient rules: split on `;`, trim OWS per segment, split on the first `=`,
/// skip a segment with no `=` or an empty name. Values are verbatim. Borrows
/// from the input; duplicate names are preserved in order (a caller building a
/// map collapses them, later-wins).
pub(crate) fn cookie_pairs(header: &[u8]) -> Vec<(&[u8], &[u8])> {
    let mut pairs = Vec::new();
    for segment in header.split(|&b| b == b';') {
        let segment = trim_ows(segment);
        let Some(eq) = segment.iter().position(|&b| b == b'=') else {
            continue;
        };
        let name = &segment[..eq];
        if name.is_empty() {
            continue;
        }
        pairs.push((name, &segment[eq + 1..]));
    }
    pairs
}

/// `lur.cookie.parse(header) -> { name = value, ... }`.
fn install_parse(lua: &Lua, cookie: &Table) -> Result<(), RunError> {
    let parse = lua
        .create_function(|lua, header: Value| {
            let header: mlua::String = argcheck::arg(lua, header, "lur.cookie.parse", 1, "string")?;
            let out = lua.create_table()?;
            let bytes = header.as_bytes();
            // Later duplicate overwrites earlier: plain table assignment.
            for (name, value) in cookie_pairs(&bytes) {
                out.set(lua.create_string(name)?, lua.create_string(value)?)?;
            }
            Ok(out)
        })
        .map_err(RunError::Init)?;
    cookie.set("parse", parse).map_err(RunError::Init)?;
    Ok(())
}

/// RFC 6265 cookie-name separator characters (a name is a token: no controls,
/// no separators, no space/tab).
fn is_separator(b: u8) -> bool {
    matches!(
        b,
        b'(' | b')'
            | b'<'
            | b'>'
            | b'@'
            | b','
            | b';'
            | b':'
            | b'\\'
            | b'"'
            | b'/'
            | b'['
            | b']'
            | b'?'
            | b'='
            | b'{'
            | b'}'
    )
}

/// Validate a cookie name: non-empty, all bytes are token characters
/// (visible ASCII `0x21..=0x7e`, excluding separators).
fn validate_name(name: &[u8]) -> Result<(), Error> {
    if name.is_empty() {
        return Err(Error::runtime(
            "lur.cookie.serialize: name must not be empty",
        ));
    }
    if !name
        .iter()
        .all(|&b| (0x21..=0x7e).contains(&b) && !is_separator(b))
    {
        return Err(Error::runtime(
            "lur.cookie.serialize: name contains an invalid character",
        ));
    }
    Ok(())
}

/// Reject bytes that would break the header: controls (`< 0x20`), DEL
/// (`0x7f`), and `;`. Used for the cookie value and for the
/// `domain`/`path`/`expires` attribute values. Bytes `>= 0x80` are allowed
/// (raw-bytes stance; the serve layer's `HeaderValue::from_bytes` is the
/// final backstop).
fn reject_bad_bytes(label: &str, v: &[u8]) -> Result<(), Error> {
    if v.iter().any(|&b| b < 0x20 || b == 0x7f || b == b';') {
        return Err(Error::runtime(format!(
            "lur.cookie.serialize: {label} contains an invalid character"
        )));
    }
    Ok(())
}

/// Canonicalize a `same_site` value, accepting any case.
fn canon_same_site(v: &[u8]) -> Result<&'static str, Error> {
    if v.eq_ignore_ascii_case(b"strict") {
        Ok("Strict")
    } else if v.eq_ignore_ascii_case(b"lax") {
        Ok("Lax")
    } else if v.eq_ignore_ascii_case(b"none") {
        Ok("None")
    } else {
        Err(Error::runtime(
            "lur.cookie.serialize: same_site must be Strict, Lax, or None",
        ))
    }
}

/// `lur.cookie.serialize(name, value, opts?) -> string`. Returns one
/// `Set-Cookie` value (without the `Set-Cookie:` prefix).
fn install_serialize(lua: &Lua, cookie: &Table) -> Result<(), RunError> {
    let serialize = lua
        .create_function(|lua, (name, value, opts): (Value, Value, Option<Table>)| {
            let name: mlua::String = argcheck::arg(lua, name, "lur.cookie.serialize", 1, "string")?;
            let value: mlua::String =
                argcheck::arg(lua, value, "lur.cookie.serialize", 2, "string")?;
            let name = name.as_bytes();
            let value = value.as_bytes();
            validate_name(&name)?;
            reject_bad_bytes("value", &value)?;

            let mut out: Vec<u8> = Vec::new();
            out.extend_from_slice(&name);
            out.push(b'=');
            out.extend_from_slice(&value);

            if let Some(opts) = opts {
                if let Some(domain) = opts.get::<Option<mlua::String>>("domain")? {
                    let domain = domain.as_bytes();
                    reject_bad_bytes("domain", &domain)?;
                    out.extend_from_slice(b"; Domain=");
                    out.extend_from_slice(&domain);
                }
                if let Some(path) = opts.get::<Option<mlua::String>>("path")? {
                    let path = path.as_bytes();
                    reject_bad_bytes("path", &path)?;
                    out.extend_from_slice(b"; Path=");
                    out.extend_from_slice(&path);
                }
                if let Some(max_age) = opts.get::<Option<Value>>("max_age")? {
                    let n = match max_age {
                        Value::Integer(i) => i,
                        Value::Number(f)
                            if f.is_finite()
                                && f.fract() == 0.0
                                && f >= i64::MIN as f64
                                && f < (1u64 << 63) as f64 =>
                        {
                            f as i64
                        }
                        _ => {
                            return Err(Error::runtime(
                                "lur.cookie.serialize: max_age must be an integer",
                            ));
                        }
                    };
                    out.extend_from_slice(format!("; Max-Age={n}").as_bytes());
                }
                if let Some(expires) = opts.get::<Option<mlua::String>>("expires")? {
                    let expires = expires.as_bytes();
                    reject_bad_bytes("expires", &expires)?;
                    out.extend_from_slice(b"; Expires=");
                    out.extend_from_slice(&expires);
                }

                let same_site = match opts.get::<Option<mlua::String>>("same_site")? {
                    Some(s) => Some(canon_same_site(&s.as_bytes())?),
                    None => None,
                };
                let secure = opts.get::<Option<bool>>("secure")?.unwrap_or(false);
                let http_only = opts.get::<Option<bool>>("http_only")?.unwrap_or(false);

                if same_site == Some("None") && !secure {
                    return Err(Error::runtime(
                        "lur.cookie.serialize: same_site=None requires secure=true",
                    ));
                }

                if http_only {
                    out.extend_from_slice(b"; HttpOnly");
                }
                if secure {
                    out.extend_from_slice(b"; Secure");
                }
                if let Some(s) = same_site {
                    out.extend_from_slice(b"; SameSite=");
                    out.extend_from_slice(s.as_bytes());
                }
            }

            lua.create_string(&out)
        })
        .map_err(RunError::Init)?;
    cookie.set("serialize", serialize).map_err(RunError::Init)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::cookie_pairs;

    #[test]
    fn cookie_pairs_basic_and_multiple() {
        assert_eq!(cookie_pairs(b"sid=abc"), vec![(&b"sid"[..], &b"abc"[..])]);
        assert_eq!(
            cookie_pairs(b"sid=abc; theme=dark"),
            vec![(&b"sid"[..], &b"abc"[..]), (&b"theme"[..], &b"dark"[..])]
        );
    }

    #[test]
    fn cookie_pairs_trims_ows_including_tabs() {
        assert_eq!(
            cookie_pairs(b"  a=1 ;\tb=2\t"),
            vec![(&b"a"[..], &b"1"[..]), (&b"b"[..], &b"2"[..])]
        );
    }

    #[test]
    fn cookie_pairs_is_lenient() {
        assert_eq!(cookie_pairs(b""), Vec::<(&[u8], &[u8])>::new());
        assert_eq!(cookie_pairs(b"garbage; x=1"), vec![(&b"x"[..], &b"1"[..])]);
        assert_eq!(cookie_pairs(b"=noname; y=2"), vec![(&b"y"[..], &b"2"[..])]);
    }

    #[test]
    fn cookie_pairs_keeps_inner_equals_and_duplicates() {
        assert_eq!(cookie_pairs(b"t=a=b"), vec![(&b"t"[..], &b"a=b"[..])]);
        assert_eq!(
            cookie_pairs(b"k=1; k=2"),
            vec![(&b"k"[..], &b"1"[..]), (&b"k"[..], &b"2"[..])]
        );
    }
}
