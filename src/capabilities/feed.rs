//! `lur.feed` — serialize RSS 2.0, Atom 1.0 and JSON Feed 1.1 from plain tables.
//! Dates are epoch milliseconds (as everywhere in `lur`). XML text is escaped and
//! characters illegal in XML 1.0 are dropped, since scraped content often has them.

use std::borrow::Cow;

use chrono::{DateTime, Utc};
use mlua::{Error, Lua, Table, Value};
use quick_xml::Writer;
use quick_xml::events::{BytesDecl, BytesText, Event};
use serde_json::{Map, json};

use crate::capabilities::argcheck;
use crate::runtime::RunError;

struct Meta {
    title: String,
    link: Option<String>,
    feed_url: Option<String>,
    description: Option<String>,
    language: Option<String>,
    id: Option<String>,
    updated: Option<i64>,
}

struct Enclosure {
    url: String,
    mime: Option<String>,
    length: Option<i64>,
}

struct Item {
    title: String,
    link: Option<String>,
    guid: Option<String>,
    date: Option<i64>,
    updated: Option<i64>,
    summary: Option<String>,
    content: Option<String>,
    author: Option<String>,
    categories: Vec<String>,
    enclosure: Option<Enclosure>,
}

pub fn install(lua: &Lua, lur: &Table) -> Result<(), RunError> {
    let feed = lua.create_table().map_err(RunError::Init)?;
    install_format(lua, &feed, "rss", render_rss)?;
    install_format(lua, &feed, "atom", render_atom)?;
    install_format(lua, &feed, "json", render_json)?;
    lur.set("feed", feed).map_err(RunError::Init)?;
    Ok(())
}

type Render = fn(&Meta, &[Item]) -> mlua::Result<String>;

fn install_format(
    lua: &Lua,
    feed: &Table,
    name: &'static str,
    render: Render,
) -> Result<(), RunError> {
    let f = lua
        .create_function(move |lua, (meta, items): (Value, Value)| {
            let fname = format!("lur.feed.{name}");
            let meta: Table = argcheck::arg(lua, meta, &fname, 1, "table")?;
            let items: Table = argcheck::arg(lua, items, &fname, 2, "table")?;
            let meta = read_meta(&meta, &fname)?;
            let items = read_items(&items, &fname)?;
            render(&meta, &items).map_err(|e| Error::runtime(format!("lur.feed.{name}: {e}")))
        })
        .map_err(RunError::Init)?;
    feed.set(name, f).map_err(RunError::Init)?;
    Ok(())
}

// ---- reading Lua tables ----

fn opt_str(t: &Table, key: &str, ctx: &str) -> mlua::Result<Option<String>> {
    match t.get::<Value>(key)? {
        Value::Nil => Ok(None),
        Value::String(s) => s
            .to_str()
            .map(|s| Some(s.to_string()))
            .map_err(|e| Error::runtime(format!("{ctx}.{key}: {e}"))),
        other => Err(Error::runtime(format!(
            "{ctx}.{key} must be string, got {}",
            other.type_name()
        ))),
    }
}

fn req_str(t: &Table, key: &str, ctx: &str) -> mlua::Result<String> {
    opt_str(t, key, ctx)?.ok_or_else(|| Error::runtime(format!("{ctx}.{key} is required")))
}

fn opt_ms(t: &Table, key: &str, ctx: &str) -> mlua::Result<Option<i64>> {
    let ms = argcheck::integer_arg(t.get::<Value>(key)?, &format!("{ctx}.{key}"), 1)
        .map_err(|e| Error::runtime(e.to_string().replace("argument #1 ", "")))?;
    if let Some(ms) = ms {
        timestamp(ms).map_err(|e| Error::runtime(format!("{ctx}.{key}: {e}")))?;
    }
    Ok(ms)
}

fn read_meta(t: &Table, fname: &str) -> mlua::Result<Meta> {
    let ctx = format!("{fname}: meta");
    Ok(Meta {
        title: req_str(t, "title", &ctx)?,
        link: opt_str(t, "link", &ctx)?,
        feed_url: opt_str(t, "feed_url", &ctx)?,
        description: opt_str(t, "description", &ctx)?,
        language: opt_str(t, "language", &ctx)?,
        id: opt_str(t, "id", &ctx)?,
        updated: opt_ms(t, "updated", &ctx)?,
    })
}

fn read_items(t: &Table, fname: &str) -> mlua::Result<Vec<Item>> {
    let mut items = Vec::new();
    for (i, value) in t.sequence_values::<Value>().enumerate() {
        let n = i + 1;
        let ctx = format!("{fname}: items[{n}]");
        let Value::Table(item) = value? else {
            return Err(Error::runtime(format!("{ctx} must be a table")));
        };
        items.push(read_item(&item, &ctx)?);
    }
    Ok(items)
}

fn read_item(t: &Table, ctx: &str) -> mlua::Result<Item> {
    let mut categories = Vec::new();
    match t.get::<Value>("categories")? {
        Value::Nil => {}
        Value::Table(list) => {
            for c in list.sequence_values::<Value>() {
                match c? {
                    Value::String(s) => categories.push(
                        s.to_str()
                            .map_err(|e| Error::runtime(format!("{ctx}.categories: {e}")))?
                            .to_string(),
                    ),
                    other => {
                        return Err(Error::runtime(format!(
                            "{ctx}.categories must contain strings, got {}",
                            other.type_name()
                        )));
                    }
                }
            }
        }
        other => {
            return Err(Error::runtime(format!(
                "{ctx}.categories must be table, got {}",
                other.type_name()
            )));
        }
    }

    let enclosure = match t.get::<Value>("enclosure")? {
        Value::Nil => None,
        Value::Table(e) => {
            let ectx = format!("{ctx}.enclosure");
            Some(Enclosure {
                url: req_str(&e, "url", &ectx)?,
                mime: opt_str(&e, "type", &ectx)?,
                length: argcheck::integer_arg(
                    e.get::<Value>("length")?,
                    &format!("{ectx}.length"),
                    1,
                )
                .map_err(|err| Error::runtime(err.to_string().replace("argument #1 ", "")))?,
            })
        }
        other => {
            return Err(Error::runtime(format!(
                "{ctx}.enclosure must be table, got {}",
                other.type_name()
            )));
        }
    };

    Ok(Item {
        title: req_str(t, "title", ctx)?,
        link: opt_str(t, "link", ctx)?,
        guid: opt_str(t, "guid", ctx)?,
        date: opt_ms(t, "date", ctx)?,
        updated: opt_ms(t, "updated", ctx)?,
        summary: opt_str(t, "summary", ctx)?,
        content: opt_str(t, "content", ctx)?,
        author: opt_str(t, "author", ctx)?,
        categories,
        enclosure,
    })
}

// ---- writing ----

fn timestamp(ms: i64) -> Result<DateTime<Utc>, String> {
    DateTime::from_timestamp_millis(ms).ok_or_else(|| "timestamp out of range".to_owned())
}

fn rfc3339(ms: i64) -> String {
    timestamp(ms)
        .map(|d| d.to_rfc3339_opts(chrono::SecondsFormat::Secs, true))
        .unwrap_or_default()
}

fn rfc2822(ms: i64) -> String {
    timestamp(ms).map(|d| d.to_rfc2822()).unwrap_or_default()
}

/// Drop characters XML 1.0 forbids; scraped content often has them. quick-xml
/// escapes the rest.
fn clean(s: &str) -> Cow<'_, str> {
    let illegal = |c: char| {
        (c < ' ' && !matches!(c, '\t' | '\n' | '\r')) || c == '\u{FFFE}' || c == '\u{FFFF}'
    };
    if s.contains(illegal) {
        Cow::Owned(s.chars().filter(|&c| !illegal(c)).collect())
    } else {
        Cow::Borrowed(s)
    }
}

type XmlWriter = Writer<Vec<u8>>;

fn xml_err(e: &std::io::Error) -> Error {
    Error::runtime(format!("xml write failed: {e}"))
}

fn new_writer() -> std::io::Result<XmlWriter> {
    let mut w = Writer::new_with_indent(Vec::new(), b' ', 2);
    w.write_event(Event::Decl(BytesDecl::new("1.0", Some("UTF-8"), None)))?;
    Ok(w)
}

fn finish(w: XmlWriter) -> mlua::Result<String> {
    let mut out = String::from_utf8(w.into_inner())
        .map_err(|e| Error::runtime(format!("xml output is not UTF-8: {e}")))?;
    out.push('\n');
    Ok(out)
}

/// `<tag>text</tag>`, skipped when `value` is `None`.
fn text_el(w: &mut XmlWriter, tag: &str, value: Option<&str>) -> std::io::Result<()> {
    if let Some(v) = value {
        w.create_element(tag)
            .write_text_content(BytesText::new(&clean(v)))?;
    }
    Ok(())
}

/// `<tag a="1" .../>`; attribute values are escaped by quick-xml.
fn empty_el(w: &mut XmlWriter, tag: &str, attrs: &[(&str, &str)]) -> std::io::Result<()> {
    let attrs: Vec<(&str, Cow<'_, str>)> = attrs.iter().map(|&(k, v)| (k, clean(v))).collect();
    w.create_element(tag)
        .with_attributes(attrs.iter().map(|(k, v)| (*k, v.as_ref())))
        .write_empty()?;
    Ok(())
}

fn render_rss(meta: &Meta, items: &[Item]) -> mlua::Result<String> {
    let link = meta
        .link
        .as_deref()
        .ok_or_else(|| Error::runtime("meta.link is required"))?;
    write_rss(meta, link, items)
        .map_err(|e| xml_err(&e))
        .and_then(finish)
}

fn write_rss(meta: &Meta, link: &str, items: &[Item]) -> std::io::Result<XmlWriter> {
    let mut w = new_writer()?;
    w.create_element("rss")
        .with_attributes([
            ("version", "2.0"),
            ("xmlns:atom", "http://www.w3.org/2005/Atom"),
            ("xmlns:content", "http://purl.org/rss/1.0/modules/content/"),
            ("xmlns:dc", "http://purl.org/dc/elements/1.1/"),
        ])
        .write_inner_content(|w| {
            w.create_element("channel").write_inner_content(|w| {
                text_el(w, "title", Some(&meta.title))?;
                text_el(w, "link", Some(link))?;
                text_el(
                    w,
                    "description",
                    Some(meta.description.as_deref().unwrap_or(&meta.title)),
                )?;
                text_el(w, "language", meta.language.as_deref())?;
                if let Some(ms) = meta
                    .updated
                    .or_else(|| items.iter().filter_map(|i| i.date).max())
                {
                    text_el(w, "lastBuildDate", Some(&rfc2822(ms)))?;
                }
                if let Some(url) = &meta.feed_url {
                    empty_el(
                        w,
                        "atom:link",
                        &[
                            ("href", url),
                            ("rel", "self"),
                            ("type", "application/rss+xml"),
                        ],
                    )?;
                }
                for item in items {
                    w.create_element("item")
                        .write_inner_content(|w| write_rss_item(w, item))?;
                }
                Ok(())
            })?;
            Ok(())
        })?;
    Ok(w)
}

fn write_rss_item(w: &mut XmlWriter, item: &Item) -> std::io::Result<()> {
    text_el(w, "title", Some(&item.title))?;
    text_el(w, "link", item.link.as_deref())?;
    if let Some(guid) = item.guid.as_deref().or(item.link.as_deref()) {
        let permalink = if item.guid.is_none() || item.guid == item.link {
            "true"
        } else {
            "false"
        };
        w.create_element("guid")
            .with_attribute(("isPermaLink", permalink))
            .write_text_content(BytesText::new(&clean(guid)))?;
    }
    if let Some(ms) = item.date {
        text_el(w, "pubDate", Some(&rfc2822(ms)))?;
    }
    text_el(w, "dc:creator", item.author.as_deref())?;
    for c in &item.categories {
        text_el(w, "category", Some(c))?;
    }
    text_el(
        w,
        "description",
        item.summary.as_deref().or(item.content.as_deref()),
    )?;
    if item.summary.is_some() {
        text_el(w, "content:encoded", item.content.as_deref())?;
    }
    if let Some(e) = &item.enclosure {
        let length = e.length.unwrap_or(0).to_string();
        empty_el(
            w,
            "enclosure",
            &[
                ("url", &e.url),
                ("length", &length),
                (
                    "type",
                    e.mime.as_deref().unwrap_or("application/octet-stream"),
                ),
            ],
        )?;
    }
    Ok(())
}

fn render_atom(meta: &Meta, items: &[Item]) -> mlua::Result<String> {
    let id = meta
        .id
        .as_deref()
        .or(meta.link.as_deref())
        .ok_or_else(|| Error::runtime("meta.id or meta.link is required"))?;
    for (i, item) in items.iter().enumerate() {
        if item.guid.is_none() && item.link.is_none() {
            return Err(Error::runtime(format!(
                "items[{}] needs guid or link",
                i + 1
            )));
        }
    }
    write_atom(meta, id, items)
        .map_err(|e| xml_err(&e))
        .and_then(finish)
}

fn write_atom(meta: &Meta, id: &str, items: &[Item]) -> std::io::Result<XmlWriter> {
    let feed_updated = meta
        .updated
        .or_else(|| items.iter().filter_map(|i| i.updated.or(i.date)).max())
        .unwrap_or(0);
    let mut w = new_writer()?;
    let mut root = w
        .create_element("feed")
        .with_attribute(("xmlns", "http://www.w3.org/2005/Atom"));
    let lang = meta.language.as_deref().map(clean);
    if let Some(l) = &lang {
        root = root.with_attribute(("xml:lang", l.as_ref()));
    }
    root.write_inner_content(|w| {
        text_el(w, "title", Some(&meta.title))?;
        text_el(w, "id", Some(id))?;
        text_el(w, "updated", Some(&rfc3339(feed_updated)))?;
        if let Some(l) = &meta.link {
            empty_el(w, "link", &[("rel", "alternate"), ("href", l)])?;
        }
        if let Some(l) = &meta.feed_url {
            empty_el(w, "link", &[("rel", "self"), ("href", l)])?;
        }
        text_el(w, "subtitle", meta.description.as_deref())?;
        for item in items {
            w.create_element("entry")
                .write_inner_content(|w| write_atom_entry(w, item, feed_updated))?;
        }
        Ok(())
    })?;
    Ok(w)
}

fn write_atom_entry(w: &mut XmlWriter, item: &Item, feed_updated: i64) -> std::io::Result<()> {
    // `render_atom` has already checked that `guid` or `link` is present.
    let entry_id = item.guid.as_deref().or(item.link.as_deref());
    text_el(w, "title", Some(&item.title))?;
    text_el(w, "id", entry_id)?;
    let updated = item.updated.or(item.date).unwrap_or(feed_updated);
    text_el(w, "updated", Some(&rfc3339(updated)))?;
    if let Some(ms) = item.date {
        text_el(w, "published", Some(&rfc3339(ms)))?;
    }
    if let Some(l) = &item.link {
        empty_el(w, "link", &[("rel", "alternate"), ("href", l)])?;
    }
    if let Some(a) = &item.author {
        w.create_element("author")
            .write_inner_content(|w| text_el(w, "name", Some(a)))?;
    }
    for c in &item.categories {
        empty_el(w, "category", &[("term", c)])?;
    }
    for (tag, value) in [("summary", &item.summary), ("content", &item.content)] {
        if let Some(v) = value {
            w.create_element(tag)
                .with_attribute(("type", "html"))
                .write_text_content(BytesText::new(&clean(v)))?;
        }
    }
    if let Some(e) = &item.enclosure {
        let length = e.length.map(|n| n.to_string());
        let mut attrs = vec![("rel", "enclosure"), ("href", e.url.as_str())];
        if let Some(m) = &e.mime {
            attrs.push(("type", m));
        }
        if let Some(n) = &length {
            attrs.push(("length", n));
        }
        empty_el(w, "link", &attrs)?;
    }
    Ok(())
}

fn render_json(meta: &Meta, items: &[Item]) -> mlua::Result<String> {
    let mut feed = Map::new();
    feed.insert("version".into(), json!("https://jsonfeed.org/version/1.1"));
    feed.insert("title".into(), json!(meta.title));
    for (key, value) in [
        ("home_page_url", &meta.link),
        ("feed_url", &meta.feed_url),
        ("description", &meta.description),
        ("language", &meta.language),
    ] {
        if let Some(v) = value {
            feed.insert(key.into(), json!(v));
        }
    }
    let mut out_items = Vec::with_capacity(items.len());
    for (i, item) in items.iter().enumerate() {
        let id = item
            .guid
            .as_deref()
            .or(item.link.as_deref())
            .ok_or_else(|| Error::runtime(format!("items[{}] needs guid or link", i + 1)))?;
        let mut o = Map::new();
        o.insert("id".into(), json!(id));
        o.insert("title".into(), json!(item.title));
        if let Some(v) = &item.link {
            o.insert("url".into(), json!(v));
        }
        if let Some(v) = &item.content {
            o.insert("content_html".into(), json!(v));
        }
        if let Some(v) = &item.summary {
            o.insert("summary".into(), json!(v));
        }
        if let Some(ms) = item.date {
            o.insert("date_published".into(), json!(rfc3339(ms)));
        }
        if let Some(ms) = item.updated {
            o.insert("date_modified".into(), json!(rfc3339(ms)));
        }
        if let Some(a) = &item.author {
            o.insert("authors".into(), json!([{ "name": a }]));
        }
        if !item.categories.is_empty() {
            o.insert("tags".into(), json!(item.categories));
        }
        if let Some(e) = &item.enclosure {
            let mut att = Map::new();
            att.insert("url".into(), json!(e.url));
            att.insert(
                "mime_type".into(),
                json!(e.mime.as_deref().unwrap_or("application/octet-stream")),
            );
            if let Some(n) = e.length {
                att.insert("size_in_bytes".into(), json!(n));
            }
            o.insert("attachments".into(), json!([att]));
        }
        out_items.push(serde_json::Value::Object(o));
    }
    feed.insert("items".into(), serde_json::Value::Array(out_items));
    serde_json::to_string(&feed).map_err(|e| Error::runtime(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::clean;

    #[test]
    fn clean_drops_illegal_controls_only() {
        assert_eq!(clean("a<&\u{1}\u{b}\tz\u{FFFE}"), "a<&\tz");
    }
}
