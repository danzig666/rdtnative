//! Pieces several screens share.

use crate::host::RdHost;
use crate::ui;
use plx_machine::machine::{Canon, Cx, LogicalState};
use plx_ui::{Painter, Rect};

/// OK on a `Control` element (`VList` rows, the keyboard) arms a press in the dispatcher, and its
/// release is delivered as `PressCommit`, never as `Activate`, which only a `Bare` element gets
/// (plx_ui `screen::ElemKind`). The screens are written against `Activate`, so this turns a commit
/// on one of `mine`'s elements into `Activate` of the focused element (from ytnative).
pub fn press_commit_as_activate(
    ev: &plx_machine::machine::ScreenEvent<RdHost>,
    cx: &Cx<'_, RdHost>,
    mine: impl Fn(u32) -> bool,
) -> Option<plx_machine::machine::ScreenEvent<RdHost>> {
    match ev {
        plx_machine::machine::ScreenEvent::PressCommit(_) => {
            cx.focus.current.map(|k| k.elem).filter(|e| mine(*e)).map(plx_machine::machine::ScreenEvent::Activate)
        }
        _ => None,
    }
}

/// The app's wordmark: an accent disc with a white "r" and "rdtnative".
pub fn logo(p: Painter, x: f32, y: f32) {
    ui::fill(p, Rect::new(x, y, 44.0, 44.0), 22.0, ui::accent());
    ui::text_raw(p, "r", x + 22.0, y + 2.0, 30, ui::WHITE, ui::ALIGN_CENTER, true);
    ui::text_raw(p, "rdtnative", x + 58.0, y + 2.0, 32, ui::WHITE, ui::ALIGN_LEFT, true);
}

/// A transient message at the bottom of the screen.
pub fn toast(p: Painter, m: &dyn plx_machine::machine::Measure, cx: &Cx<'_, RdHost>) {
    let Some((msg, _)) = &cx.views.store.toast else { return };
    let msg = crate::i18n::tr(msg);
    let w = ui::text_width(m, msg, 28, false) + 80.0;
    let r = Rect::new((1920.0 - w) / 2.0, 960.0, w, 70.0);
    ui::fill(p, r, 35.0, [0.08, 0.08, 0.1, 0.94]);
    ui::text(p, msg, 960.0, r.y + 19.0, 28, ui::WHITE, ui::ALIGN_CENTER, false);
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

/// `1234567` -> `1.2M`, `15234` -> `15.2k`: scores and comment counts on a list row.
pub fn short_count(n: i64) -> String {
    let a = n.unsigned_abs() as f64;
    let sign = if n < 0 { "-" } else { "" };
    if a >= 1_000_000.0 {
        format!("{sign}{:.1}M", a / 1_000_000.0)
    } else if a >= 10_000.0 {
        format!("{sign}{:.0}k", a / 1000.0)
    } else if a >= 1000.0 {
        format!("{sign}{:.1}k", a / 1000.0)
    } else {
        n.to_string()
    }
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
