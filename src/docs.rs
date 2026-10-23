//! Render the embedded `GUIDE.md` for `lur docs`: a hand-rolled
//! `pulldown-cmark` sink whose plain mode strips all markup and escapes.

use pulldown_cmark::{Event, Options, Parser, Tag, TagEnd};

const BOLD: &str = "\x1b[1m";
const H1: &str = "\x1b[1;7m"; // reverse video bar
const H3C: &str = "\x1b[1;36m"; // bold cyan (capability headings)
const INLINE: &str = "\x1b[36m"; // inline code (cyan)
const DIM: &str = "\x1b[2m"; // code frame bar
const RESET: &str = "\x1b[0m";

const KW_C: &str = "\x1b[1;36m"; // keyword (bold cyan)
const STR_C: &str = "\x1b[32m"; // string (green)
const COM_C: &str = "\x1b[90m"; // comment (bright black)
const NUM_C: &str = "\x1b[33m"; // number (yellow)

const LUA_KEYWORDS: &[&str] = &[
    "and", "break", "do", "else", "elseif", "end", "false", "for", "function", "goto", "if", "in",
    "local", "nil", "not", "or", "repeat", "return", "then", "true", "until", "while",
];

/// A pragmatic Lua highlighter, not a full lexer: comments/strings are scanned
/// before operators, keywords match on word boundaries, and an unterminated
/// string/comment runs to the end.
fn highlight_lua(code: &str, color: bool) -> String {
    if !color {
        return code.to_string();
    }
    let bytes = code.as_bytes();
    let mut out = String::with_capacity(code.len() + 32);
    let mut i = 0;
    let wrap = |out: &mut String, c: &str, s: &str| {
        out.push_str(c);
        out.push_str(s);
        out.push_str(RESET);
    };
    while i < bytes.len() {
        let rest = &code[i..];
        if rest.starts_with("--") {
            let end = if rest.starts_with("--[[") {
                rest.find("]]").map_or(rest.len(), |p| p + 2)
            } else {
                rest.find('\n').unwrap_or(rest.len())
            };
            wrap(&mut out, COM_C, &rest[..end]);
            i += end;
            continue;
        }
        if rest.starts_with("[[") {
            let end = rest.find("]]").map_or(rest.len(), |p| p + 2);
            wrap(&mut out, STR_C, &rest[..end]);
            i += end;
            continue;
        }
        let c0 = bytes[i];
        if c0 == b'"' || c0 == b'\'' {
            let quote = c0;
            let mut j = i + 1;
            while j < bytes.len() {
                if bytes[j] == b'\\' {
                    j += 2;
                    continue;
                }
                if bytes[j] == quote {
                    j += 1;
                    break;
                }
                j += 1;
            }
            let end = j.min(bytes.len());
            wrap(&mut out, STR_C, &code[i..end]);
            i = end;
            continue;
        }
        if c0.is_ascii_digit() {
            let mut j = i + 1;
            while j < bytes.len()
                && (bytes[j].is_ascii_alphanumeric() || bytes[j] == b'.' || bytes[j] == b'x')
            {
                j += 1;
            }
            wrap(&mut out, NUM_C, &code[i..j]);
            i = j;
            continue;
        }
        if c0.is_ascii_alphabetic() || c0 == b'_' {
            let mut j = i + 1;
            while j < bytes.len() && (bytes[j].is_ascii_alphanumeric() || bytes[j] == b'_') {
                j += 1;
            }
            let word = &code[i..j];
            if LUA_KEYWORDS.contains(&word) {
                wrap(&mut out, KW_C, word);
            } else {
                out.push_str(word);
            }
            i = j;
            continue;
        }
        let ch = rest.chars().next().unwrap();
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}

/// Printed width of `s`, skipping `ESC [ … m` colour sequences. Text between
/// escapes is measured with `unicode-width`, so emoji and ZWJ sequences count
/// as the terminal draws them.
fn visible_len(s: &str) -> usize {
    use unicode_width::UnicodeWidthStr;
    let mut n = 0;
    let mut rest = s;
    while let Some(i) = rest.find('\x1b') {
        n += UnicodeWidthStr::width(&rest[..i]);
        let tail = &rest[i..];
        let end = tail.find('m').map_or(tail.len(), |p| p + 1);
        rest = &tail[end..];
    }
    n + UnicodeWidthStr::width(rest)
}

/// Track the SGR state across `s`: a reset clears it, any other escape appends.
fn track_style(s: &str, active: &mut String) {
    let mut rest = s;
    while let Some(i) = rest.find('\x1b') {
        let tail = &rest[i..];
        let end = tail.find('m').map_or(tail.len(), |p| p + 1);
        if &tail[..end] == RESET {
            active.clear();
        } else {
            active.push_str(&tail[..end]);
        }
        rest = &tail[end..];
    }
}

/// Greedy word wrap of one cell to `width` columns. Colour state is closed at
/// each line end and reopened on the next, so styled spans survive a wrap.
fn wrap_cell(cell: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let mut lines = Vec::new();
    let mut cur = String::new();
    let mut curw = 0;
    let mut active = String::new();
    let flush = |lines: &mut Vec<String>, cur: &mut String, curw: &mut usize, active: &str| {
        let mut line = std::mem::replace(cur, active.to_string());
        if !active.is_empty() {
            line.push_str(RESET);
        }
        lines.push(line);
        *curw = 0;
    };
    for word in cell.split_whitespace() {
        let w = visible_len(word);
        if curw > 0 && curw + 1 + w > width {
            flush(&mut lines, &mut cur, &mut curw, &active);
        }
        if w <= width {
            if curw > 0 {
                cur.push(' ');
                curw += 1;
            }
            cur.push_str(word);
            curw += w;
            track_style(word, &mut active);
            continue;
        }
        // Longer than a whole line: break by character.
        if curw > 0 {
            flush(&mut lines, &mut cur, &mut curw, &active);
        }
        let mut chars = word.chars();
        while let Some(c) = chars.next() {
            if c == '\x1b' {
                let mut seq = String::from(c);
                for c in chars.by_ref() {
                    seq.push(c);
                    if c == 'm' {
                        break;
                    }
                }
                track_style(&seq, &mut active);
                cur.push_str(&seq);
                continue;
            }
            let cw = unicode_width::UnicodeWidthChar::width(c).unwrap_or(0);
            if curw > 0 && curw + cw > width {
                flush(&mut lines, &mut cur, &mut curw, &active);
            }
            cur.push(c);
            curw += cw;
        }
    }
    if curw > 0 || lines.is_empty() {
        lines.push(cur);
    }
    lines
}

/// Smallest width a column is shrunk to when fitting a table to the terminal.
const MIN_COL: usize = 6;

/// Column-aligned table: bold header, dim rule, two-space gutters, last column
/// unpadded. Every line starts with `prefix` (indent and/or blockquote bar,
/// `prefix_width` columns wide). With `max_width`, cells wrap so the table fits.
fn render_table(
    rows: &[Vec<String>],
    prefix: &str,
    prefix_width: usize,
    max_width: Option<usize>,
    color: bool,
) -> String {
    let cols = rows.iter().map(Vec::len).max().unwrap_or(0);
    let mut widths = vec![0; cols];
    for row in rows {
        for (i, cell) in row.iter().enumerate() {
            widths[i] = widths[i].max(visible_len(cell));
        }
    }
    let gutters = 2 * cols.saturating_sub(1);
    if let Some(max) = max_width {
        let avail = max.saturating_sub(prefix_width);
        while widths.iter().sum::<usize>() + gutters > avail {
            let (i, &w) = widths
                .iter()
                .enumerate()
                .max_by_key(|&(i, &w)| (w, std::cmp::Reverse(i)))
                .unwrap();
            if w <= MIN_COL {
                break;
            }
            widths[i] -= 1;
        }
    }
    let mut out = String::new();
    for (r, row) in rows.iter().enumerate() {
        let wrapped: Vec<Vec<String>> = row
            .iter()
            .enumerate()
            .map(|(i, cell)| {
                if max_width.is_some() && visible_len(cell) > widths[i] {
                    wrap_cell(cell, widths[i])
                } else {
                    vec![cell.clone()]
                }
            })
            .collect();
        let height = wrapped.iter().map(Vec::len).max().unwrap_or(1);
        for l in 0..height {
            let start = out.len();
            out.push_str(prefix);
            let prefix_end = out.len();
            for (i, lines) in wrapped.iter().enumerate() {
                let cell = lines.get(l).map_or("", String::as_str);
                if r == 0 && color && !cell.is_empty() {
                    out.push_str(BOLD);
                    out.push_str(cell);
                    out.push_str(RESET);
                } else {
                    out.push_str(cell);
                }
                if i + 1 < wrapped.len() {
                    let fill = (widths[i] + 2).saturating_sub(visible_len(cell));
                    out.push_str(&" ".repeat(fill));
                }
            }
            let trimmed = out.trim_end_matches(' ').len().max(prefix_end);
            out.truncate(trimmed);
            if out.len() == prefix_end {
                out.truncate(start + prefix.trim_end_matches(' ').len());
            }
            out.push('\n');
        }
        if r == 0 {
            let total = widths.iter().sum::<usize>() + gutters;
            out.push_str(prefix);
            if color {
                out.push_str(DIM);
            }
            out.push_str(&"\u{2500}".repeat(total));
            if color {
                out.push_str(RESET);
            }
            out.push('\n');
        }
    }
    out
}

/// Terminal width for `lur docs`: `None` (no wrapping) unless stdout is a TTY,
/// then `COLUMNS` if set to a positive number, else the `TIOCGWINSZ` width
/// (unix), else 80.
pub fn terminal_width() -> Option<usize> {
    use std::io::IsTerminal;
    if !std::io::stdout().is_terminal() {
        return None;
    }
    let cols = std::env::var("COLUMNS")
        .ok()
        .and_then(|v| v.trim().parse::<usize>().ok())
        .filter(|&n| n > 0);
    #[cfg(unix)]
    let cols = cols.or_else(|| {
        rustix::termios::tcgetwinsize(std::io::stdout())
            .ok()
            .map(|ws| usize::from(ws.ws_col))
            .filter(|&n| n > 0)
    });
    Some(cols.unwrap_or(80))
}

/// Open container while rendering: blockquotes and lists nest in any order, and
/// the line prefix is the composition of the whole stack.
#[derive(Clone, Copy, PartialEq)]
enum Ctx {
    Quote,
    List,
}

/// Inputs of [`compose_prefix`].
#[derive(Clone, Copy)]
struct PrefixSpec<'a> {
    stack: &'a [Ctx],
    from: usize,
    margin: usize,
    color: bool,
    bullet: bool,
}

/// Line prefix for a block inside a blockquote: `margin`, then one bar per
/// quote and two spaces per list, outermost first. Entries before `from` are
/// skipped (already on the line); `bullet` swaps the innermost list's indent
/// for the `- ` marker.
fn compose_prefix(spec: &PrefixSpec) -> String {
    let PrefixSpec {
        stack,
        from,
        margin,
        color,
        bullet,
    } = *spec;
    let mut s = String::new();
    if from == 0 {
        s.push_str(&" ".repeat(margin));
    }
    for (i, c) in stack.iter().enumerate().skip(from) {
        match c {
            Ctx::Quote => {
                if color {
                    s.push_str(DIM);
                }
                s.push('\u{2502}');
                if color {
                    s.push_str(RESET);
                }
                s.push(' ');
            }
            Ctx::List if bullet && i + 1 == stack.len() => s.push_str("- "),
            Ctx::List => s.push_str("  "),
        }
    }
    s
}

/// Where an item marker was just written: `(out.len(), index of its list)`.
type OpenMarker = Option<(usize, usize)>;

/// Glamour-style terminal rendering. Without `color`, the structure (heading
/// `#` prefixes, `│ ` code frame, indentation) stays but no escapes are emitted.
pub fn render(markdown: &str, color: bool) -> String {
    render_width(markdown, color, None)
}

/// Like [`render`], wrapping table cells so tables fit `width` columns.
pub fn render_width(markdown: &str, color: bool, width: Option<usize>) -> String {
    let mut renderer = Renderer::new(color, width);
    for ev in Parser::new_ext(markdown, Options::ENABLE_TABLES) {
        renderer.handle(ev);
    }
    format!("{}\n", renderer.out.trim())
}

/// The output so far plus the state the event handlers share.
struct Renderer {
    color: bool,
    width: Option<usize>,
    out: String,
    list_depth: usize,
    /// Left indent of the current section's body.
    margin: usize,
    in_code: bool,
    /// 0 = not in a heading.
    heading_level: usize,

    /// Open blockquotes and lists, outermost first. Outside any quote the
    /// original list-only layout rules apply; inside one, every block is
    /// prefixed with the composed stack.
    stack: Vec<Ctx>,
    quotes: usize,
    /// No block written yet in the innermost quote/item (no separator needed).
    first: bool,
    open: OpenMarker,
    /// Next code line is the block's first.
    code_fresh: bool,
    /// `out.len()` right after a paragraph.
    para_end: usize,
    /// The last finished item ended with a paragraph.
    prev_loose: bool,

    /// Table state: finished rows, the row being built, and the output buffer
    /// parked while a cell collects its text.
    rows: Vec<Vec<String>>,
    row: Vec<String>,
    parked: String,
    in_cell: bool,
    /// Inline style open on the terminal; the quote prefix's RESET cancels it
    /// at a line break, so it is re-emitted after the prefix.
    bold_open: bool,
}

impl Renderer {
    fn new(color: bool, width: Option<usize>) -> Self {
        Self {
            color,
            width,
            out: String::new(),
            list_depth: 0,
            margin: 0,
            in_code: false,
            heading_level: 0,
            stack: Vec::new(),
            quotes: 0,
            first: true,
            open: None,
            code_fresh: false,
            para_end: usize::MAX,
            prev_loose: false,
            rows: Vec::new(),
            row: Vec::new(),
            parked: String::new(),
            in_cell: false,
            bold_open: false,
        }
    }

    fn handle(&mut self, ev: Event) {
        match ev {
            Event::Start(Tag::Table(_)) => self.start_table(),
            Event::Start(Tag::TableCell) => {
                self.in_cell = true;
                self.parked = std::mem::take(&mut self.out);
            }
            Event::End(TagEnd::TableCell) => {
                self.in_cell = false;
                let cell = std::mem::replace(&mut self.out, std::mem::take(&mut self.parked));
                self.row.push(cell);
            }
            Event::End(TagEnd::TableHead | TagEnd::TableRow) => {
                self.rows.push(std::mem::take(&mut self.row));
            }
            Event::End(TagEnd::Table) => self.end_table(),
            Event::Start(Tag::Heading { level, .. }) => self.start_heading(level as usize),
            Event::End(TagEnd::Heading(_)) => self.end_heading(),
            Event::Start(Tag::Paragraph) => self.start_paragraph(),
            Event::Start(Tag::Strong | Tag::Emphasis) => {
                self.bold_open = true;
                self.push_style(BOLD);
            }
            Event::End(TagEnd::Strong | TagEnd::Emphasis) => {
                self.bold_open = false;
                self.push_style(RESET);
            }
            Event::Start(Tag::List(_)) => self.start_list(),
            Event::End(TagEnd::List(_)) => self.end_list(),
            Event::Start(Tag::Item) => self.start_item(),
            Event::Start(Tag::BlockQuote(_)) => self.start_blockquote(),
            Event::Start(Tag::CodeBlock(_)) => self.start_code_block(),
            Event::End(TagEnd::CodeBlock) => {
                self.in_code = false;
                self.out.push('\n');
            }
            Event::Code(text) => {
                self.push_style(INLINE);
                self.out.push_str(&text);
                self.push_style(RESET);
            }
            Event::Text(text) => self.text(&text),
            Event::End(TagEnd::Paragraph) => {
                self.out.push('\n');
                self.para_end = self.out.len();
            }
            Event::End(TagEnd::Item) => self.end_item(),
            Event::End(TagEnd::BlockQuote(_)) => self.end_blockquote(),
            Event::SoftBreak | Event::HardBreak => self.line_break(),
            Event::Rule => self.rule(),
            _ => {}
        }
    }

    /// `s` when colour is on, nothing otherwise.
    fn style(&self, s: &'static str) -> &'static str {
        if self.color { s } else { "" }
    }

    fn push_style(&mut self, s: &'static str) {
        self.out.push_str(self.style(s));
    }

    /// An item marker was just written and nothing has followed it yet.
    fn marker_is_open(&self) -> bool {
        self.open.is_some_and(|(len, _)| len == self.out.len())
    }

    /// Finish the line being written, if any.
    fn end_line(&mut self) {
        if !self.out.is_empty() && !self.out.ends_with('\n') {
            self.out.push('\n');
        }
    }

    fn prefix_from(&self, from: usize, bullet: bool) -> String {
        compose_prefix(&PrefixSpec {
            stack: &self.stack,
            from,
            margin: self.margin,
            color: self.color,
            bullet,
        })
    }

    /// Terminal width of the full quote prefix.
    fn prefix_cols(&self) -> usize {
        self.margin + 2 * self.stack.len()
    }

    /// Bar-only line separating two blocks of the same quote.
    fn quote_separator(&self) -> String {
        let mut s = self.prefix_from(0, false);
        s.truncate(s.trim_end_matches(' ').len());
        s.push('\n');
        s
    }

    fn push_quote_separator(&mut self) {
        let separator = self.quote_separator();
        self.out.push_str(&separator);
    }

    /// Start a block inside a blockquote: finish the previous line, add a
    /// bar-only separator unless this is the container's first block, then
    /// write the prefix (just the part after the marker when the block shares
    /// the marker's line).
    fn start_quote_block(&mut self) {
        let was_first = std::mem::replace(&mut self.first, false);
        if let Some((len, idx)) = self.open
            && len == self.out.len()
        {
            let prefix = self.prefix_from(idx + 1, false);
            self.out.push_str(&prefix);
            return;
        }
        self.end_line();
        if !was_first {
            self.push_quote_separator();
        }
        let prefix = self.prefix_from(0, false);
        self.out.push_str(&prefix);
    }

    fn start_table(&mut self) {
        self.rows.clear();
        if self.quotes == 0 {
            self.end_line();
            if self.list_depth == 0 && !self.out.ends_with("\n\n") {
                self.out.push('\n');
            }
            return;
        }
        if self.marker_is_open() {
            // Marker line stays dangling, like the unquoted path.
            self.out.push('\n');
            self.first = false;
            return;
        }
        self.start_quote_block();
        // render_table writes the prefix on every line itself.
        let prefix = self.prefix_from(0, false);
        if self.out.ends_with(&prefix) {
            self.out.truncate(self.out.len() - prefix.len());
        }
    }

    fn end_table(&mut self) {
        let (prefix, prefix_width) = if self.quotes > 0 {
            (self.prefix_from(0, false), self.prefix_cols())
        } else {
            let indent = self.margin + self.list_depth * 2;
            (" ".repeat(indent), indent)
        };
        let table = render_table(&self.rows, &prefix, prefix_width, self.width, self.color);
        self.out.push_str(&table);
        self.rows.clear();
    }

    fn start_heading(&mut self, level: usize) {
        self.heading_level = level;
        if self.quotes > 0 {
            self.start_quote_block();
        } else {
            self.margin = level.saturating_sub(1) * 2;
            self.out.push('\n');
            self.out.push_str(&" ".repeat(self.margin));
        }
        if level == 1 {
            self.push_style(H1);
            self.out.push(' '); // bar padding
            return;
        }
        self.push_style(if level == 3 { H3C } else { BOLD });
        self.out.push_str(&"#".repeat(level));
        self.out.push(' ');
    }

    fn end_heading(&mut self) {
        if self.heading_level == 1 {
            self.out.push(' '); // close the bar
        }
        self.push_style(RESET);
        self.out.push('\n');
        self.heading_level = 0;
    }

    fn start_paragraph(&mut self) {
        if self.quotes > 0 {
            self.start_quote_block();
        } else if !self.out.ends_with("- ") {
            self.out.push_str(&" ".repeat(self.margin));
        }
    }

    fn start_list(&mut self) {
        if self.quotes > 0 {
            self.separate_list_in_quote();
        } else if self.list_depth == 0 && !self.out.is_empty() && !self.out.ends_with("\n\n") {
            self.end_line();
            self.out.push('\n');
        }
        self.stack.push(Ctx::List);
        self.list_depth += 1;
        self.first = true;
    }

    fn separate_list_in_quote(&mut self) {
        if self.marker_is_open() {
            return;
        }
        self.end_line();
        if !self.first && self.stack.last() == Some(&Ctx::Quote) {
            self.push_quote_separator();
        }
    }

    fn end_list(&mut self) {
        self.stack.pop();
        self.list_depth = self.list_depth.saturating_sub(1);
        if self.quotes == 0 && self.list_depth == 0 {
            self.out.push('\n');
        }
        self.first = false;
    }

    fn start_item(&mut self) {
        if self.quotes > 0 {
            self.end_line();
            if self.prev_loose && !self.first {
                self.push_quote_separator();
            }
            let prefix = self.prefix_from(0, true);
            self.out.push_str(&prefix);
        } else {
            let indent = self.margin + self.list_depth.saturating_sub(1) * 2;
            self.out.push_str(&" ".repeat(indent));
            self.out.push_str("- ");
        }
        self.open = Some((self.out.len(), self.stack.len() - 1));
        self.first = true;
    }

    fn end_item(&mut self) {
        // An item that ended with a paragraph is loose: the next item gets a
        // blank (bar-only, in a quote) line before it.
        self.prev_loose = self.out.len() == self.para_end;
        if !self.out.ends_with('\n') || self.quotes == 0 {
            self.out.push('\n');
        }
        self.open = None;
        self.first = false;
    }

    fn start_blockquote(&mut self) {
        if self.quotes > 0 && !self.marker_is_open() {
            self.end_line();
            if !self.first {
                self.push_quote_separator();
            }
        }
        self.stack.push(Ctx::Quote);
        self.quotes += 1;
        self.first = true;
    }

    fn end_blockquote(&mut self) {
        self.stack.pop();
        self.quotes = self.quotes.saturating_sub(1);
        if self.quotes == 0 {
            self.out.push('\n');
        }
        self.first = false;
    }

    fn start_code_block(&mut self) {
        self.in_code = true;
        if self.quotes > 0 {
            self.start_quote_block();
            self.code_fresh = true;
        }
    }

    fn text(&mut self, text: &str) {
        if !self.in_code {
            self.out.push_str(text);
            return;
        }
        for line in text.split_inclusive('\n') {
            self.code_line(line.strip_suffix('\n').unwrap_or(line));
        }
    }

    fn code_line(&mut self, body: &str) {
        if self.quotes == 0 {
            self.out.push('\n');
            self.out.push_str(&" ".repeat(self.margin));
        } else if !self.code_fresh {
            self.out.push('\n');
            let prefix = self.prefix_from(0, false);
            self.out.push_str(&prefix);
        }
        self.code_fresh = false;
        self.push_style(DIM);
        self.out.push('\u{2502}');
        self.push_style(RESET);
        self.out.push(' ');
        self.out.push_str(&highlight_lua(body, self.color));
    }

    fn line_break(&mut self) {
        self.out.push('\n');
        if self.quotes == 0 {
            self.out
                .push_str(&" ".repeat(self.margin + self.list_depth * 2));
            return;
        }
        let prefix = self.prefix_from(0, false);
        self.out.push_str(&prefix);
        if self.bold_open && !self.in_cell {
            self.push_style(BOLD);
        }
    }

    fn rule(&mut self) {
        if self.quotes == 0 {
            self.out.push_str("\n---\n");
            return;
        }
        self.start_quote_block();
        self.out.push_str("---\n");
    }
}

#[cfg(test)]
mod tests {
    use super::highlight_lua;
    use super::render;

    const MD: &str = "# Guide\n\n## Section\n\n### lur.json\n\nUse `lur.json` here.\n\n```lua\nlocal x = 1\n```\n";

    #[test]
    fn color_off_keeps_structure_without_escapes() {
        let out = render(MD, false);
        assert!(!out.contains('\x1b'), "no ANSI codes: {out:?}");
        assert!(out.contains("## Section"), "h2 prefix: {out:?}");
        assert!(out.contains("### lur.json"), "h3 prefix: {out:?}");
        assert!(out.contains("\u{2502} local x = 1"), "code frame: {out:?}");
        // Inline code and heading text survive as plain text.
        assert!(out.contains("Guide") && out.contains("lur.json"), "{out:?}");
    }

    #[test]
    fn tables_render_aligned_without_pipes_or_separator() {
        let md = "| Call | Returns |\n| --- | --- |\n| `a` | one |\n| `longer` | two |\n\nafter\n";
        let out = render(md, false);
        assert!(!out.contains("| ---") && !out.contains('|'), "{out:?}");
        assert!(!out.contains('\x1b'), "{out:?}");
        let rows: Vec<&str> = out
            .lines()
            .filter(|l| l.contains("one") || l.contains("two"))
            .collect();
        assert_eq!(rows.len(), 2, "{out:?}");
        // Second column starts at the same offset on both rows.
        assert_eq!(rows[0].find("one"), rows[1].find("two"), "{out:?}");
        assert!(out.contains("Call") && out.contains("after"), "{out:?}");
    }

    #[test]
    fn table_inside_list_item_keeps_items_clean() {
        let md =
            "- first\n\n  | k | v |\n  | --- | --- |\n  | a | b |\n\n- second\n\n  more text\n";
        let out = render(md, false);
        assert!(!out.contains("| ---"), "{out:?}");
        assert!(
            !out.contains("-       "),
            "no margin glued after the bullet: {out:?}"
        );
        assert!(
            out.contains("- first") && out.contains("- second"),
            "{out:?}"
        );
    }

    #[test]
    fn color_on_styles_headings_and_highlights_code() {
        let out = render(MD, true);
        assert!(out.contains("\x1b[1;7m"), "h1 reverse bar: {out:?}");
        assert!(out.contains("\x1b[1;36m"), "h3 bold cyan: {out:?}");
        assert!(
            out.contains("\x1b[1;36mlocal\x1b[0m"),
            "code highlighted: {out:?}"
        );
        assert!(out.contains('\u{2502}'), "code frame: {out:?}");
    }

    #[test]
    fn wrapped_paragraph_lines_keep_the_margin() {
        let md = "### lur.json\n\nfirst line here\nsecond line here\n";
        let out = render(md, false);
        // Under H3 the body margin is 4; the continuation line must be indented too.
        assert!(out.contains("    first line here"), "{out:?}");
        assert!(out.contains("    second line here"), "{out:?}");
    }

    #[test]
    fn highlight_lua_roundtrips_without_color() {
        let code = "local x = 1 -- a comment\nlocal s = \"hi\"\n";
        assert_eq!(highlight_lua(code, false), code);
    }

    #[test]
    fn highlight_lua_colors_each_token_class() {
        let out = highlight_lua("local n = 42 -- c\nlocal s = \"hi\"\n", true);
        assert!(out.contains("\x1b[1;36mlocal\x1b[0m"), "keyword: {out:?}");
        assert!(out.contains("\x1b[33m42\x1b[0m"), "number: {out:?}");
        assert!(out.contains("\x1b[32m\"hi\"\x1b[0m"), "string: {out:?}");
        assert!(out.contains("\x1b[90m-- c\x1b[0m"), "comment: {out:?}");
    }

    #[test]
    fn highlight_lua_does_not_misread_tokens() {
        // `--` inside a string is not a comment; `endpoint` is not the `end` keyword.
        let out = highlight_lua("local u = \"a--b\"\nlocal endpoint = 1\n", true);
        assert!(
            out.contains("\x1b[32m\"a--b\"\x1b[0m"),
            "string keeps --: {out:?}"
        );
        assert!(
            !out.contains("\x1b[1;36mend\x1b[0m"),
            "no partial keyword: {out:?}"
        );
        assert!(out.contains("endpoint"), "{out:?}");
    }

    #[test]
    fn highlight_lua_handles_unterminated_string() {
        // Must not panic; remainder is colored as the string.
        let out = highlight_lua("local s = \"oops", true);
        assert!(out.contains("\x1b[32m\"oops\x1b[0m"), "{out:?}");
    }
}

#[cfg(test)]
mod quote_fix_tests {
    use super::render;

    #[test]
    fn quoted_table_on_marker_line_has_no_stray_bar() {
        let out = render("> - | a | b |\n>   |---|---|\n>   | 1 | 2 |\n", false);
        assert!(!out.contains("\u{2502} -   "), "{out:?}");
        assert!(
            out.lines().next().unwrap().trim_end().ends_with('-'),
            "{out:?}"
        );
        assert!(
            out.contains("\u{2502}") && out.contains('1') && out.contains('2'),
            "{out:?}"
        );
    }

    #[test]
    fn nested_quote_gets_bar_only_separator() {
        let out = render("> a\n>\n> > b\n", false);
        assert_eq!(out, "\u{2502} a\n\u{2502}\n\u{2502} \u{2502} b\n");
    }

    #[test]
    fn heading_in_quote_does_not_shift_margin() {
        let out = render("> # T\n>\n> a\n>\n> b\n", false);
        assert!(out.contains("\u{2502} a\n"), "{out:?}");
        assert!(out.contains("\u{2502} b\n"), "{out:?}");
        assert!(!out.contains("\u{2502}   "), "{out:?}");
    }
}

#[cfg(test)]
mod table_tests {
    use super::{render, render_width, visible_len};

    const NARROW: &str = "| Call | Returns |\n| --- | --- |\n| `a` | one |\n| `longer` | two |\n";

    #[test]
    fn narrow_ascii_table_is_unchanged_by_width() {
        let plain = render(NARROW, false);
        assert_eq!(render_width(NARROW, false, Some(80)), plain);
        assert_eq!(
            plain,
            "Call    Returns\n\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\na       one\nlonger  two\n"
        );
    }

    #[test]
    fn wide_table_wraps_to_width_and_keeps_columns_aligned() {
        let md = "| Name | Description |\n| --- | --- |\n| `fetch` | Fetches a remote resource over HTTP and returns the body as a string value |\n| `x` | short |\n";
        let out = render_width(md, false, Some(40));
        for l in out.lines() {
            assert!(visible_len(l) <= 40, "line too wide: {l:?}");
        }
        let lines: Vec<&str> = out.lines().collect();
        let col = lines[0].find("Description").unwrap();
        assert!(lines.len() > 5, "wrapped into several lines: {out:?}");
        // Continuation lines start in the second column, first column blank.
        assert!(lines[2].starts_with("fetch"), "{out:?}");
        assert!(
            lines[3].starts_with(&" ".repeat(col)) && !lines[3][col..].is_empty(),
            "{out:?}"
        );
        assert_eq!(lines.last().unwrap().find("short"), Some(col), "{out:?}");
        // No wrapping without a width.
        assert!(render(md, false).lines().any(|l| l.len() > 60));
    }

    #[test]
    fn cjk_cells_use_display_width() {
        let md = "| 名稱 | 說明 |\n| --- | --- |\n| `ab` | one |\n| 中文字 | two |\n| ✅ | yes |\n| 👨‍👩‍👧 | fam |\n";
        let out = render(md, false);
        assert_eq!(visible_len("中文字"), 6);
        assert_eq!(visible_len("✅"), 2);
        assert_eq!(visible_len("❌"), 2);
        assert_eq!(visible_len("👨‍👩‍👧"), 2);
        assert_eq!(visible_len("👍🏽"), 2);
        assert_eq!(visible_len("\x1b[1m中\x1b[0m✅"), 4);
        // Second column starts at the same display column on every row.
        let cols: Vec<usize> = out
            .lines()
            .filter(|l| {
                l.ends_with("one")
                    || l.ends_with("two")
                    || l.ends_with("yes")
                    || l.ends_with("fam")
                    || l.ends_with("說明")
            })
            .map(|l| {
                let (first, _) = l.split_once("  ").unwrap();
                let gap = l[first.len()..].len() - l[first.len()..].trim_start().len();
                visible_len(first) + gap
            })
            .collect();
        assert_eq!(cols.len(), 5, "{out:?}");
        assert!(cols.iter().all(|&c| c == cols[0]), "{cols:?} {out:?}");
    }

    #[test]
    fn table_in_blockquote_keeps_bar_on_every_line() {
        let md = "> note\n>\n> | k | v |\n> | --- | --- |\n> | a | bbb |\n> | cc | d |\n";
        let out = render(md, false);
        let table: Vec<&str> = out
            .lines()
            .filter(|l| !l.contains("note"))
            .filter(|l| !l.trim().is_empty() && *l != "\u{2502}")
            .collect();
        assert_eq!(table.len(), 4, "{out:?}");
        for l in &table {
            assert!(l.starts_with("\u{2502} "), "bar missing: {l:?} in {out:?}");
        }
        assert!(!out.contains('|'), "{out:?}");
        assert_eq!(table[2].find("bbb"), table[3].find('d'), "{out:?}");
        // Wrapping accounts for the bar.
        let wide = "> | k | v |\n> | --- | --- |\n> | a | one two three four five six seven eight nine ten |\n";
        for l in render_width(wide, false, Some(30)).lines() {
            assert!(visible_len(l) <= 30, "{l:?}");
        }
    }

    fn lines(md: &str, width: Option<usize>) -> Vec<String> {
        render_width(md, false, width)
            .lines()
            .map(str::to_owned)
            .collect()
    }

    #[test]
    fn nested_quote_table_carries_one_bar_per_level() {
        let md = "> > | k | v |\n> > | - | - |\n> > | a | b |\n";
        assert_eq!(
            lines(md, None),
            ["│ │ k  v", "│ │ ────", "│ │ a  b"],
            "{md:?}"
        );
    }

    #[test]
    fn list_in_quote_with_table_and_paragraph() {
        let md = "> - item\n>\n>   | k | v |\n>   | - | - |\n>   | a | b |\n>\n> after\n";
        assert_eq!(
            lines(md, None),
            [
                "│ - item",
                "│",
                "│   k  v",
                "│   ────",
                "│   a  b",
                "│",
                "│ after"
            ]
        );
    }

    #[test]
    fn paragraph_after_table_in_quote_keeps_bar() {
        let md = "> | k | v |\n> | - | - |\n> | a | b |\n>\n> after\n";
        assert_eq!(
            lines(md, None),
            ["│ k  v", "│ ────", "│ a  b", "│", "│ after"]
        );
    }

    #[test]
    fn multi_line_paragraph_in_quote_keeps_bar() {
        let md = "> one\n> two\n>\n> three\n> four\n";
        assert_eq!(
            lines(md, None),
            ["│ one", "│ two", "│", "│ three", "│ four"]
        );
    }

    #[test]
    fn code_fence_in_quote_keeps_bar() {
        let md = "> before\n>\n> ```lua\n> local x = 1\n> local y = 2\n> ```\n>\n> after\n";
        assert_eq!(
            lines(md, None),
            [
                "│ before",
                "│",
                "│ │ local x = 1",
                "│ │ local y = 2",
                "│",
                "│ after"
            ]
        );
    }

    #[test]
    fn quote_inside_list_item() {
        let md = "- item\n\n  > quoted\n  > more\n\n  tail\n- next\n";
        let out = lines(md, None);
        assert_eq!(out[0], "- item", "{out:?}");
        assert!(out.contains(&"  │ quoted".to_owned()), "{out:?}");
        assert!(out.contains(&"  │ more".to_owned()), "{out:?}");
        assert!(out.iter().any(|l| l.ends_with("- next")), "{out:?}");
        // A quote directly after the marker shares its line.
        let md = "- > quoted\n  > more\n";
        assert_eq!(lines(md, None), ["- │ quoted", "  │ more"]);
    }

    #[test]
    fn loose_list_in_quote_keeps_bars_between_items() {
        let md = "> - one\n>\n> - two\n>\n> after\n";
        assert_eq!(lines(md, None), ["│ - one", "│", "│ - two", "│", "│ after"]);
    }

    #[test]
    fn bold_continues_across_quoted_soft_break() {
        let out = render("> **bold\n> continues** after\n", true);
        assert!(
            out.contains("\u{2502}\x1b[0m \x1b[1mcontinues"),
            "bold re-opened after the prefix: {out:?}"
        );
    }

    #[test]
    fn italic_continues_across_nested_quote_soft_break() {
        let out = render("> > *it\n> > goes* on\n", true);
        assert!(
            out.contains("\u{2502}\x1b[0m \x1b[1mgoes"),
            "style re-opened after nested prefix: {out:?}"
        );
    }

    #[test]
    fn bold_continues_across_list_item_line_in_quote() {
        let out = render("> - **a\n>   b** c\n", true);
        assert!(
            out.contains("\x1b[0m   \x1b[1mb"),
            "list continuation: {out:?}"
        );
    }

    #[test]
    fn closed_style_does_not_leak_across_break() {
        let out = render("> **a** b\n> c\n", true);
        assert_eq!(out.matches("\x1b[1m").count(), 1, "{out:?}");
    }

    #[test]
    fn colour_off_soft_break_unchanged() {
        let out = render("> **bold\n> continues** after\n", false);
        assert!(!out.contains('\x1b'));
        assert!(out.contains("\u{2502} bold\n"));
        assert!(out.contains("\u{2502} continues after"));
    }

    #[test]
    fn narrow_width_wraps_inside_nested_quote() {
        let md = "> > | k | v |\n> > | - | - |\n> > | a | one two three four five six seven eight nine ten |\n";
        let out = lines(md, Some(24));
        assert!(out.len() > 4, "{out:?}");
        for l in &out {
            assert!(l.starts_with("│ │ ") && visible_len(l) <= 24, "{l:?}");
        }
    }
}
