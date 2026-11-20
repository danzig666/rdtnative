//! Reddit markdown → blocks of styled spans, for the TV's rich-text widget. Covers what Reddit's
//! own renderer (snudown / its "new" markdown) does on real posts and comments: paragraphs, hard
//! breaks, headings, quotes, ordered and bullet lists (nested), fenced and indented code, rules,
//! tables, bold, italic, strike, inline code, superscript, spoilers, links, bare URLs, `r/` and
//! `u/` mentions and HTML entities. Unknown syntax is shown as text.

/// Span style bits.
pub const BOLD: u8 = 1;
pub const ITALIC: u8 = 2;
pub const STRIKE: u8 = 4;
pub const CODE: u8 = 8;
pub const SUP: u8 = 16;
pub const SPOILER: u8 = 32;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Span {
    pub text: String,
    pub style: u8,
    pub link: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Block {
    Para(Vec<Span>),
    Heading(u8, Vec<Span>),
    Quote(Vec<Block>),
    List { ordered: bool, start: u32, items: Vec<Vec<Block>> },
    Code(String),
    Rule,
    Table { head: Vec<Vec<Span>>, rows: Vec<Vec<Vec<Span>>> },
}

/// Decodes the HTML entities Reddit leaves in bodies (`&amp;`, `&#x200B;`, `&nbsp;`, …).
pub fn entities(s: &str) -> String {
    if !s.contains('&') {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find('&') {
        out.push_str(&rest[..i]);
        rest = &rest[i..];
        let end = rest.find(';').filter(|&e| e < 12);
        let decoded = end.and_then(|e| {
            let name = &rest[1..e];
            let c = match name {
                "amp" => Some('&'),
                "lt" => Some('<'),
                "gt" => Some('>'),
                "quot" => Some('"'),
                "apos" | "#39" => Some('\''),
                "nbsp" => Some('\u{a0}'),
                _ if name.starts_with("#x") || name.starts_with("#X") => u32::from_str_radix(&name[2..], 16).ok().and_then(char::from_u32),
                _ if name.starts_with('#') => name[1..].parse().ok().and_then(char::from_u32),
                _ => None,
            }?;
            Some((c, e + 1))
        });
        match decoded {
            Some((c, n)) => {
                // Zero-width spaces only pad empty paragraphs on Reddit: drop them.
                if c != '\u{200b}' {
                    out.push(c);
                }
                rest = &rest[n..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

// ---------------------------------------------------------------------------------- inline

fn push(out: &mut Vec<Span>, text: &str, style: u8, link: Option<&str>) {
    if text.is_empty() {
        return;
    }
    if let Some(last) = out.last_mut() {
        if last.style == style && last.link.as_deref() == link {
            last.text.push_str(text);
            return;
        }
    }
    out.push(Span { text: text.to_string(), style, link: link.map(str::to_string) });
}

fn is_word(c: char) -> bool {
    c.is_alphanumeric()
}

/// Finds the closing `delim` for an opening at the start of `s` (after the delimiter), not
/// preceded by whitespace and with something between.
fn closing(s: &str, delim: &str) -> Option<usize> {
    let mut from = 0;
    while let Some(i) = s[from..].find(delim) {
        let mut at = from + i;
        if at > 0 && !s[..at].ends_with(char::is_whitespace) && !s[..at].ends_with('\\') {
            // In a longer run ("***" closing "**" around an inner "*"), the outer closes last.
            let c = delim.chars().next().unwrap_or(' ');
            while s[at + delim.len()..].starts_with(c) {
                at += c.len_utf8();
            }
            return Some(at);
        }
        from = at + delim.len();
    }
    None
}

fn url_end(s: &str) -> usize {
    let mut depth = 0i32;
    let mut end = 0;
    for (i, c) in s.char_indices() {
        if c.is_whitespace() || c == '<' || c == '>' || c == '"' {
            break;
        }
        match c {
            '(' => depth += 1,
            ')' if depth == 0 => break,
            ')' => depth -= 1,
            _ => {}
        }
        end = i + c.len_utf8();
    }
    // Trailing punctuation belongs to the sentence.
    while end > 0 && s[..end].ends_with(['.', ',', ';', ':', '!', '?', '\'', '*']) {
        end -= 1;
    }
    end
}

/// Inline markdown of one paragraph (lines already joined) into spans.
pub fn inline(s: &str) -> Vec<Span> {
    let mut out = Vec::new();
    run(&entities(s), 0, None, &mut out);
    out
}

fn run(s: &str, style: u8, link: Option<&str>, out: &mut Vec<Span>) {
    let mut text_start = 0;
    let mut i = 0;
    let b = s.as_bytes();
    let flush = |out: &mut Vec<Span>, from: usize, to: usize| push(out, &s[from..to], style, link);
    while i < s.len() {
        let rest = &s[i..];
        let prev = s[..i].chars().last();
        // Escapes.
        if rest.starts_with('\\') && rest.len() > 1 {
            let c = rest[1..].chars().next().unwrap();
            if c.is_ascii_punctuation() {
                flush(out, text_start, i);
                push(out, &c.to_string(), style, link);
                i += 1 + c.len_utf8();
                text_start = i;
                continue;
            }
        }
        // Inline code: no markup inside.
        if rest.starts_with('`') {
            let ticks = rest.len() - rest.trim_start_matches('`').len();
            let delim = &rest[..ticks];
            if let Some(e) = rest[ticks..].find(delim) {
                flush(out, text_start, i);
                push(out, rest[ticks..ticks + e].trim(), style | CODE, link);
                i += ticks + e + ticks;
                text_start = i;
                continue;
            }
        }
        // Spoilers >!text!<
        if let Some(inner) = rest.strip_prefix(">!") {
            if let Some(e) = inner.find("!<") {
                flush(out, text_start, i);
                run(&inner[..e], style | SPOILER, link, out);
                i += 2 + e + 2;
                text_start = i;
                continue;
            }
        }
        // Inline media ![gif](giphy|id) / ![img](emote|...): shown as a tag, linked when it is a URL.
        if rest.starts_with("![") {
            if let Some((text, url, used)) = md_link(&rest[1..]) {
                flush(out, text_start, i);
                let label = if text.eq_ignore_ascii_case("gif") { "[GIF]" } else { "[image]" };
                let link = (url.starts_with("http")).then_some(url.as_str());
                push(out, label, style | ITALIC, link.or(link));
                i += 1 + used;
                text_start = i;
                continue;
            }
        }
        // Links [text](url "title")
        if rest.starts_with('[') && link.is_none() {
            if let Some((text, url, used)) = md_link(rest) {
                flush(out, text_start, i);
                run(text, style, Some(&url), out);
                i += used;
                text_start = i;
                continue;
            }
        }
        // Emphasis.
        for (delim, bit) in [("***", BOLD | ITALIC), ("**", BOLD), ("__", BOLD), ("~~", STRIKE), ("*", ITALIC), ("_", ITALIC)] {
            if !rest.starts_with(delim) {
                continue;
            }
            // Underscores inside words are not emphasis (snake_case).
            if delim.starts_with('_') && prev.is_some_and(is_word) {
                break;
            }
            let inner = &rest[delim.len()..];
            if inner.starts_with(char::is_whitespace) || inner.is_empty() {
                break;
            }
            if let Some(e) = closing(inner, delim) {
                let after = inner[e + delim.len()..].chars().next();
                if delim.starts_with('_') && after.is_some_and(is_word) {
                    break;
                }
                flush(out, text_start, i);
                run(&inner[..e], style | bit, link, out);
                i += delim.len() + e + delim.len();
                text_start = i;
                break;
            }
            break;
        }
        if text_start > i {
            continue;
        }
        if i < text_start {
            continue;
        }
        // Superscript ^word or ^(several words)
        if rest.starts_with('^') && rest.len() > 1 {
            let (inner, used) = if rest[1..].starts_with('(') {
                match rest[2..].find(')') {
                    Some(e) => (&rest[2..2 + e], 2 + e + 1),
                    None => ("", 0),
                }
            } else {
                let e = rest[1..].find(char::is_whitespace).unwrap_or(rest.len() - 1);
                (&rest[1..1 + e], 1 + e)
            };
            if used > 0 && !inner.is_empty() {
                flush(out, text_start, i);
                run(inner, style | SUP, link, out);
                i += used;
                text_start = i;
                continue;
            }
        }
        // Bare URLs.
        if link.is_none() && (rest.starts_with("http://") || rest.starts_with("https://")) && !prev.is_some_and(is_word) {
            let e = url_end(rest);
            if e > 8 {
                flush(out, text_start, i);
                push(out, &rest[..e], style, Some(&rest[..e]));
                i += e;
                text_start = i;
                continue;
            }
        }
        // r/sub and u/user mentions.
        if link.is_none() && !prev.is_some_and(|c| is_word(c) || c == '/') {
            let r = rest.strip_prefix('/').unwrap_or(rest);
            let lead = rest.len() - r.len();
            if (r.starts_with("r/") || r.starts_with("u/")) && r.len() > 2 {
                let name_len = r[2..].find(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == '-')).unwrap_or(r.len() - 2);
                if name_len >= 2 {
                    let mention = &r[..2 + name_len];
                    flush(out, text_start, i);
                    push(out, &rest[..lead + 2 + name_len], style, Some(&format!("https://www.reddit.com/{mention}")));
                    i += lead + 2 + name_len;
                    text_start = i;
                    continue;
                }
            }
        }
        i += rest.chars().next().map_or(1, char::len_utf8);
        let _ = b;
    }
    flush(out, text_start, s.len());
}

/// `[text](url)` at the start of `s`: (text, url, bytes used).
fn md_link(s: &str) -> Option<(&str, String, usize)> {
    let mut depth = 0;
    let mut close = None;
    for (i, c) in s.char_indices() {
        match c {
            '[' => depth += 1,
            ']' => {
                depth -= 1;
                if depth == 0 {
                    close = Some(i);
                    break;
                }
            }
            _ => {}
        }
    }
    let close = close?;
    let after = &s[close + 1..];
    if !after.starts_with('(') {
        return None;
    }
    let mut depth = 0;
    let mut end = None;
    for (i, c) in after.char_indices() {
        match c {
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    end = Some(i);
                    break;
                }
            }
            _ => {}
        }
    }
    let end = end?;
    let target = after[1..end].trim();
    let url = target.split_once(" \"").map_or(target, |(u, _)| u).trim_matches(['<', '>']);
    if url.is_empty() {
        return None;
    }
    let url = if url.starts_with("/r/") || url.starts_with("/u/") || url.starts_with("/user/") {
        format!("https://www.reddit.com{url}")
    } else {
        url.to_string()
    };
    Some((&s[1..close], url, close + 1 + end + 1))
}

// ---------------------------------------------------------------------------------- blocks

fn indent(line: &str) -> usize {
    let mut n = 0;
    for c in line.chars() {
        match c {
            ' ' => n += 1,
            '\t' => n += 4,
            _ => break,
        }
    }
    n
}

fn strip_indent(line: &str, n: usize) -> &str {
    let mut taken = 0;
    for (i, c) in line.char_indices() {
        if taken >= n {
            return &line[i..];
        }
        match c {
            ' ' => taken += 1,
            '\t' => taken += 4,
            _ => return &line[i..],
        }
    }
    ""
}

/// A list marker at the start of `line` (after indentation): (ordered, number, content offset).
fn list_marker(line: &str) -> Option<(bool, u32, usize)> {
    let t = line.trim_start();
    let lead = line.len() - t.len();
    let b = t.as_bytes();
    if b.len() >= 2 && matches!(b[0], b'*' | b'-' | b'+') && matches!(b[1], b' ' | b'\t') {
        return Some((false, 0, lead + 2));
    }
    let digits = t.bytes().take_while(u8::is_ascii_digit).count();
    if digits > 0 && digits < 10 && t[digits..].starts_with(['.', ')']) && t[digits + 1..].starts_with([' ', '\t']) {
        return Some((true, t[..digits].parse().unwrap_or(1), lead + digits + 2));
    }
    None
}

fn is_rule(t: &str) -> bool {
    let c: String = t.chars().filter(|c| !c.is_whitespace()).collect();
    c.len() >= 3 && (c.chars().all(|x| x == '-') || c.chars().all(|x| x == '*') || c.chars().all(|x| x == '_'))
}

fn table_cells(line: &str) -> Vec<String> {
    let t = line.trim().trim_start_matches('|').trim_end_matches('|');
    t.split('|').map(|c| c.trim().to_string()).collect()
}

fn is_table_sep(line: &str) -> bool {
    let t = line.trim();
    t.contains('-') && t.contains('|') && t.chars().all(|c| matches!(c, '|' | '-' | ':' | ' ' | '\t'))
        || (t.contains("--") && t.chars().all(|c| matches!(c, '|' | '-' | ':' | ' ')) && table_cells(t).len() > 1)
}

/// Parses Reddit markdown into blocks.
pub fn parse(md: &str) -> Vec<Block> {
    let md = md.replace("\r\n", "\n");
    let lines: Vec<&str> = md.split('\n').collect();
    blocks(&lines)
}

fn blocks(lines: &[&str]) -> Vec<Block> {
    let mut out = Vec::new();
    let mut i = 0;
    let mut para: Vec<String> = Vec::new();
    let end_para = |para: &mut Vec<String>, out: &mut Vec<Block>| {
        if para.is_empty() {
            return;
        }
        // Lines join with a space; two trailing spaces (or a backslash) make a hard break.
        let mut text = String::new();
        for (k, l) in para.iter().enumerate() {
            let hard = l.ends_with("  ") || l.ends_with('\\');
            text.push_str(l.trim_end().trim_end_matches('\\'));
            if k + 1 < para.len() {
                text.push(if hard { '\n' } else { ' ' });
            }
        }
        let spans = inline(text.trim());
        if !spans.is_empty() {
            out.push(Block::Para(spans));
        }
        para.clear();
    };
    while i < lines.len() {
        let line = lines[i];
        let t = line.trim();
        if t.is_empty() {
            end_para(&mut para, &mut out);
            i += 1;
            continue;
        }
        // Fenced code.
        if t.starts_with("```") || t.starts_with("~~~") {
            end_para(&mut para, &mut out);
            let fence = &t[..3];
            let mut code = Vec::new();
            i += 1;
            while i < lines.len() && !lines[i].trim().starts_with(fence) {
                code.push(lines[i]);
                i += 1;
            }
            i += 1;
            out.push(Block::Code(code.join("\n")));
            continue;
        }
        // Indented code (not inside a paragraph).
        if para.is_empty() && indent(line) >= 4 && list_marker(line).is_none() {
            let mut code = Vec::new();
            while i < lines.len() && (indent(lines[i]) >= 4 || lines[i].trim().is_empty()) {
                code.push(strip_indent(lines[i], 4));
                i += 1;
            }
            while code.last().is_some_and(|l| l.trim().is_empty()) {
                code.pop();
            }
            out.push(Block::Code(code.join("\n")));
            continue;
        }
        if t.starts_with('#') {
            let level = t.bytes().take_while(|&b| b == b'#').count();
            if level <= 6 && (t.len() == level || t[level..].starts_with(' ')) {
                end_para(&mut para, &mut out);
                let text = t[level..].trim().trim_end_matches('#').trim();
                out.push(Block::Heading(level as u8, inline(text)));
                i += 1;
                continue;
            }
        }
        if is_rule(t) && (para.is_empty() || !t.starts_with('-')) {
            end_para(&mut para, &mut out);
            out.push(Block::Rule);
            i += 1;
            continue;
        }
        if t.starts_with('>') && !t.starts_with(">!") {
            end_para(&mut para, &mut out);
            let mut inner = Vec::new();
            while i < lines.len() {
                let l = lines[i].trim_start();
                if let Some(r) = l.strip_prefix('>').filter(|_| !l.starts_with(">!")) {
                    inner.push(r.strip_prefix(' ').unwrap_or(r));
                } else if !l.is_empty() && !inner.is_empty() && list_marker(l).is_none() {
                    // Lazy continuation of the quoted paragraph.
                    inner.push(l);
                } else {
                    break;
                }
                i += 1;
            }
            out.push(Block::Quote(blocks(&inner)));
            continue;
        }
        if t.contains('|') && i + 1 < lines.len() && is_table_sep(lines[i + 1]) {
            end_para(&mut para, &mut out);
            let head = table_cells(t).iter().map(|c| inline(c)).collect();
            i += 2;
            let mut rows = Vec::new();
            while i < lines.len() && lines[i].contains('|') && !lines[i].trim().is_empty() {
                rows.push(table_cells(lines[i]).iter().map(|c| inline(c)).collect());
                i += 1;
            }
            out.push(Block::Table { head, rows });
            continue;
        }
        if let Some((ordered, start, _)) = list_marker(line).filter(|_| para.is_empty() || indent(line) < 4) {
            end_para(&mut para, &mut out);
            let base = indent(line);
            let mut items: Vec<Vec<String>> = Vec::new();
            while i < lines.len() {
                let l = lines[i];
                let ind = indent(l);
                match list_marker(l) {
                    Some((o, _, off)) if ind <= base + 1 && o == ordered => {
                        items.push(vec![l[off..].to_string()]);
                        i += 1;
                    }
                    _ if l.trim().is_empty() => {
                        // A blank line ends the list unless the next line continues it.
                        let next = lines.get(i + 1).copied().unwrap_or("");
                        if indent(next) > base || list_marker(next).is_some_and(|(o, _, _)| o == ordered && indent(next) <= base + 1) {
                            if let Some(it) = items.last_mut() {
                                it.push(String::new());
                            }
                            i += 1;
                        } else {
                            break;
                        }
                    }
                    _ if ind > base => {
                        if let Some(it) = items.last_mut() {
                            it.push(strip_indent(l, (base + 2).min(ind)).to_string());
                        }
                        i += 1;
                    }
                    Some(_) => break,
                    None if !items.is_empty() && !l.trim().is_empty() && list_marker(l).is_none() && !l.trim_start().starts_with(['#', '>']) => {
                        // Lazy continuation of the item's paragraph.
                        if let Some(it) = items.last_mut() {
                            it.push(l.trim().to_string());
                        }
                        i += 1;
                    }
                    None => break,
                }
            }
            let items = items.iter().map(|it| blocks(&it.iter().map(String::as_str).collect::<Vec<_>>())).collect();
            out.push(Block::List { ordered, start, items });
            continue;
        }
        para.push(line.trim_start().to_string());
        i += 1;
    }
    end_para(&mut para, &mut out);
    out
}

/// The text of `md` without markup, on one line (card previews, notifications).
pub fn plain(md: &str) -> String {
    fn spans(s: &[Span], out: &mut String) {
        for sp in s {
            out.push_str(&sp.text.replace('\n', " "));
        }
    }
    fn walk(b: &[Block], out: &mut String) {
        for blk in b {
            if !out.is_empty() && !out.ends_with(' ') {
                out.push(' ');
            }
            match blk {
                Block::Para(s) | Block::Heading(_, s) => spans(s, out),
                Block::Quote(inner) => walk(inner, out),
                Block::List { items, .. } => {
                    for it in items {
                        walk(it, out);
                    }
                }
                Block::Code(c) => out.push_str(&c.replace('\n', " ")),
                Block::Rule => {}
                Block::Table { head, rows } => {
                    for c in head.iter().chain(rows.iter().flatten()) {
                        spans(c, out);
                        out.push(' ');
                    }
                }
            }
        }
    }
    let mut out = String::new();
    walk(&parse(md), &mut out);
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Every link in `blocks`, in order, without repeats.
pub fn links(blocks: &[Block]) -> Vec<(String, String)> {
    fn spans(s: &[Span], out: &mut Vec<(String, String)>) {
        for sp in s {
            if let Some(l) = &sp.link {
                match out.iter_mut().find(|(u, _)| u == l) {
                    Some(e) => {
                        if !e.1.contains(&sp.text) && e.1.len() < 80 {
                            e.1.push_str(&sp.text);
                        }
                    }
                    None => out.push((l.clone(), sp.text.clone())),
                }
            }
        }
    }
    fn walk(b: &[Block], out: &mut Vec<(String, String)>) {
        for blk in b {
            match blk {
                Block::Para(s) | Block::Heading(_, s) => spans(s, out),
                Block::Quote(inner) => walk(inner, out),
                Block::List { items, .. } => items.iter().for_each(|it| walk(it, out)),
                Block::Table { head, rows } => head.iter().chain(rows.iter().flatten()).for_each(|c| spans(c, out)),
                Block::Code(_) | Block::Rule => {}
            }
        }
    }
    let mut out = Vec::new();
    walk(blocks, &mut out);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sp(text: &str, style: u8) -> Span {
        Span { text: text.into(), style, link: None }
    }

    #[test]
    fn emphasis_code_strike_sup_spoiler() {
        assert_eq!(inline("a **b** *c* ~~d~~ `e*f*`"), vec![sp("a ", 0), sp("b", BOLD), sp(" ", 0), sp("c", ITALIC), sp(" ", 0), sp("d", STRIKE), sp(" ", 0), sp("e*f*", CODE)]);
        assert_eq!(inline("x^2 and ^(two words)"), vec![sp("x", 0), sp("2", SUP), sp(" and ", 0), sp("two words", SUP)]);
        assert_eq!(inline("see >!he dies!< ok"), vec![sp("see ", 0), sp("he dies", SPOILER), sp(" ok", 0)]);
        assert_eq!(inline("snake_case_name stays"), vec![sp("snake_case_name stays", 0)]);
        assert_eq!(inline("***both***"), vec![sp("both", BOLD | ITALIC)]);
        assert_eq!(inline("2 * 3 * 4"), vec![sp("2 * 3 * 4", 0)]);
        assert_eq!(inline("\\*not\\*"), vec![sp("*not*", 0)]);
        assert_eq!(inline("**bold *and italic***"), vec![sp("bold ", BOLD), sp("and italic", BOLD | ITALIC)]);
    }

    #[test]
    fn links_mentions_and_entities() {
        let s = inline("[the docs](https://a.b/c_(d)) and https://x.y/z. Ask r/rust or /u/someone &amp; &#x200B;co");
        assert_eq!(s[0], Span { text: "the docs".into(), style: 0, link: Some("https://a.b/c_(d)".into()) });
        assert_eq!(s[2].link.as_deref(), Some("https://x.y/z"));
        assert!(s.iter().any(|x| x.text == "r/rust" && x.link.as_deref() == Some("https://www.reddit.com/r/rust")));
        assert!(s.iter().any(|x| x.text == "/u/someone" && x.link.as_deref() == Some("https://www.reddit.com/u/someone")));
        assert!(s.last().unwrap().text.ends_with(" & co"));
        assert_eq!(inline("[**bold link**](/r/pics)")[0], Span { text: "bold link".into(), style: BOLD, link: Some("https://www.reddit.com/r/pics".into()) });
        assert_eq!(entities("a&lt;b&gt;&#39;&quot;&nbsp;&unknown;"), "a<b>'\"\u{a0}&unknown;");
    }

    #[test]
    fn blocks_of_a_real_comment() {
        let md = "# Title\n\nFirst line\ncontinues here  \nhard break\n\n> quoted\n> more\n\n* one\n* two\n    * nested\n\n1. a\n2. b\n\n---\n\n    code line\n\n```\nfenced\n```\n\n|h1|h2|\n|:-|-:|\n|a|b|\n|c|d|\n\nend";
        let b = parse(md);
        assert_eq!(b[0], Block::Heading(1, vec![sp("Title", 0)]));
        assert_eq!(b[1], Block::Para(vec![sp("First line continues here\nhard break", 0)]));
        assert_eq!(b[2], Block::Quote(vec![Block::Para(vec![sp("quoted more", 0)])]));
        let Block::List { ordered: false, items, .. } = &b[3] else { panic!("{:?}", b[3]) };
        assert_eq!(items.len(), 2);
        assert!(matches!(&items[1][1], Block::List { ordered: false, items, .. } if items.len() == 1));
        assert!(matches!(&b[4], Block::List { ordered: true, start: 1, items } if items.len() == 2));
        assert_eq!(b[5], Block::Rule);
        assert_eq!(b[6], Block::Code("code line".into()));
        assert_eq!(b[7], Block::Code("fenced".into()));
        let Block::Table { head, rows } = &b[8] else { panic!("{:?}", b[8]) };
        assert_eq!((head.len(), rows.len()), (2, 2));
        assert_eq!(rows[1][1], vec![sp("d", 0)]);
        assert_eq!(b[9], Block::Para(vec![sp("end", 0)]));
    }

    #[test]
    fn inline_media_tags() {
        assert_eq!(inline("look ![gif](giphy|abc123)"), vec![sp("look ", 0), sp("[GIF]", ITALIC)]);
        assert_eq!(inline("![img](https://i.redd.it/x.png)")[0].link.as_deref(), Some("https://i.redd.it/x.png"));
    }

    #[test]
    fn multibyte_line_starts_and_entities() {
        assert_eq!(parse("\u{201c}quoted\u{201d}\n\u{2014} dash"), vec![Block::Para(vec![sp("\u{201c}quoted\u{201d} \u{2014} dash", 0)])]);
        assert_eq!(entities("&\u{e9}\u{e9}\u{e9}\u{e9}\u{e9}\u{e9};"), "&\u{e9}\u{e9}\u{e9}\u{e9}\u{e9}\u{e9};");
    }

    #[test]
    fn plain_and_links() {
        assert_eq!(plain("**Hi** there\n\n* [a](https://a.com)\n* b"), "Hi there a b");
        let l = links(&parse("[x](https://a.com) and https://b.com and [y](https://a.com)"));
        assert_eq!(l.iter().map(|(u, _)| u.as_str()).collect::<Vec<_>>(), ["https://a.com", "https://b.com"]);
    }
}
