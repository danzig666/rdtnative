//! Rich text: Reddit markdown (`rd_core::markdown` blocks) laid out into wrapped lines of styled
//! runs at a width and text size, and drawn. Layouts are cached by (key, width, size), so a comment
//! is measured once, not every frame.

use crate::ui;
use plx_ui::{Painter, Rect};
use rd_core::markdown::{self, Block, Span, BOLD, CODE, ITALIC, SPOILER, STRIKE, SUP};
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Ink {
    Body,
    Dim,
    Link,
    Code,
    Heading,
}

#[derive(Clone, Debug)]
pub enum Item {
    Text { s: String, x: f32, y: f32, size: i32, code: i32, ink: Ink, strike: bool, underline: bool, spoiler: bool, w: f32 },
    /// Backgrounds: inline code, code blocks, spoiler covers, quote bars, rules, table lines.
    Fill { r: Rect, kind: FillKind },
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum FillKind {
    CodeBg,
    QuoteBar,
    Rule,
    Spoiler,
    TableHead,
    TableLine,
}

#[derive(Default, Debug)]
pub struct Laid {
    pub h: f32,
    pub items: Vec<Item>,
    /// Links in order of appearance: (url, text).
    pub links: Vec<(String, String)>,
    pub has_spoiler: bool,
}

fn line_h(size: i32) -> f32 {
    (size as f32 * 1.42).round()
}

struct Ctx {
    out: Laid,
    base: i32,
}

/// One line being filled.
struct LineBuf {
    x0: f32,
    w: f32,
    x: f32,
    y: f32,
    size: i32,
    pending: Vec<Item>,
}

impl LineBuf {
    fn new(x0: f32, w: f32, y: f32, size: i32) -> LineBuf {
        LineBuf { x0, w, x: x0, y, size, pending: Vec::new() }
    }
}

fn code_of(style: u8, heading: bool) -> i32 {
    if style & CODE != 0 {
        ui::mono_code(style & BOLD != 0)
    } else {
        ui::font_code(style & BOLD != 0 || heading)
    }
}

impl Ctx {
    /// Lays out `spans` from `y` in the column (x0, w); returns the y below the last line.
    fn spans(&mut self, spans: &[Span], x0: f32, w: f32, y: f32, size: i32, ink: Ink, heading: bool) -> f32 {
        let mut line = LineBuf::new(x0, w, y, size);
        for sp in spans {
            let style = sp.style;
            let sz = if style & SUP != 0 { ((size as f32) * 0.72) as i32 } else if style & CODE != 0 { ((size as f32) * 0.9) as i32 } else { size };
            let sz = sz.clamp(10, 79);
            let code = code_of(style, heading);
            let link = sp.link.as_ref();
            if let Some(l) = link {
                if !self.out.links.iter().any(|(u, _)| u == l) {
                    self.out.links.push((l.clone(), sp.text.clone()));
                }
            }
            if style & SPOILER != 0 {
                self.out.has_spoiler = true;
            }
            let run_ink = if link.is_some() {
                Ink::Link
            } else if style & CODE != 0 {
                Ink::Code
            } else if heading {
                Ink::Heading
            } else if style & ITALIC != 0 && ink == Ink::Body {
                Ink::Dim
            } else {
                ink
            };
            // Words and the spaces between them; '\n' is a hard break.
            for (k, hard) in sp.text.split('\n').enumerate() {
                if k > 0 {
                    self.flush_line(&mut line);
                }
                let mut rest = hard;
                while !rest.is_empty() {
                    let ws = rest.len() - rest.trim_start().len();
                    if ws > 0 {
                        if line.x > line.x0 {
                            let sw = ui::width_code(" ", sz, code) * ws.min(4) as f32;
                            self.place(&mut line, " ", sw, sz, code, run_ink, style, link.is_some());
                        }
                        rest = &rest[ws..];
                        continue;
                    }
                    let end = rest.find(char::is_whitespace).unwrap_or(rest.len());
                    let word = &rest[..end];
                    rest = &rest[end..];
                    let mut word = word.to_string();
                    loop {
                        let ww = ui::width_code(&word, sz, code);
                        if line.x + ww <= line.x0 + line.w + 0.5 {
                            self.place(&mut line, &word, ww, sz, code, run_ink, style, link.is_some());
                            break;
                        }
                        if line.x > line.x0 {
                            self.flush_line(&mut line);
                            continue;
                        }
                        // One word wider than the line: as much as fits, the rest below.
                        let n = fit(&word, line.w, sz, code).max(word.chars().next().map_or(1, char::len_utf8));
                        let head = word[..n].to_string();
                        let hw = ui::width_code(&head, sz, code);
                        self.place(&mut line, &head, hw, sz, code, run_ink, style, link.is_some());
                        self.flush_line(&mut line);
                        word = word[n..].to_string();
                        if word.is_empty() {
                            break;
                        }
                    }
                }
            }
        }
        self.flush_line(&mut line);
        line.y
    }

    #[allow(clippy::too_many_arguments)]
    fn place(&mut self, line: &mut LineBuf, s: &str, w: f32, size: i32, code: i32, ink: Ink, style: u8, link: bool) {
        let spoiler = style & SPOILER != 0;
        let y_off = if style & SUP != 0 { -(line.size as f32 * 0.12) } else { (line.size - size) as f32 * 0.55 };
        // Merge with the previous run of the same look.
        if let Some(Item::Text { s: ps, w: pw, size: psz, code: pc, ink: pi, strike, underline, spoiler: psp, x, .. }) = line.pending.last_mut() {
            if *psz == size && *pc == code && *pi == ink && *strike == (style & STRIKE != 0) && *underline == link && *psp == spoiler && (*x + *pw - line.x).abs() < 0.5 {
                ps.push_str(s);
                *pw += w;
                line.x += w;
                return;
            }
        }
        if s == " " && line.pending.is_empty() {
            return;
        }
        line.pending.push(Item::Text {
            s: s.to_string(),
            x: line.x,
            y: line.y + y_off,
            size,
            code,
            ink,
            strike: style & STRIKE != 0,
            underline: link,
            spoiler,
            w,
        });
        line.x += w;
    }

    fn flush_line(&mut self, line: &mut LineBuf) {
        let lh = line_h(line.size);
        for it in line.pending.drain(..) {
            if let Item::Text { s, x, y, size, w, ink, spoiler, .. } = &it {
                let trimmed = s.trim_end();
                if *ink == Ink::Code && !trimmed.is_empty() {
                    self.out.items.push(Item::Fill { r: Rect::new(*x - 3.0, *y - 1.0, *w + 6.0, *size as f32 * 1.3), kind: FillKind::CodeBg });
                }
                if *spoiler && !trimmed.is_empty() {
                    self.out.items.push(Item::Fill { r: Rect::new(*x - 2.0, *y, *w + 4.0, *size as f32 * 1.3), kind: FillKind::Spoiler });
                }
            }
            self.out.items.push(it);
        }
        line.y += lh;
        line.x = line.x0;
    }

    fn blocks(&mut self, blocks: &[Block], x0: f32, w: f32, mut y: f32, ink: Ink) -> f32 {
        let size = self.base;
        let gap = (size as f32 * 0.55).round();
        for (k, b) in blocks.iter().enumerate() {
            if k > 0 {
                y += gap;
            }
            match b {
                Block::Para(s) => y = self.spans(s, x0, w, y, size, ink, false),
                Block::Heading(level, s) => {
                    let k = match level {
                        1 => 1.45,
                        2 => 1.3,
                        3 => 1.18,
                        _ => 1.08,
                    };
                    y = self.spans(s, x0, w, y, ((size as f32 * k) as i32).min(79), Ink::Heading, true);
                }
                Block::Quote(inner) => {
                    let top = y;
                    let indent = (size as f32 * 1.1).round();
                    y = self.blocks(inner, x0 + indent, w - indent, y, Ink::Dim);
                    self.out.items.push(Item::Fill { r: Rect::new(x0 + 2.0, top, 5.0, (y - top - 4.0).max(8.0)), kind: FillKind::QuoteBar });
                }
                Block::List { ordered, start, items } => {
                    let marker_w = if *ordered { ui::width_code("00.", size, ui::font_code(false)) + size as f32 * 0.5 } else { size as f32 * 1.3 };
                    for (n, it) in items.iter().enumerate() {
                        if n > 0 {
                            y += gap * 0.4;
                        }
                        let marker = if *ordered { format!("{}.", *start as usize + n) } else { "\u{2022}".into() };
                        self.out.items.push(Item::Text {
                            s: marker,
                            x: x0 + size as f32 * 0.2,
                            y,
                            size,
                            code: ui::font_code(*ordered),
                            ink: Ink::Dim,
                            strike: false,
                            underline: false,
                            spoiler: false,
                            w: 0.0,
                        });
                        let before = y;
                        y = self.blocks(it, x0 + marker_w, w - marker_w, y, ink);
                        if (y - before).abs() < 1.0 {
                            y += line_h(size);
                        }
                    }
                }
                Block::Code(text) => {
                    let sz = ((size as f32) * 0.85) as i32;
                    let code = ui::mono_code(false);
                    let lh = line_h(sz);
                    let lines: Vec<&str> = text.split('\n').collect();
                    let pad = 12.0;
                    let h = lines.len() as f32 * lh + 2.0 * pad;
                    self.out.items.push(Item::Fill { r: Rect::new(x0, y, w, h), kind: FillKind::CodeBg });
                    for (n, l) in lines.iter().enumerate() {
                        let l = l.replace('\t', "    ");
                        let cut = fit(&l, w - 2.0 * pad, sz, code);
                        let shown = &l[..cut];
                        if shown.trim().is_empty() {
                            continue;
                        }
                        self.out.items.push(Item::Text {
                            s: shown.to_string(),
                            x: x0 + pad,
                            y: y + pad + n as f32 * lh,
                            size: sz,
                            code,
                            ink: Ink::Code,
                            strike: false,
                            underline: false,
                            spoiler: false,
                            w: 0.0,
                        });
                    }
                    y += h;
                }
                Block::Rule => {
                    y += gap * 0.5;
                    self.out.items.push(Item::Fill { r: Rect::new(x0, y, w, 2.0), kind: FillKind::Rule });
                    y += gap * 0.5 + 2.0;
                }
                Block::Table { head, rows } => {
                    let cols = head.len().max(rows.iter().map(Vec::len).max().unwrap_or(0)).max(1);
                    let cw = w / cols as f32;
                    let pad = 8.0;
                    let sz = ((size as f32) * 0.88) as i32;
                    let saved = self.base;
                    self.base = sz;
                    let mut row = |ctx: &mut Ctx, cells: &[Vec<Span>], y: f32, bold: bool| -> f32 {
                        let mut bottom = y + line_h(sz);
                        for (c, cell) in cells.iter().enumerate().take(cols) {
                            let cell: Vec<Span> = cell.iter().map(|s| Span { style: s.style | if bold { BOLD } else { 0 }, ..s.clone() }).collect();
                            let b = ctx.spans(&cell, x0 + c as f32 * cw + pad, cw - 2.0 * pad, y + pad / 2.0, sz, Ink::Body, false);
                            bottom = bottom.max(b + pad / 2.0);
                        }
                        bottom
                    };
                    let top = y;
                    let hb = row(self, head, y, true);
                    self.out.items.push(Item::Fill { r: Rect::new(x0, top, w, hb - top), kind: FillKind::TableHead });
                    y = hb;
                    for r in rows {
                        self.out.items.push(Item::Fill { r: Rect::new(x0, y, w, 1.5), kind: FillKind::TableLine });
                        y = row(self, r, y, false);
                    }
                    self.base = saved;
                }
            }
        }
        y
    }
}

/// The longest start of `s` (bytes, at a char boundary) no wider than `w`.
fn fit(s: &str, w: f32, size: i32, code: i32) -> usize {
    let ends: Vec<usize> = s.char_indices().map(|(i, c)| i + c.len_utf8()).collect();
    let (mut lo, mut hi) = (0usize, ends.len());
    while lo < hi {
        let mid = (lo + hi).div_ceil(2);
        if ui::width_code(&s[..ends[mid - 1]], size, code) <= w {
            lo = mid;
        } else {
            hi = mid - 1;
        }
    }
    if lo == 0 { 0 } else { ends[lo - 1] }
}

/// Lays out `blocks` `w` px wide at text size `size`.
pub fn layout(blocks: &[Block], w: f32, size: i32) -> Laid {
    let mut c = Ctx { out: Laid::default(), base: size };
    let h = c.blocks(blocks, 0.0, w, 0.0, Ink::Body);
    c.out.h = h.max(0.0);
    c.out
}

thread_local! {
    static CACHE: RefCell<HashMap<(String, u32, i32, i32), Rc<Laid>>> = RefCell::new(HashMap::new());
}

/// [`layout`] of markdown `md`, cached under `key` (a comment's or post's ID).
pub fn cached(key: &str, md: &str, w: f32, size: i32) -> Rc<Laid> {
    let k = (key.to_string(), w.round() as u32, size, ui::font_code(false));
    if let Some(l) = CACHE.with(|c| c.borrow().get(&k).cloned()) {
        return l;
    }
    let laid = Rc::new(layout(&markdown::parse(md), w, size));
    CACHE.with(|c| {
        let mut c = c.borrow_mut();
        if c.len() > 3000 {
            c.clear();
        }
        c.insert(k, laid.clone());
    });
    laid
}

/// Plain text (titles) laid out like a paragraph: wrapped lines, at most `max` of them; the last
/// shown line fades out when cut. Returns the lines.
pub fn wrap_title(s: &str, w: f32, size: i32, bold: bool, max: usize) -> Vec<String> {
    thread_local! {
        static T: RefCell<HashMap<(String, u32, i32, bool, usize, i32), Vec<String>>> = RefCell::new(HashMap::new());
    }
    let k = (s.to_string(), w.round() as u32, size, bold, max, ui::font_code(false));
    if let Some(v) = T.with(|t| t.borrow().get(&k).cloned()) {
        return v;
    }
    let code = ui::font_code(bold);
    let mut lines: Vec<String> = Vec::new();
    let mut rest = s.trim();
    while !rest.is_empty() && lines.len() < max {
        if lines.len() + 1 == max || ui::width_code(rest, size, code) <= w {
            lines.push(rest.to_string());
            break;
        }
        let mut line = String::new();
        let mut used = 0;
        for word in rest.split_inclusive(' ') {
            let trial = format!("{line}{word}");
            if !line.is_empty() && ui::width_code(trial.trim_end(), size, code) > w {
                break;
            }
            line = trial;
            used += word.len();
        }
        if line.trim().is_empty() || ui::width_code(line.trim_end(), size, code) > w {
            let n = fit(rest, w, size, code).max(rest.chars().next().map_or(1, char::len_utf8));
            lines.push(rest[..n].to_string());
            rest = rest[n..].trim_start();
            continue;
        }
        lines.push(line.trim_end().to_string());
        rest = rest[used..].trim_start();
    }
    T.with(|t| {
        let mut t = t.borrow_mut();
        if t.len() > 4000 {
            t.clear();
        }
        t.insert(k, lines.clone());
    });
    lines
}

/// Draws wrapped title lines from `y`, the last one fading where it is cut. Returns the y below.
#[allow(clippy::too_many_arguments)]
pub fn draw_lines(p: Painter, m: &dyn plx_machine::machine::Measure, lines: &[String], x: f32, y: f32, w: f32, size: i32, col: [f32; 4], bold: bool) -> f32 {
    let lh = line_h(size);
    for (i, l) in lines.iter().enumerate() {
        ui::text_cut(p, m, l, x, y + i as f32 * lh, w, size, col, ui::ALIGN_LEFT, bold);
    }
    y + lines.len() as f32 * lh
}

pub fn title_h(lines: usize, size: i32) -> f32 {
    lines as f32 * line_h(size)
}

/// Draws `laid` with its top-left at (x, y), skipping what lies outside `clip`'s rows.
pub fn draw(p: Painter, laid: &Laid, x: f32, y: f32, clip: Rect, body: [f32; 4], reveal_spoilers: bool) {
    let accent = ui::accent();
    let dim = [body[0], body[1], body[2], body[3] * 0.72];
    let code_ink = [0.86, 0.9, 1.0, body[3]];
    let lift = ui::surface(0.12);
    for it in &laid.items {
        match it {
            Item::Fill { r, kind } => {
                let r = Rect::new(r.x + x, r.y + y, r.w, r.h);
                if r.y > clip.y + clip.h || r.y + r.h < clip.y {
                    continue;
                }
                match kind {
                    FillKind::CodeBg => ui::fill(p, r, 6.0, [lift[0], lift[1], lift[2], 0.9]),
                    FillKind::QuoteBar => ui::fill(p, r, 2.5, ui::with_alpha(accent, 0.55)),
                    FillKind::Rule => ui::fill(p, r, 1.0, [1.0, 1.0, 1.0, 0.18]),
                    FillKind::Spoiler if !reveal_spoilers => ui::fill(p, r, 4.0, [0.24, 0.24, 0.27, 1.0]),
                    FillKind::Spoiler => ui::fill(p, r, 4.0, [1.0, 1.0, 1.0, 0.08]),
                    FillKind::TableHead => ui::fill(p, r, 4.0, [1.0, 1.0, 1.0, 0.07]),
                    FillKind::TableLine => ui::fill(p, r, 0.0, [1.0, 1.0, 1.0, 0.12]),
                }
            }
            Item::Text { s, x: tx, y: ty, size, code, ink, strike, underline, spoiler, w } => {
                let (tx, ty) = (tx + x, ty + y);
                if ty > clip.y + clip.h || ty + *size as f32 * 1.4 < clip.y {
                    continue;
                }
                if *spoiler && !reveal_spoilers {
                    continue;
                }
                let col = match ink {
                    Ink::Body => body,
                    Ink::Dim => dim,
                    Ink::Link => [accent[0], accent[1], accent[2], body[3]],
                    Ink::Code => code_ink,
                    Ink::Heading => body,
                };
                ui::text_code(p, s, tx, ty, *size, col, ui::ALIGN_LEFT, *code);
                let w = if *w > 0.0 { *w } else { ui::width_code(s, *size, *code) };
                let tw = ui::width_code(s.trim_end(), *size, *code).min(w);
                if *strike {
                    ui::fill(p, Rect::new(tx, ty + *size as f32 * 0.62, tw, 2.0), 0.0, col);
                }
                if *underline {
                    ui::fill(p, Rect::new(tx, ty + *size as f32 * 1.18, tw, 2.0), 0.0, ui::with_alpha(col, 0.5));
                }
            }
        }
    }
}
