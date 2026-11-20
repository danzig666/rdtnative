//! Pieces several screens share: buttons, badges, the post meta line, the toast, the clock,
//! colour keys and BACK, and empty or error states.

use crate::host::RdHost;
use crate::store::PostItem;
use crate::ui::{self, icon::Kind as Icon};
use plx_machine::machine::{Canon, Cx, Edge, InputEvent, InputKind, Key, LogicalState, ScreenEvent};
use plx_ui::{Painter, Rect};

/// OK on a `Control` element (`VList` rows, the keyboard) arms a press in the dispatcher, and its
/// release is delivered as `PressCommit`, never as `Activate`, which only a `Bare` element gets
/// (plx_ui `screen::ElemKind`). The screens are written against `Activate`, so this turns a commit
/// on one of `mine`'s elements into `Activate` of the focused element (from ytnative).
pub fn press_commit_as_activate(ev: &ScreenEvent<RdHost>, cx: &Cx<'_, RdHost>, mine: impl Fn(u32) -> bool) -> Option<ScreenEvent<RdHost>> {
    match ev {
        ScreenEvent::PressCommit(_) => cx.focus.current.map(|k| k.elem).filter(|e| mine(*e)).map(ScreenEvent::Activate),
        _ => None,
    }
}

/// A long press on OK: the focused element (cards open their menu).
pub fn press_hold(ev: &ScreenEvent<RdHost>, cx: &Cx<'_, RdHost>) -> Option<u32> {
    match ev {
        ScreenEvent::PressHold(_) => cx.focus.current.map(|k| k.elem),
        _ => None,
    }
}

/// The remote's colour buttons.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Color {
    Red,
    Green,
    Yellow,
    Blue,
}

/// A colour button pressed (LG codes 403-406; F1-F4 or 1-4 in the simulator).
pub fn color_key(ev: &ScreenEvent<RdHost>) -> Option<Color> {
    let ScreenEvent::Input(InputEvent { kind: InputKind::Key { key: Key::Other, sym, wcode, edge: Edge::Down, .. }, .. }) = ev else {
        return None;
    };
    let by_code = match wcode {
        403 => Some(Color::Red),
        404 => Some(Color::Green),
        405 => Some(Color::Yellow),
        406 => Some(Color::Blue),
        _ => None,
    };
    by_code.or(match *sym {
        0x4000_003A => Some(Color::Red),
        0x4000_003B => Some(Color::Green),
        0x4000_003C => Some(Color::Yellow),
        0x4000_003D => Some(Color::Blue),
        _ => None,
    })
}

/// A smooth wheel step (the pointer edge glide): rows, > 0 toward the top.
pub fn wheel_dy(ev: &ScreenEvent<RdHost>) -> Option<f32> {
    match ev {
        ScreenEvent::Input(InputEvent { kind: InputKind::Wheel { dy }, .. }) => Some(*dy),
        _ => None,
    }
}

/// BACK went down.
pub fn back_down(ev: &ScreenEvent<RdHost>) -> bool {
    matches!(ev, ScreenEvent::Input(InputEvent { kind: InputKind::Key { key: Key::Back, edge: Edge::Down, .. }, .. }))
}

/// Any BACK edge (screens that take BACK swallow the up edge too).
pub fn back_any(ev: &ScreenEvent<RdHost>) -> bool {
    matches!(ev, ScreenEvent::Input(InputEvent { kind: InputKind::Key { key: Key::Back, .. }, .. }))
}

/// The app's wordmark: an accent disc with a white "r" and "rdtnative".
pub fn logo(p: Painter, x: f32, y: f32) {
    ui::fill(p, Rect::new(x, y, 44.0, 44.0), 22.0, ui::accent());
    ui::text_raw(p, "r", x + 22.0, y + 2.0, 30, ui::WHITE, ui::ALIGN_CENTER, true);
    ui::text_raw(p, "rdtnative", x + 58.0, y + 2.0, 32, ui::WHITE, ui::ALIGN_LEFT, true);
}

/// A transient message at the bottom of the screen.
pub fn toast(p: Painter, m: &dyn plx_machine::machine::Measure, cx: &Cx<'_, RdHost>) {
    let Some((msg, until)) = &cx.views.store.toast else { return };
    let now = cx.views.now_ms;
    let shown = now.wrapping_sub(until.wrapping_sub(3500)) as f32;
    let left = until.wrapping_sub(now) as f32;
    let a = ui::ease_out(shown / 220.0).min(ui::ease_out(left / 320.0)).clamp(0.0, 1.0);
    if a < 1.0 {
        ui::animating();
    }
    let p = p.alpha(a);
    let rise = (1.0 - a) * 24.0;
    let p = p.translate(0.0, rise);
    let msg = crate::i18n::tr(msg);
    let size = ui::sz(28);
    let w = (ui::text_width(m, msg, size, false) + 80.0).min(1800.0);
    let h = ui::ls(70.0);
    let r = Rect::new((1920.0 - w) / 2.0, 1060.0 - h - 30.0, w, h);
    p.shadow(r, h / 2.0, 30.0, 8.0, [0.0, 0.0, 0.0, 0.5]);
    ui::fill(p, r, h / 2.0, [0.12, 0.12, 0.14, 0.97]);
    ui::text_cut(p, m, msg, 960.0, r.y + (h - size as f32 * 1.3) / 2.0, w - 60.0, size, ui::WHITE, ui::ALIGN_CENTER, false);
}

/// A screen's logical state for plx's probes: just a string.
pub struct State(pub String);

impl LogicalState for State {
    fn write(&self, c: &mut Canon) {
        c.str(&self.0);
    }
    fn probe(&self, out: &mut String) {
        out.push_str(&self.0);
    }
}

/// `1234567` -> `1.2M`, `15234` -> `15k`: scores and comment counts.
pub fn short_count(n: i64) -> String {
    rd_core::listing::short_count(n)
}

/// "1 comment", "12 comments", "1.2k comments".
pub fn comments(n: i64) -> String {
    if n == 1 {
        "1 comment".into()
    } else {
        format!("{} comments", short_count(n))
    }
}

/// " +3" when comments were added since the post was last opened, else nothing.
pub fn grown(item: &PostItem) -> String {
    if item.new_comments > 0 {
        format!(" +{}", short_count(item.new_comments))
    } else {
        String::new()
    }
}

/// A post's comment count with what was added since it was last opened: "12 comments +3".
pub fn comments_of(item: &PostItem) -> String {
    format!("{}{}", comments(item.post.num_comments), grown(item))
}

/// The star of a saved post or comment.
pub const SAVED_STAR: [f32; 4] = [1.0, 0.8, 0.2, 1.0];

/// A pill button: icon and label; accent-filled when focused, outlined when `active`.
#[allow(clippy::too_many_arguments)]
pub fn button(p: Painter, m: &dyn plx_machine::machine::Measure, r: Rect, label: &str, icon: Option<Icon>, focused: bool, active: bool, raw: bool) {
    let rad = r.h / 2.0;
    if focused {
        p.shadow(r, rad, 18.0, 4.0, [0.0, 0.0, 0.0, 0.45]);
        ui::fill(p, r, rad, [0.95, 0.95, 0.97, 1.0]);
    } else {
        ui::fill(p, r, rad, ui::surface(if active { 0.2 } else { 0.1 }));
        if active {
            p.rring(r, rad, 2.5, ui::with_alpha(ui::accent(), 0.9));
        }
    }
    let ink = if focused { [0.08, 0.08, 0.1, 1.0] } else { ui::WHITE };
    let size = ui::sz(24);
    let mut x = r.x + r.h * 0.45;
    let label_w = r.w - r.h * 0.9;
    if let Some(k) = icon {
        let s = size as f32 * 1.1;
        ui::icon::draw(p, k, Rect::new(x, r.y + (r.h - s) / 2.0, s, s), if focused { [0.85, 0.25, 0.05, 1.0] } else { ui::accent() }, active);
        x += s + 10.0;
    }
    let room = (r.x + r.w - r.h * 0.4 - x).max(10.0).min(label_w);
    let ty = r.y + (r.h - size as f32 * 1.3) / 2.0;
    let s = if raw { label.to_string() } else { crate::i18n::tr(label).to_string() };
    ui::text_cut(p, m, &s, x, ty, room, size, ink, ui::ALIGN_LEFT, focused || active);
}

/// Width a [`button`] needs.
pub fn button_w(m: &dyn plx_machine::machine::Measure, label: &str, icon: bool, h: f32) -> f32 {
    let size = ui::sz(24);
    let s = crate::i18n::tr(label);
    ui::text_width(m, s, size, true) + h * 0.9 + if icon { size as f32 * 1.1 + 10.0 } else { 0.0 }
}

/// A small coloured tag ("NSFW", "GALLERY 4", flair).
pub fn badge(p: Painter, m: &dyn plx_machine::machine::Measure, x: f32, y: f32, label: &str, bg: [f32; 4], ink: [f32; 4], size: i32) -> f32 {
    let w = ui::text_width(m, label, size, true) + size as f32 * 0.9;
    let h = size as f32 * 1.45;
    ui::fill(p, Rect::new(x, y, w, h), h * 0.3, bg);
    ui::text_raw(p, label, x + size as f32 * 0.45, y + (h - size as f32 * 1.3) / 2.0, size, ink, ui::ALIGN_LEFT, true);
    w
}

pub const NSFW_RED: [f32; 4] = [0.86, 0.15, 0.22, 1.0];
pub const SPOILER_GRAY: [f32; 4] = [0.35, 0.35, 0.4, 1.0];
pub const BADGE_BG: [f32; 4] = [0.0, 0.0, 0.0, 0.72];

/// What kind of media a post holds, for its badge ("VIDEO 0:31", "GALLERY 5", "LINK").
pub fn media_badge(item: &PostItem) -> Option<String> {
    use rd_core::media::Media;
    match &item.media {
        Media::Gallery(g) => Some(format!("\u{25A6} {}", g.len())),
        Media::Video(v) if v.gif => Some("GIF".into()),
        Media::Video(v) if v.duration > 0 => Some(format!("\u{25B6} {}", ui::fmt_time(v.duration as u64 * 1000))),
        Media::Video(_) => Some("\u{25B6} VIDEO".into()),
        Media::YouTube { .. } => Some("\u{25B6} YOUTUBE".into()),
        Media::Link { url, .. } => Some(format!("\u{2197} {}", rd_core::media::host(url))),
        Media::Imgur(r) => Some(r.service().into()),
        _ => None,
    }
}

/// The meta line of a post: "r/sub · u/author · 5h · domain".
pub fn post_meta(item: &PostItem, cx: &Cx<'_, RdHost>, with_sub: bool) -> String {
    let p = &item.post;
    let prefs = &cx.views.store.prefs;
    let mut parts: Vec<String> = Vec::new();
    if with_sub {
        parts.push(format!("r/{}", p.subreddit));
    }
    parts.push(format!("u/{}", p.author));
    parts.push(rd_core::listing::age(p.created_utc, rd_core::now_secs() as i64));
    if prefs.on("show_domain") && !p.is_self && !p.domain.is_empty() && !p.domain.starts_with("self.") && !p.domain.contains("redd.it") && !p.domain.contains("reddit.com") {
        parts.push(p.domain.clone());
    }
    parts.join("  \u{b7}  ")
}

/// Score and comment count with their icons, at (x, y); returns the width drawn.
#[allow(clippy::too_many_arguments)]
pub fn score_line(p: Painter, m: &dyn plx_machine::machine::Measure, item: &PostItem, x: f32, y: f32, size: i32, ink: [f32; 4], show_score: bool) -> f32 {
    let post = &item.post;
    let s = size as f32 * 1.05;
    let mut cx = x;
    if show_score {
        let (col, filled) = match post.likes {
            Some(true) => (ui::UPVOTE, true),
            Some(false) => (ui::DOWNVOTE, true),
            None => (ink, false),
        };
        let k = if post.likes == Some(false) { Icon::ThumbDown } else { Icon::ThumbUp };
        ui::icon::draw(p, k, Rect::new(cx, y + size as f32 * 0.12, s, s), col, filled);
        cx += s + 8.0;
        cx += ui::text_raw(p, &short_count(post.score), cx, y, size, col, ui::ALIGN_LEFT, true).max(ui::text_width(m, "0", size, true));
        cx += size as f32 * 1.1;
    }
    ui::icon::draw(p, Icon::Chat, Rect::new(cx, y + size as f32 * 0.12, s, s), ink, false);
    cx += s + 8.0;
    cx += ui::text_raw(p, &short_count(post.num_comments), cx, y, size, ink, ui::ALIGN_LEFT, false);
    if item.new_comments > 0 {
        cx += ui::text_raw(p, &grown(item), cx, y, size, ui::accent(), ui::ALIGN_LEFT, true);
    }
    if post.saved {
        cx += size as f32 * 1.1;
        ui::icon::draw(p, Icon::Star, Rect::new(cx, y + size as f32 * 0.12, s, s), SAVED_STAR, true);
        cx += s;
    }
    cx - x
}

/// The time, for the corner of a page.
pub fn clock(p: Painter, x: f32, y: f32) {
    let secs = rd_core::now_secs() as i64 + local_offset();
    let (h, m) = ((secs / 3600) % 24, (secs / 60) % 60);
    ui::text_raw(p, &format!("{h:02}:{m:02}"), x, y, ui::sz(34), ui::DIM, ui::ALIGN_RIGHT, true);
}

/// The TV's UTC offset in seconds (from `localtime` on the TV; the simulator asks the system).
fn local_offset() -> i64 {
    #[cfg(unix)]
    unsafe {
        let t = libc::time(std::ptr::null_mut());
        let mut tm: libc::tm = std::mem::zeroed();
        if !libc::localtime_r(&t, &mut tm).is_null() {
            return tm.tm_gmtoff as i64;
        }
    }
    0
}

/// A centred message with a smaller line under it (empty feeds, errors).
pub fn message(p: Painter, m: &dyn plx_machine::machine::Measure, r: Rect, title: &str, detail: &str) {
    let ts = ui::sz(36);
    let ds = ui::sz(26);
    let cy = r.y + r.h * 0.35;
    ui::text(p, title, r.x + r.w / 2.0, cy, ts, ui::WHITE, ui::ALIGN_CENTER, true);
    for (i, line) in ui::wrap_lines(m, detail, r.w.min(1200.0), ds, false, 5).iter().enumerate() {
        ui::text_raw(p, line, r.x + r.w / 2.0, cy + ts as f32 * 1.8 + i as f32 * ds as f32 * 1.45, ds, ui::DIM, ui::ALIGN_CENTER, false);
    }
}

/// A row of pill buttons laid out from `x`, `y`: their rects.
pub fn button_row(m: &dyn plx_machine::machine::Measure, labels: &[(&str, bool)], x: f32, y: f32, h: f32, gap: f32, right_align: Option<f32>) -> Vec<Rect> {
    let widths: Vec<f32> = labels.iter().map(|(l, icon)| button_w(m, l, *icon, h)).collect();
    let total: f32 = widths.iter().sum::<f32>() + gap * (widths.len().saturating_sub(1)) as f32;
    let mut cx = match right_align {
        Some(r) => r - total,
        None => x,
    };
    widths
        .iter()
        .map(|w| {
            let r = Rect::new(cx, y, *w, h);
            cx += w + gap;
            r
        })
        .collect()
}

#[cfg(test)]
mod tests {
    #[test]
    fn short_counts() {
        assert_eq!(super::short_count(999), "999");
        assert_eq!(super::short_count(1234), "1.2k");
        assert_eq!(super::short_count(15234), "15k");
        assert_eq!(super::short_count(2_500_000), "2.5M");
        assert_eq!(super::short_count(-12), "-12");
    }
}
