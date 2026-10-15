//! `lur.charset` — decode/encode legacy encodings (Big5, GBK, `Shift_JIS`, …)
//! with `encoding_rs`, so a scraped page that isn't UTF-8 can reach
//! `lur.html` and `lur.feed` intact.
//!
//! ## Design notes
//!
//! **`lur.charset`**: `encoding_rs` (the Encoding Standard's labels and BOM/replacement rules,
//! as browsers behave), not `chardet`-style guessing: an unlabeled page decodes as UTF-8 and
//! the script says otherwise, so output never depends on a heuristic. Explicit, not automatic
//! in `lur.http`/`lur.html.parse`: both stay byte-oriented, and decoding is one call
//! (`lur.charset.decode(res.body, res.headers["content-type"])`). A bare label or a whole
//! `Content-Type` is accepted as the hint, so callers don't parse the header. An unknown
//! label raises rather than guessing (wrap in `pcall` to fall back). `<meta>` sniffing only
//! looks at `charset=` in the first 1 KiB and ignores a UTF-16 declaration, as browsers do.
//! Not done: encoding detection, streaming decode, UTF-16 targets for `encode`.

use encoding_rs::{Encoding, UTF_8};
use mlua::{Error, Lua, Table, Value};

use crate::capabilities::argcheck;
use crate::runtime::RunError;

/// How far into the document a `<meta>` declaration may sit (the HTML spec's limit).
const SNIFF_BYTES: usize = 1024;

pub fn install(lua: &Lua, lur: &Table) -> Result<(), RunError> {
    let charset = lua.create_table().map_err(RunError::Init)?;

    let decode = lua
        .create_function(|lua, (data, hint): (Value, Option<String>)| {
            let data: mlua::LuaString =
                argcheck::arg(lua, data, "lur.charset.decode", 1, "string")?;
            let bytes = data.as_bytes();
            let enc = match hint.as_deref().and_then(label_of) {
                Some(label) => Some(lookup("lur.charset.decode", label)?),
                None => None,
            };
            lua.create_string(decode_bytes(&bytes, enc).as_bytes())
        })
        .map_err(RunError::Init)?;
    charset.set("decode", decode).map_err(RunError::Init)?;

    let encode = lua
        .create_function(|lua, (text, label): (Value, String)| {
            let text: mlua::LuaString =
                argcheck::arg(lua, text, "lur.charset.encode", 1, "string")?;
            let text = std::str::from_utf8(&text.as_bytes())
                .map_err(|e| {
                    Error::runtime(format!(
                        "lur.charset.encode: input is not valid UTF-8 ({e})"
                    ))
                })?
                .to_owned();
            let enc = lookup("lur.charset.encode", &label)?;
            // encoding_rs would silently emit UTF-8 for UTF-16 targets.
            if enc.output_encoding() != enc {
                return Err(Error::runtime(format!(
                    "lur.charset.encode: cannot encode to {}",
                    enc.name()
                )));
            }
            // Unmappable characters become `&#NNN;` references.
            let (bytes, _, _) = enc.encode(&text);
            lua.create_string(&*bytes)
        })
        .map_err(RunError::Init)?;
    charset.set("encode", encode).map_err(RunError::Init)?;

    lur.set("charset", charset).map_err(RunError::Init)?;
    Ok(())
}

/// Decodes `bytes` to text: a BOM wins, then `enc` (the already-resolved
/// header/label hint), then a `<meta charset>` in the first kilobyte, then
/// UTF-8. Invalid sequences become U+FFFD. Shared by `lur.charset.decode` and
/// `res:text()` in `lur.http`.
pub(crate) fn decode_bytes(bytes: &[u8], enc: Option<&'static Encoding>) -> String {
    let enc = enc.unwrap_or_else(|| sniff_meta(bytes).unwrap_or(UTF_8));
    let (text, _, _) = enc.decode(bytes);
    text.into_owned()
}

/// Like [`decode_bytes`] for a `Content-Type` header value. An unknown or
/// absent charset falls back to sniffing instead of raising (a server's
/// header is not the script's mistake).
pub(crate) fn decode_content_type(bytes: &[u8], content_type: Option<&str>) -> String {
    let enc = content_type
        .and_then(label_of)
        .and_then(|l| Encoding::for_label(l.trim().as_bytes()));
    decode_bytes(bytes, enc)
}

fn lookup(fname: &str, label: &str) -> mlua::Result<&'static Encoding> {
    Encoding::for_label(label.trim().as_bytes())
        .ok_or_else(|| Error::runtime(format!("{fname}: unknown charset {label:?}")))
}

/// The charset named by a bare label (`"big5"`) or a `Content-Type` value
/// (`"text/html; charset=GBK"`). `None` for a Content-Type with no charset,
/// so the caller falls back to sniffing the document.
fn label_of(hint: &str) -> Option<&str> {
    let hint = hint.trim();
    if hint.is_empty() {
        return None;
    }
    if let Some(i) = hint.to_ascii_lowercase().find("charset=") {
        let rest = hint[i + "charset=".len()..].trim_start();
        let rest = rest.trim_start_matches(['"', '\'']);
        let end = rest
            .find(|c: char| c == ';' || c == '"' || c == '\'' || c.is_whitespace())
            .unwrap_or(rest.len());
        let label = &rest[..end];
        return (!label.is_empty()).then_some(label);
    }
    // A media type such as `text/html` names no charset.
    (!hint.contains('/')).then_some(hint)
}

/// `<meta charset=…>` / `<meta http-equiv … content="…; charset=…">` in the
/// first kilobyte. A UTF-16 declaration is ignored: the document was readable
/// as ASCII to find it, so it can't be UTF-16 (same rule as browsers).
fn sniff_meta(bytes: &[u8]) -> Option<&'static Encoding> {
    let head = String::from_utf8_lossy(&bytes[..bytes.len().min(SNIFF_BYTES)]).to_ascii_lowercase();
    let mut from = 0;
    while let Some(i) = head[from..].find("charset") {
        let after = head[from + i + "charset".len()..].trim_start();
        from += i + "charset".len();
        let Some(after) = after.strip_prefix('=') else {
            continue;
        };
        let after = after.trim_start().trim_start_matches(['"', '\'']);
        let end = after
            .find(|c: char| !(c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | ':' | '.')))
            .unwrap_or(after.len());
        if let Some(enc) = Encoding::for_label(&after.as_bytes()[..end]) {
            return Some(if enc.output_encoding() == enc {
                enc
            } else {
                UTF_8
            });
        }
    }
    None
}
