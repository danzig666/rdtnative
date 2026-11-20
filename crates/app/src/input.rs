#![allow(dead_code)]
//! SDL key events → the dispatcher's input events. Adapted from plx-native's
//! `app/events.rs::decode_key` and `app/bridge.rs::key_input` (GPL-3.0-or-later).

use plx_machine::machine::{Edge, InputEvent, InputKind, Key, Source, Tick};

pub fn rd_u32(ev: &[u8], at: usize) -> u32 {
    u32::from_ne_bytes(ev[at..at + 4].try_into().unwrap())
}

/// LG's SDL fork shifts `SDL_KeyboardEvent` by four bytes against upstream headers:
/// +16 state (low byte pressed, bit 0x100 auto-repeat), +20 wcode (LG remote code), +24 sym.
#[cfg(not(any(windows, feature = "sim")))]
pub fn decode_key(ev: &[u8]) -> (u32, u32, u32) {
    (rd_u32(ev, 16), rd_u32(ev, 20), rd_u32(ev, 24))
}

/// Windows simulator: stock SDL2 layout (state u8@12, repeat u8@13, keysym.sym @20). The PC
/// keyboard stands in for the remote: arrows, Enter = OK, Escape = BACK (Backspace stays
/// Backspace: the on-screen keyboard deletes with it).
#[cfg(any(windows, feature = "sim"))]
pub fn decode_key(ev: &[u8]) -> (u32, u32, u32) {
    let state = ev[12] as u32 | if ev[13] != 0 { 0x100 } else { 0 };
    let sym = rd_u32(ev, 20);
    const SDLK_ESCAPE: u32 = 27;
    const SDLK_KP_ENTER: u32 = 0x4000_0058;
    match sym {
        SDLK_ESCAPE => (state, 482, 0),
        SDLK_KP_ENTER => (state, 0, 13),
        _ => (state, 0, sym),
    }
}

/// Shift+Enter on a USB keyboard: a new line in a reply (plain Enter is the remote's OK).
pub const SHIFT_ENTER: u32 = 10;

const SDLK_CAPSLOCK: u32 = 57 | (1 << 30);
const SDLK_LSHIFT: u32 = 225 | (1 << 30);
const SDLK_RSHIFT: u32 = 229 | (1 << 30);

/// A USB keyboard's Shift and Caps Lock, followed from the key events themselves in order (a
/// quick Shift+letter is never read late, and no field of LG's event struct is needed), so a
/// printable key comes out as the character typed: `a` with Shift is `A`, `1` is `!` (US layout).
#[derive(Default)]
pub struct Mods {
    left: bool,
    right: bool,
    caps: bool,
}

impl Mods {
    /// The key `sym` as typed, `state` as [`decode_key`] gives it.
    pub fn typed(&mut self, sym: u32, state: u32) -> u32 {
        let down = state & 0xff == 1;
        match sym {
            SDLK_LSHIFT => self.left = down,
            SDLK_RSHIFT => self.right = down,
            SDLK_CAPSLOCK if down && state & 0x100 == 0 => self.caps = !self.caps,
            _ => {}
        }
        shifted(sym, self.left || self.right, self.caps)
    }
}

fn shifted(sym: u32, shift: bool, caps: bool) -> u32 {
    let Some(c) = char::from_u32(sym).filter(|c| c.is_ascii()) else { return sym };
    if c.is_ascii_lowercase() {
        return if shift != caps { c.to_ascii_uppercase() as u32 } else { sym };
    }
    if !shift {
        return sym;
    }
    if c == '\r' {
        return SHIFT_ENTER;
    }
    const PLAIN: &str = "1234567890-=[]\\;',./`";
    const SHIFTED: &str = "!@#$%^&*()_+{}|:\"<>?~";
    PLAIN.find(c).map_or(sym, |i| SHIFTED.as_bytes()[i] as u32)
}

pub fn rd_i32(ev: &[u8], at: usize) -> i32 {
    i32::from_ne_bytes(ev[at..at + 4].try_into().unwrap())
}

/// Pointer position of an SDL mouse motion / button event (x@20, y@24 on LG's SDL and upstream),
/// in the UI's logical 1920x1080 space.
pub fn pointer_xy(ev: &[u8]) -> (f32, f32) {
    plx_base::surface::to_logical(rd_i32(ev, 20) as f32, rd_i32(ev, 24) as f32)
}

fn ev(kind: InputKind<u32>, now: Tick) -> InputEvent<u32> {
    InputEvent { at: now, source: Source::Sdl, kind }
}

/// Magic Remote pointer moved: hover moves focus to what is under it (the dispatcher's hit map).
pub fn pointer(x: f32, y: f32, now: Tick) -> InputEvent<u32> {
    ev(InputKind::Pointer { x, y, hit: None }, now)
}

/// Button down: presses what is under the pointer.
pub fn click(x: f32, y: f32, now: Tick) -> InputEvent<u32> {
    ev(InputKind::Click { x, y, hit: None }, now)
}

/// Motion with the button held (the player's scrub bar).
pub fn drag(x: f32, y: f32, now: Tick) -> InputEvent<u32> {
    ev(InputKind::Drag { x, y, hit: None }, now)
}

/// Button up: the dispatcher's press machine releases on an OK key-up (as in plx-native's bridge).
pub fn release(now: Tick) -> InputEvent<u32> {
    ev(InputKind::Key { key: Key::Ok, sym: 0, wcode: 0, edge: Edge::Up, at_edge: false }, now)
}

/// A wheel notch as the dispatcher's wheel (`dy` > 0 scrolls up, as plx's Stack expects).
pub fn wheel_event(dy: f32, now: Tick) -> InputEvent<u32> {
    ev(InputKind::Wheel { dy }, now)
}

/// A wheel notch (the Magic Remote's scroll wheel) as Up / Down, both edges.
pub fn wheel(dy: i32, now: Tick) -> Vec<InputEvent<u32>> {
    let key = if dy < 0 { Key::Down } else { Key::Up };
    [Edge::Down, Edge::Up].into_iter().map(|edge| ev(InputKind::Key { key, sym: 0, wcode: 0, edge, at_edge: false }, now)).collect()
}

pub fn key_input(sym: u32, wcode: u32, state: u32, now: Tick) -> InputEvent<u32> {
    use plx_ui::consts::{classify, Key as K};
    if matches!(classify(sym, wcode), K::PointerHidden) {
        // The Magic Remote's pointer timed out: back to D-pad focus.
        return ev(InputKind::PointerHidden, now);
    }
    let key = match classify(sym, wcode) {
        K::Up => Key::Up,
        K::Down => Key::Down,
        K::Left { alt: false } => Key::Left,
        K::Right { alt: false } => Key::Right,
        K::Ok => Key::Ok,
        K::Back => Key::Back,
        _ => Key::Other,
    };
    let edge = if (state & 0xff) != 1 {
        Edge::Up
    } else if state & 0x100 != 0 {
        Edge::Repeat
    } else {
        Edge::Down
    };
    InputEvent { at: now, source: Source::Sdl, kind: InputKind::Key { key, sym, wcode, edge, at_edge: false } }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DOWN: u32 = 1;
    const REPEAT: u32 = 0x101;
    const UP: u32 = 0;

    #[test]
    fn shift_and_caps_lock_follow_the_key_events() {
        let mut m = Mods::default();
        let a = 'a' as u32;
        assert_eq!(m.typed(a, DOWN), a);
        m.typed(SDLK_LSHIFT, DOWN);
        assert_eq!(m.typed(a, DOWN), 'A' as u32);
        assert_eq!(m.typed('1' as u32, DOWN), '!' as u32);
        assert_eq!(m.typed('/' as u32, DOWN), '?' as u32);
        assert_eq!(m.typed(13, DOWN), SHIFT_ENTER);
        // The other Shift released does not end this one.
        m.typed(SDLK_RSHIFT, UP);
        assert_eq!(m.typed('q' as u32, DOWN), 'Q' as u32);
        m.typed(SDLK_LSHIFT, UP);
        assert_eq!(m.typed('q' as u32, DOWN), 'q' as u32);
        assert_eq!(m.typed(13, DOWN), 13, "plain Enter stays the remote's OK");
        // Caps Lock flips letters only, once per press (not on auto-repeat).
        m.typed(SDLK_CAPSLOCK, DOWN);
        m.typed(SDLK_CAPSLOCK, REPEAT);
        m.typed(SDLK_CAPSLOCK, UP);
        assert_eq!(m.typed(a, DOWN), 'A' as u32);
        assert_eq!(m.typed('1' as u32, DOWN), '1' as u32);
        m.typed(SDLK_RSHIFT, DOWN);
        assert_eq!(m.typed(a, DOWN), a);
        // Keys that are no characters pass through.
        assert_eq!(m.typed(82 | (1 << 30), DOWN), 82 | (1 << 30));
        assert_eq!(m.typed(8, DOWN), 8);
    }

    #[test]
    fn a_typed_capital_q_is_no_back_key() {
        let k = |sym| match key_input(sym, 0, DOWN, Tick { ms: 0, dt_us: 0 }).kind {
            InputKind::Key { key, .. } => key,
            _ => unreachable!(),
        };
        assert_eq!(k('Q' as u32), Key::Other);
        assert_eq!(k(8), Key::Other, "Backspace reaches the keyboard");
    }
}
