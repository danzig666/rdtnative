//! A scrolling column or grid of focusable items of any height, the building block of the feed,
//! the comment list and the post page. It lays items out in rows, keeps the focused item in view
//! (a smooth glide when "Smooth scrolling" is on), pages through an item taller than the screen
//! before moving past it, and plugs into plx's focus engine (groups, neighbours, places, pointer
//! stops).

use crate::host::RdHost;
use plx_machine::machine::{Cx, EntryId, FocusKey, GroupId};
use plx_ui::screen::{AxisMask, Dir, EdgeRule, ElemKind, GroupKind, GroupSpec, Placed, Seat, Step};
use plx_ui::Rect;
use std::cell::{Cell, RefCell};

pub struct Flow {
    pub entry: EntryId,
    pub group: GroupId,
    pub base: u32,
    /// The viewport on screen.
    pub rect: Rect,
    pub cols: usize,
    /// Space between columns and between rows.
    pub gap: f32,
    pub row_gap: f32,
    /// Room above the first row and below the last (inside the scrolled content).
    pub pad_top: f32,
    pub pad_bottom: f32,
    /// What stays visible above / below a revealed item.
    pub margin: f32,
    scroll: Cell<f32>,
    target: Cell<f32>,
    /// Per item: (x offset, top, width, height) in content space.
    boxes: RefCell<Vec<Rect>>,
    content_h: Cell<f32>,
    last_ms: Cell<u32>,
    last_focus: Cell<Option<usize>>,
}

pub fn smooth() -> bool {
    SMOOTH.load(std::sync::atomic::Ordering::Relaxed)
}
pub static SMOOTH: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);

impl Flow {
    pub fn new(entry: EntryId, group: GroupId, base: u32, rect: Rect) -> Flow {
        Flow {
            entry,
            group,
            base,
            rect,
            cols: 1,
            gap: 24.0,
            row_gap: 24.0,
            pad_top: 0.0,
            pad_bottom: 40.0,
            margin: 60.0,
            scroll: Cell::new(0.0),
            target: Cell::new(0.0),
            boxes: RefCell::new(Vec::new()),
            content_h: Cell::new(0.0),
            last_ms: Cell::new(0),
            last_focus: Cell::new(None),
        }
    }

    pub fn col_w(&self) -> f32 {
        ((self.rect.w - (self.cols as f32 - 1.0) * self.gap) / self.cols as f32).floor()
    }

    /// Lays out items with these heights (a row is as tall as its tallest item).
    pub fn set_heights(&self, heights: &[f32]) {
        let cw = self.col_w();
        let mut boxes = Vec::with_capacity(heights.len());
        let mut y = self.pad_top;
        for row in heights.chunks(self.cols.max(1)) {
            let rh = row.iter().cloned().fold(0.0, f32::max);
            for (c, h) in row.iter().enumerate() {
                boxes.push(Rect::new(c as f32 * (cw + self.gap), y, cw, *h));
            }
            y += rh + self.row_gap;
        }
        let h = if heights.is_empty() { 0.0 } else { y - self.row_gap + self.pad_bottom };
        self.content_h.set(h);
        *self.boxes.borrow_mut() = boxes;
        // Content shrank under the scroll (a reload): stay within it.
        let max = self.max_scroll();
        if self.target.get() > max {
            self.target.set(max);
        }
        if self.scroll.get() > max {
            self.scroll.set(max);
        }
    }

    pub fn len(&self) -> usize {
        self.boxes.borrow().len()
    }

    pub fn max_scroll(&self) -> f32 {
        (self.content_h.get() - self.rect.h).max(0.0)
    }

    pub fn scroll(&self) -> f32 {
        self.scroll.get()
    }

    /// Item `i` on screen (at the current scroll).
    pub fn screen_rect(&self, i: usize) -> Option<Rect> {
        let b = *self.boxes.borrow().get(i)?;
        Some(Rect::new(self.rect.x + b.x, self.rect.y + b.y - self.scroll.get(), b.w, b.h))
    }

    /// Item `i` where it rests once the scroll settles.
    fn target_rect(&self, i: usize) -> Option<Rect> {
        let b = *self.boxes.borrow().get(i)?;
        Some(Rect::new(self.rect.x + b.x, self.rect.y + b.y - self.target.get(), b.w, b.h))
    }

    /// Items at least partly on screen.
    pub fn visible(&self) -> std::ops::Range<usize> {
        let boxes = self.boxes.borrow();
        let (top, bot) = (self.scroll.get() - 10.0, self.scroll.get() + self.rect.h + 10.0);
        let first = boxes.iter().position(|b| b.y + b.h >= top).unwrap_or(boxes.len());
        let last = boxes.iter().rposition(|b| b.y <= bot).map_or(first, |l| l + 1);
        first..last.max(first)
    }

    /// Scrolls so item `i` is in view (its top when it is taller than the viewport).
    pub fn reveal(&self, i: usize) {
        let Some(b) = self.boxes.borrow().get(i).copied() else { return };
        let (top, h) = (b.y, b.h);
        let view = self.rect.h;
        let cur = self.target.get();
        let m = self.margin.min(view / 4.0);
        let t = if h + 2.0 * m >= view {
            top - m.min(20.0)
        } else if top - m < cur {
            top - m
        } else if top + h + m > cur + view {
            top + h + m - view
        } else {
            cur
        };
        self.set_target(t);
    }

    pub fn set_target(&self, t: f32) {
        let t = t.clamp(0.0, self.max_scroll());
        self.target.set(t);
        if !smooth() {
            self.scroll.set(t);
        }
        crate::ui::animating();
    }

    pub fn scroll_by(&self, d: f32) {
        self.set_target(self.target.get() + d);
    }

    pub fn to_top(&self) {
        self.target.set(0.0);
        self.scroll.set(0.0);
    }

    /// Steps the glide; call once per drawn frame.
    pub fn animate(&self, now_ms: u32) {
        let dt = now_ms.wrapping_sub(self.last_ms.get()).min(80) as f32;
        self.last_ms.set(now_ms);
        let (s, t) = (self.scroll.get(), self.target.get());
        if (s - t).abs() < 0.5 {
            self.scroll.set(t);
            return;
        }
        let k = 1.0 - (-dt / 70.0).exp();
        self.scroll.set(s + (t - s) * k);
        crate::ui::animating();
    }

    pub fn elem(&self, i: usize) -> u32 {
        self.base + i as u32
    }
    pub fn key(&self, i: usize) -> FocusKey<u32> {
        FocusKey { entry: self.entry, elem: self.elem(i) }
    }
    pub fn index(&self, e: u32) -> Option<usize> {
        e.checked_sub(self.base).map(|i| i as usize).filter(|&i| i < self.len())
    }
    pub fn owns(&self, e: u32) -> bool {
        e >= self.base && e < self.base + 0x0100_0000
    }
    pub fn focused(&self, cx: &Cx<'_, RdHost>) -> Option<usize> {
        cx.focus.current.filter(|k| k.entry == self.entry).and_then(|k| self.index(k.elem))
    }

    /// Focus moved to `i`: keys reveal it; the pointer does not move the page under itself.
    pub fn on_focus(&self, i: usize, by_pointer: bool) {
        self.last_focus.set(Some(i));
        if !by_pointer {
            self.reveal(i);
        }
    }

    pub fn spec(&self) -> GroupSpec {
        GroupSpec {
            id: self.group,
            kind: if self.cols > 1 { GroupKind::Grid { cols: self.cols, holes: &[] } } else { GroupKind::Column },
            seat: Seat::Remembered,
            reachable: AxisMask::BOTH,
            edge: [EdgeRule::Geometric; 4],
            extent: self.rect,
            len: self.len(),
            // Cards: a long OK press is a hold (the item's menu).
            elem: ElemKind::Card,
        }
    }

    /// Up/Down/Left/Right inside the flow. An item taller than the screen is paged through first.
    pub fn neighbour(&self, key: FocusKey<u32>, dir: Dir) -> Step<u32> {
        let Some(i) = self.index(key.elem) else { return Step::Edge };
        let n = self.len();
        let cols = self.cols.max(1);
        let view = self.rect.h;
        if let Some(r) = self.target_rect(i) {
            let (top, bot) = (self.rect.y, self.rect.y + view);
            match dir {
                Dir::Down if r.y + r.h > bot + 4.0 => {
                    self.scroll_by((view * 0.7).min(r.y + r.h - bot + self.margin.min(40.0)));
                    return Step::Move(key);
                }
                Dir::Up if r.y < top - 4.0 => {
                    self.scroll_by(-(view * 0.7).min(top - r.y + self.margin.min(40.0)));
                    return Step::Move(key);
                }
                _ => {}
            }
        }
        let to = match dir {
            Dir::Up if i >= cols => Some(i - cols),
            Dir::Down if i + cols < n => Some(i + cols),
            // The last, short row: Down from above it lands on its last item.
            Dir::Down if i / cols < (n - 1) / cols => Some(n - 1),
            Dir::Left if cols > 1 && i % cols > 0 => Some(i - 1),
            Dir::Right if cols > 1 && i % cols + 1 < cols && i + 1 < n => Some(i + 1),
            _ => None,
        };
        match to {
            Some(j) => {
                self.reveal(j);
                Step::Move(self.key(j))
            }
            None => Step::Edge,
        }
    }

    pub fn place(&self, e: u32) -> Option<Placed> {
        let i = self.index(e)?;
        let r = self.target_rect(i)?;
        Some(Placed { rect: r, rest_rect: r, clip: self.rect, index: Some(i as u32) })
    }

    /// Registers the visible items with the pointer.
    pub fn stops(&self, f: &mut plx_ui::screen::DrawFrame<'_, '_, RdHost>) {
        for i in self.visible() {
            if let Some(r) = self.screen_rect(i) {
                let clipped = intersect(r, self.rect);
                if clipped.h > 8.0 {
                    crate::ui::stop(f, self.key(i), clipped, self.rect);
                }
            }
        }
    }

    /// Faint arrows when there is more above / below.
    pub fn draw_more_marks(&self, p: plx_ui::Painter) {
        let x = self.rect.x + self.rect.w / 2.0;
        if self.scroll.get() > 4.0 {
            crate::ui::text(p, "\u{25B2}", x, self.rect.y - 30.0, 20, crate::ui::FAINT, crate::ui::ALIGN_CENTER, false);
        }
        if self.scroll.get() < self.max_scroll() - 4.0 {
            crate::ui::text(p, "\u{25BC}", x, self.rect.y + self.rect.h + 4.0, 20, crate::ui::FAINT, crate::ui::ALIGN_CENTER, false);
        }
    }
}

pub fn intersect(a: Rect, b: Rect) -> Rect {
    let x0 = a.x.max(b.x);
    let y0 = a.y.max(b.y);
    let x1 = (a.x + a.w).min(b.x + b.w);
    let y1 = (a.y + a.h).min(b.y + b.h);
    Rect::new(x0, y0, (x1 - x0).max(0.0), (y1 - y0).max(0.0))
}
