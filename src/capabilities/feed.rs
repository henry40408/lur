//! `lur.feed` — serialize RSS 2.0, Atom 1.0 and JSON Feed 1.1 from plain tables.
//! Dates are epoch milliseconds (as everywhere in `lur`). XML text is escaped and
//! characters illegal in XML 1.0 are dropped, since scraped content often has them.

use std::fmt::Write as _;

use chrono::{DateTime, Utc};
use mlua::{Error, Lua, Table, Value};
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

/// Escape for XML text and attribute values; drop characters XML 1.0 forbids.
fn esc(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            '\t' | '\n' | '\r' => out.push(c),
            c if c < ' ' || c == '\u{FFFE}' || c == '\u{FFFF}' => {}
            c => out.push(c),
        }
    }
    out
}

/// `<tag>text</tag>\n` at the given indent, skipped when `value` is `None`.
fn elem(out: &mut String, indent: usize, tag: &str, value: Option<&str>) {
    if let Some(v) = value {
        let _ = writeln!(out, "{:indent$}<{tag}>{}</{tag}>", "", esc(v));
    }
}

fn render_rss(meta: &Meta, items: &[Item]) -> mlua::Result<String> {
    let link = meta
        .link
        .as_deref()
        .ok_or_else(|| Error::runtime("meta.link is required"))?;
    let mut o = String::from(concat!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n",
        "<rss version=\"2.0\" xmlns:atom=\"http://www.w3.org/2005/Atom\" ",
        "xmlns:content=\"http://purl.org/rss/1.0/modules/content/\" ",
        "xmlns:dc=\"http://purl.org/dc/elements/1.1/\">\n<channel>\n"
    ));
    elem(&mut o, 2, "title", Some(&meta.title));
    elem(&mut o, 2, "link", Some(link));
    elem(
        &mut o,
        2,
        "description",
        Some(meta.description.as_deref().unwrap_or(&meta.title)),
    );
    elem(&mut o, 2, "language", meta.language.as_deref());
    if let Some(ms) = meta
        .updated
        .or_else(|| items.iter().filter_map(|i| i.date).max())
    {
        elem(&mut o, 2, "lastBuildDate", Some(&rfc2822(ms)));
    }
    if let Some(url) = &meta.feed_url {
        let _ = writeln!(
            o,
            "  <atom:link href=\"{}\" rel=\"self\" type=\"application/rss+xml\"/>",
            esc(url)
        );
    }
    for item in items {
        o.push_str("  <item>\n");
        elem(&mut o, 4, "title", Some(&item.title));
        elem(&mut o, 4, "link", item.link.as_deref());
        if let Some(guid) = item.guid.as_deref().or(item.link.as_deref()) {
            let permalink = item.guid.is_none() || item.guid == item.link;
            let _ = writeln!(
                o,
                "    <guid isPermaLink=\"{permalink}\">{}</guid>",
                esc(guid)
            );
        }
        if let Some(ms) = item.date {
            elem(&mut o, 4, "pubDate", Some(&rfc2822(ms)));
        }
        elem(&mut o, 4, "dc:creator", item.author.as_deref());
        for c in &item.categories {
            elem(&mut o, 4, "category", Some(c));
        }
        elem(
            &mut o,
            4,
            "description",
            item.summary.as_deref().or(item.content.as_deref()),
        );
        if item.summary.is_some() {
            elem(&mut o, 4, "content:encoded", item.content.as_deref());
        }
        if let Some(e) = &item.enclosure {
            let _ = writeln!(
                o,
                "    <enclosure url=\"{}\" length=\"{}\" type=\"{}\"/>",
                esc(&e.url),
                e.length.unwrap_or(0),
                esc(e.mime.as_deref().unwrap_or("application/octet-stream"))
            );
        }
        o.push_str("  </item>\n");
    }
    o.push_str("</channel>\n</rss>\n");
    Ok(o)
}

fn render_atom(meta: &Meta, items: &[Item]) -> mlua::Result<String> {
    let id = meta
        .id
        .as_deref()
        .or(meta.link.as_deref())
        .ok_or_else(|| Error::runtime("meta.id or meta.link is required"))?;
    let feed_updated = meta
        .updated
        .or_else(|| items.iter().filter_map(|i| i.updated.or(i.date)).max())
        .unwrap_or(0);
    let lang = meta
        .language
        .as_deref()
        .map(|l| format!(" xml:lang=\"{}\"", esc(l)))
        .unwrap_or_default();
    let mut o = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<feed xmlns=\"http://www.w3.org/2005/Atom\"{lang}>\n"
    );
    elem(&mut o, 2, "title", Some(&meta.title));
    elem(&mut o, 2, "id", Some(id));
    elem(&mut o, 2, "updated", Some(&rfc3339(feed_updated)));
    if let Some(l) = &meta.link {
        let _ = writeln!(o, "  <link rel=\"alternate\" href=\"{}\"/>", esc(l));
    }
    if let Some(l) = &meta.feed_url {
        let _ = writeln!(o, "  <link rel=\"self\" href=\"{}\"/>", esc(l));
    }
    elem(&mut o, 2, "subtitle", meta.description.as_deref());
    for (i, item) in items.iter().enumerate() {
        let entry_id = item
            .guid
            .as_deref()
            .or(item.link.as_deref())
            .ok_or_else(|| Error::runtime(format!("items[{}] needs guid or link", i + 1)))?;
        o.push_str("  <entry>\n");
        elem(&mut o, 4, "title", Some(&item.title));
        elem(&mut o, 4, "id", Some(entry_id));
        let updated = item.updated.or(item.date).unwrap_or(feed_updated);
        elem(&mut o, 4, "updated", Some(&rfc3339(updated)));
        if let Some(ms) = item.date {
            elem(&mut o, 4, "published", Some(&rfc3339(ms)));
        }
        if let Some(l) = &item.link {
            let _ = writeln!(o, "    <link rel=\"alternate\" href=\"{}\"/>", esc(l));
        }
        if let Some(a) = &item.author {
            let _ = writeln!(o, "    <author><name>{}</name></author>", esc(a));
        }
        for c in &item.categories {
            let _ = writeln!(o, "    <category term=\"{}\"/>", esc(c));
        }
        if let Some(s) = &item.summary {
            let _ = writeln!(o, "    <summary type=\"html\">{}</summary>", esc(s));
        }
        if let Some(c) = &item.content {
            let _ = writeln!(o, "    <content type=\"html\">{}</content>", esc(c));
        }
        if let Some(e) = &item.enclosure {
            let mut attrs = format!("rel=\"enclosure\" href=\"{}\"", esc(&e.url));
            if let Some(m) = &e.mime {
                let _ = write!(attrs, " type=\"{}\"", esc(m));
            }
            if let Some(n) = e.length {
                let _ = write!(attrs, " length=\"{n}\"");
            }
            let _ = writeln!(o, "    <link {attrs}/>");
        }
        o.push_str("  </entry>\n");
    }
    o.push_str("</feed>\n");
    Ok(o)
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
    use super::esc;

    #[test]
    fn esc_escapes_markup_and_drops_illegal_controls() {
        assert_eq!(
            esc("a<b>&\"'\u{1}\u{b}\tz"),
            "a&lt;b&gt;&amp;&quot;&apos;\tz"
        );
    }
}
