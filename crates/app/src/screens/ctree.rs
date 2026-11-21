//! A comment tree as rows: what each row shows, how tall it is and how it is drawn. Shared by the
//! post page's comment column and the two-pane preview.

use super::common;
use crate::host::{RdFx, RdHost};
use crate::i18n::tr;
use crate::images::{self, Fit};
use crate::prefs::Prefs;
use crate::richtext;
use crate::store::ThreadState;
use crate::ui::{self, icon::Kind as Icon};
use plx_machine::machine::{Effects, Fx};
use plx_ui::{Painter, Rect};
use rd_core::comments::{self, Row as CRow};
use std::collections::HashSet;

/// Depth rail colours.
pub const RAILS: [[f32; 4]; 6] = [
    [0.36, 0.58, 1.0, 0.8],
    [0.25, 0.78, 0.55, 0.8],
    [1.0, 0.72, 0.25, 0.8],
    [0.85, 0.4, 0.9, 0.8],
    [1.0, 0.45, 0.45, 0.8],
    [0.3, 0.8, 0.85, 0.8],
];

/// Height of a picture shown under a comment.
pub const COMMENT_PIC_H: f32 = 300.0;

/// One comment row, as drawn: what it is and the data the drawing needs (owned, so it outlives
/// the borrow of the store).
#[derive(Clone)]
pub enum CItem {
    Comment {
        name: String,
        author: String,
        body: String,
        score: i64,
        score_hidden: bool,
        created: i64,
        depth: u32,
        op: bool,
        mod_: bool,
        likes: Option<bool>,
        collapsed: bool,
        hidden: usize,
        flair: Option<String>,
        stickied: bool,
        saved: bool,
        /// Written since the last visit, or brought in by a reload.
        new: bool,
        /// Brought in by a reload while the post was open (highlighted a moment).
        arrived: bool,
    },
    More { id: String, depth: u32, count: i64, cont: bool, parent: String },
}

impl CItem {
    pub fn depth(&self) -> u32 {
        match self {
            CItem::Comment { depth, .. } | CItem::More { depth, .. } => *depth,
        }
    }
}

/// The rows of thread `t`: `op` is the post's author, `me` the signed-in user, `since` the last
/// visit (comments after it are new), `arrived_before` the reloaded comments seen before.
pub fn rows(t: &ThreadState, op: &str, me: &str, since: Option<i64>, arrived_before: &HashSet<String>) -> Vec<CItem> {
    comments::flatten(&t.tree, &t.toggled)
        .into_iter()
        .map(|r| match r {
            CRow::Comment { c, depth, collapsed, hidden_replies } => {
                let arrived = t.arrived.contains(&c.name) && !arrived_before.contains(&c.name);
                // The user's own comments are never news to them.
                let mine = !me.is_empty() && c.author.eq_ignore_ascii_case(me);
                CItem::Comment {
                    name: c.name.clone(),
                    author: c.author.clone(),
                    body: c.body.clone(),
                    score: c.score,
                    score_hidden: c.score_hidden,
                    created: c.created_utc,
                    depth,
                    op: c.is_submitter || (!op.is_empty() && c.author == op),
                    mod_: c.distinguished.as_deref() == Some("moderator"),
                    likes: c.likes,
                    collapsed,
                    hidden: hidden_replies,
                    flair: c.author_flair_text.clone(),
                    stickied: c.stickied,
                    saved: c.saved,
                    new: !mine && (arrived || since.is_some_and(|s| c.created_utc > s)),
                    arrived,
                }
            }
            CRow::More { m, depth } => CItem::More { id: m.id.clone(), depth, count: m.count, cont: m.is_continue_thread(), parent: m.parent_id.clone() },
        })
        .collect()
}

/// A comment's layout cache key: its name and text (a reload may bring an edit).
pub fn laid_key(name: &str, body: &str) -> String {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    body.hash(&mut h);
    format!("{name}:{:x}", h.finish())
}

pub fn indent(depth: u32) -> f32 {
    depth.min(10) as f32 * ui::ls(26.0).min(34.0)
}

/// The height of row `c` in a column `w` wide.
pub fn height(c: &CItem, w: f32) -> f32 {
    let ms = ui::sz(21);
    match c {
        CItem::Comment { name, body, depth, collapsed, .. } => {
            let x = indent(*depth) + 20.0;
            let head = ms as f32 * 1.42;
            if *collapsed {
                return head + 2.0 * 12.0;
            }
            let laid = richtext::cached(&laid_key(name, body), body, w - x - 34.0, ui::csz(25));
            let pics = comment_pictures(body).len() as f32;
            12.0 + head + 6.0 + laid.h + pics * (COMMENT_PIC_H + 12.0) + 14.0
        }
        CItem::More { .. } => ms as f32 * 1.42 + 24.0,
    }
}

/// The row of the parent of row `i`.
pub fn parent_row(rows: &[CItem], i: usize) -> Option<usize> {
    let depth = rows.get(i)?.depth();
    if depth == 0 {
        return None;
    }
    (0..i).rev().find(|&j| matches!(&rows[j], CItem::Comment { depth: d, .. } if *d + 1 == depth))
}

/// The next row at the same or a shallower depth (skips a thread).
pub fn next_sibling(rows: &[CItem], i: usize) -> Option<usize> {
    let depth = rows.get(i)?.depth();
    ((i + 1)..rows.len()).find(|&j| rows[j].depth() <= depth)
}

/// Votes on comment `c` (the same vote again takes it back).
pub fn vote(c: Option<CItem>, dir: i8, fx: &mut Effects<'_, RdHost>) {
    if let Some(CItem::Comment { name, likes, .. }) = c {
        let d = match (dir, likes) {
            (1, Some(true)) | (-1, Some(false)) => 0,
            _ => dir,
        };
        fx.push(Fx::App(RdFx::Vote { fullname: name, dir: d }));
    }
}

/// Saves comment `c`, or unsaves it.
pub fn save(c: Option<CItem>, fx: &mut Effects<'_, RdHost>) {
    if let Some(CItem::Comment { name, saved, .. }) = c {
        fx.push(Fx::App(RdFx::Save { fullname: name, on: !saved }));
    }
}

/// Pictures a comment links to (Reddit and Imgur uploads), at most two: shown under its text.
pub fn comment_pictures(body: &str) -> Vec<String> {
    if !body.contains("redd.it/") && !body.contains("imgur.com/") {
        return Vec::new();
    }
    let blocks = rd_core::markdown::parse(body);
    rd_core::markdown::links(&blocks)
        .into_iter()
        .map(|(u, _)| rd_core::media::unwrap_media_url(&u))
        .filter(|u| {
            let host = rd_core::media::host(u);
            let path = u.split(['?', '#']).next().unwrap_or("").to_ascii_lowercase();
            // A GIF's still from Reddit is its .gif address asked for as a PNG or JPEG.
            let still = path.ends_with(".gif") && (u.contains("format=png") || u.contains("format=pjpg") || u.contains("format=jpg"));
            let pic = still || [".jpg", ".jpeg", ".png", ".webp"].iter().any(|e| path.ends_with(e));
            pic && matches!(host.as_str(), "i.redd.it" | "preview.redd.it" | "external-preview.redd.it" | "i.imgur.com")
        })
        .take(2)
        .collect()
}

/// How one row is drawn besides its data.
pub struct Look {
    /// Focused, and how far its highlight has eased in (0..1).
    pub focus: Option<f32>,
    /// A reloaded comment's fading glow (0: none).
    pub glow: f32,
    /// The comment a link pointed at.
    pub target: bool,
    /// Its spoilers are revealed.
    pub revealed: bool,
    /// A "load more" row whose load is in flight.
    pub loading: bool,
    /// The column it is clipped to.
    pub clip: Rect,
    pub now_ms: u32,
}

/// Draws row `c` in `r`.
pub fn draw(p: Painter, m: &dyn plx_machine::machine::Measure, prefs: &Prefs, c: &CItem, r: Rect, look: &Look) {
    let ms = ui::sz(21);
    let depth = c.depth();
    let x0 = r.x + indent(depth);
    // Depth rails.
    for d in 0..depth.min(10) {
        let rx = r.x + indent(d) + 6.0;
        ui::fill(p, Rect::new(rx, r.y - 6.0, 3.0, r.h + 12.0), 1.5, ui::with_alpha(RAILS[d as usize % RAILS.len()], 0.45));
    }
    let card = Rect::new(x0, r.y, r.x + r.w - x0, r.h);
    if let Some(t) = look.focus {
        ui::fill(p, card, 14.0, ui::surface(0.05 + 0.09 * t));
        p.rring(card, 14.0, 3.0, ui::with_alpha(ui::accent(), 0.25 + 0.65 * t));
    } else {
        ui::fill(p, card, 14.0, ui::surface(0.05));
    }
    if look.glow > 0.0 {
        ui::fill(p, card, 14.0, ui::with_alpha(ui::accent(), 0.18 * look.glow));
    }
    // The comment a link pointed at stays marked.
    if look.focus.is_none() && look.target {
        ui::fill(p, card, 14.0, ui::with_alpha(ui::accent(), 0.08));
        p.rring(card, 14.0, 2.0, ui::with_alpha(ui::accent(), 0.5));
    }
    let rail = RAILS[depth as usize % RAILS.len()];
    ui::fill(p, Rect::new(card.x, card.y + 10.0, 5.0, card.h - 20.0), 2.5, rail);
    let x = card.x + 20.0;
    match c {
        CItem::Comment { name, author, body, score, score_hidden, created, op, mod_, likes, collapsed, hidden, flair, stickied, saved, new, .. } => {
            let y = card.y + 12.0;
            let mut hx = x;
            let author_col = if *mod_ {
                [0.3, 0.85, 0.45, 1.0]
            } else if *op {
                [0.45, 0.65, 1.0, 1.0]
            } else {
                [1.0, 1.0, 1.0, 0.9]
            };
            hx += ui::text_raw(p, author, hx, y, ms, author_col, ui::ALIGN_LEFT, true) + 10.0;
            let bs = (ms as f32 * 0.9) as i32;
            if *new {
                hx += common::badge(p, m, hx, y + 2.0, tr("NEW"), ui::accent(), ui::WHITE, bs) + 8.0;
            }
            if *op {
                hx += common::badge(p, m, hx, y + 2.0, "OP", [0.25, 0.45, 0.9, 1.0], ui::WHITE, bs) + 8.0;
            }
            if *mod_ {
                hx += common::badge(p, m, hx, y + 2.0, "MOD", [0.15, 0.6, 0.3, 1.0], ui::WHITE, bs) + 8.0;
            }
            if *stickied {
                hx += common::badge(p, m, hx, y + 2.0, "PINNED", [0.15, 0.6, 0.3, 1.0], ui::WHITE, bs) + 8.0;
            }
            if let (Some(fl), true) = (flair, prefs.on("show_flair")) {
                let fl: String = fl.chars().take(30).collect();
                hx += common::badge(p, m, hx, y + 2.0, &fl, [1.0, 1.0, 1.0, 0.12], ui::DIM, bs) + 8.0;
            }
            if *saved {
                let s = ms as f32 * 1.05;
                ui::icon::draw(p, Icon::Star, Rect::new(hx, y + ms as f32 * 0.12, s, s), common::SAVED_STAR, true);
                hx += s + 8.0;
            }
            let mut meta = Vec::new();
            if prefs.on("show_scores") {
                meta.push(if *score_hidden { "\u{2022} points".to_string() } else { format!("{} points", common::short_count(*score)) });
            }
            meta.push(rd_core::listing::age(*created, rd_core::now_secs() as i64));
            if *collapsed && *hidden > 0 {
                meta.push(format!("+{hidden} hidden"));
            }
            let col = match likes {
                Some(true) => ui::UPVOTE,
                Some(false) => ui::DOWNVOTE,
                None => ui::FAINT,
            };
            ui::text_cut(p, m, &meta.join("  \u{b7}  "), hx + 6.0, y, card.x + card.w - hx - 20.0, ms, col, ui::ALIGN_LEFT, false);
            if !*collapsed {
                // The same width `height` laid it out at.
                let laid = richtext::cached(&laid_key(name, body), body, r.x + r.w - x - 34.0, ui::csz(25));
                let ty = y + ms as f32 * 1.42 + 6.0;
                richtext::draw(p, &laid, x, ty, look.clip, [0.93, 0.93, 0.95, 1.0], look.revealed);
                // Pictures linked in the comment, under its text.
                let mut py = ty + laid.h + 10.0;
                for url in comment_pictures(body) {
                    let box_ = Rect::new(x, py, (r.x + r.w - x - 34.0).min(COMMENT_PIC_H * 1.8), COMMENT_PIC_H);
                    ui::fill(p, box_, 10.0, [0.0, 0.0, 0.0, 0.35]);
                    if let images::Drawn::Loading = images::draw(p, images::THUMB, &url, box_, 10.0, Fit::Contain, ui::WHITE) {
                        ui::spinner(p, look.now_ms, box_.x + box_.w / 2.0, box_.y + box_.h / 2.0);
                    }
                    py += COMMENT_PIC_H + 12.0;
                }
            }
        }
        CItem::More { count, cont, .. } => {
            let label = if look.loading {
                "Loading\u{2026}".to_string()
            } else if *cont {
                "Continue this thread \u{203a}".to_string()
            } else if *count > 0 {
                format!("Load {} more {}", count, if *count == 1 { "reply" } else { "replies" })
            } else {
                "Load more replies".to_string()
            };
            ui::text_raw(p, &label, x, card.y + 12.0, ms, ui::accent(), ui::ALIGN_LEFT, true);
        }
    }
}
