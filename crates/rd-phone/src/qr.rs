//! QR code of the form's URL: a module matrix the TV draws with GL (like RustCouch's `draw_qr`),
//! and a text rendering for terminals (the spike).

/// Modules per side and dark flags, row by row (no quiet zone; draw 4 light modules around it).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Qr {
    pub width: usize,
    pub dark: Vec<bool>,
}

pub const QUIET_ZONE: usize = 4;

impl Qr {
    pub fn new(text: &str) -> Option<Qr> {
        let code = qrcode::QrCode::with_error_correction_level(text.as_bytes(), qrcode::EcLevel::L).ok()?;
        let width = code.width();
        let dark = code.to_colors().into_iter().map(|c| c == qrcode::Color::Dark).collect();
        Some(Qr { width, dark })
    }

    pub fn is_dark(&self, x: usize, y: usize) -> bool {
        x < self.width && y < self.width && self.dark[y * self.width + x]
    }

    /// Largest whole-pixel module size so the code plus quiet zone fits in `side` pixels, and the
    /// resulting total size. Whole pixels keep edges sharp for phone cameras.
    pub fn fit(&self, side: u32) -> (u32, u32) {
        let modules = (self.width + 2 * QUIET_ZONE) as u32;
        let m = (side / modules).max(1);
        (m, m * modules)
    }

    /// Two module rows per text line with half blocks; dark modules print as the terminal's
    /// background, so it scans on a dark terminal (light quiet zone included).
    pub fn to_terminal(&self) -> String {
        let n = self.width + 2 * QUIET_ZONE;
        let light = |x: usize, y: usize| -> bool {
            let (x, y) = (x as isize - QUIET_ZONE as isize, y as isize - QUIET_ZONE as isize);
            !(x >= 0 && y >= 0 && self.is_dark(x as usize, y as usize))
        };
        let mut out = String::new();
        for y in (0..n).step_by(2) {
            for x in 0..n {
                let top = light(x, y);
                let bottom = y + 1 < n && light(x, y + 1);
                out.push(match (top, bottom) {
                    (true, true) => '█',
                    (true, false) => '▀',
                    (false, true) => '▄',
                    (false, false) => ' ',
                });
            }
            out.push('\n');
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_a_lan_url() {
        let q = Qr::new("http://192.168.1.20:43567/0123456789abcdef0123456789abcdef").unwrap();
        assert!(q.width >= 21 && q.width % 4 == 1);
        assert_eq!(q.dark.len(), q.width * q.width);
        // Finder pattern corner is dark.
        assert!(q.is_dark(0, 0) && q.is_dark(6, 6) && !q.is_dark(q.width, 0));
        let (m, total) = q.fit(400);
        assert_eq!(total, m * (q.width as u32 + 8));
        assert!(total <= 400);
        let t = q.to_terminal();
        assert_eq!(t.lines().count(), (q.width + 8).div_ceil(2));
        assert!(t.lines().all(|l| l.chars().count() == q.width + 8));
    }
}
