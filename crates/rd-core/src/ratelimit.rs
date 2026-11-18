//! Reddit's rate-limit headers (`x-ratelimit-used`, `-remaining`, `-reset`) and the budget that
//! decides what may still be sent: user actions always, page loads while some budget is left,
//! prefetch and images only while plenty is left (PLAN.md §2).

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RateLimit {
    pub used: u32,
    pub remaining: f32,
    /// Seconds until the window resets.
    pub reset_secs: u64,
    /// When this snapshot was taken (Unix seconds).
    pub at: u64,
}

impl RateLimit {
    /// Parses the three headers; `None` if any is missing (e.g. on the token endpoint).
    pub fn from_headers(get: impl Fn(&str) -> Option<String>, at: u64) -> Option<RateLimit> {
        let used = get("x-ratelimit-used")?.trim().parse::<f32>().ok()? as u32;
        let remaining = get("x-ratelimit-remaining")?.trim().parse::<f32>().ok()?;
        let reset_secs = get("x-ratelimit-reset")?.trim().parse::<f32>().ok()? as u64;
        Some(RateLimit { used, remaining, reset_secs, at })
    }

    /// Requests still allowed at `now`; the full window once the reset has passed.
    pub fn remaining_at(&self, now: u64) -> f32 {
        if now >= self.at + self.reset_secs {
            f32::INFINITY
        } else {
            self.remaining
        }
    }

    pub fn resets_in(&self, now: u64) -> u64 {
        (self.at + self.reset_secs).saturating_sub(now)
    }
}

/// How important a request is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Priority {
    /// Next-page prefetch, thumbnails of off-screen posts.
    Prefetch,
    /// Something the user is waiting to see.
    Page,
    /// Vote, save, reply, sign-in: never held back by the budget.
    Action,
}

/// Reserve kept for higher-priority work.
pub const PAGE_RESERVE: f32 = 5.0;
pub const PREFETCH_RESERVE: f32 = 20.0;

/// `None` = send now; `Some(secs)` = wait that long first.
pub fn should_wait(last: Option<&RateLimit>, prio: Priority, now: u64) -> Option<u64> {
    let rl = last?;
    let left = rl.remaining_at(now);
    let reserve = match prio {
        Priority::Action => 0.0,
        Priority::Page => PAGE_RESERVE,
        Priority::Prefetch => PREFETCH_RESERVE,
    };
    if left > reserve || (prio == Priority::Action && left >= 1.0) {
        None
    } else {
        Some(rl.resets_in(now).max(1))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rl(remaining: f32, reset: u64) -> RateLimit {
        RateLimit { used: 0, remaining, reset_secs: reset, at: 1000 }
    }

    #[test]
    fn parses_reddits_headers_including_fractions() {
        let h = |k: &str| match k {
            "x-ratelimit-used" => Some("12".into()),
            "x-ratelimit-remaining" => Some("88.0".into()),
            "x-ratelimit-reset" => Some("341".into()),
            _ => None,
        };
        let r = RateLimit::from_headers(h, 50).unwrap();
        assert_eq!((r.used, r.remaining, r.reset_secs, r.at), (12, 88.0, 341, 50));
        assert!(RateLimit::from_headers(|_| None, 0).is_none());
    }

    #[test]
    fn budget_holds_back_low_priority_first() {
        let low = rl(10.0, 120);
        assert_eq!(should_wait(Some(&low), Priority::Prefetch, 1000), Some(120));
        assert_eq!(should_wait(Some(&low), Priority::Page, 1000), None);
        assert_eq!(should_wait(Some(&low), Priority::Action, 1000), None);

        let empty = rl(0.0, 30);
        assert_eq!(should_wait(Some(&empty), Priority::Action, 1010), Some(20));
        // After the reset everything flows again.
        assert_eq!(should_wait(Some(&empty), Priority::Prefetch, 1031), None);
        assert_eq!(should_wait(None, Priority::Prefetch, 0), None);
    }
}
