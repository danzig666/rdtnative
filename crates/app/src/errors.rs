//! Reddit errors in plain words for the screen: a missing subreddit or user says so by name, and
//! a network failure says to check the TV's network. The technical text (the request path,
//! reqwest's message) goes to the log, where the caller writes it.

use crate::i18n::tr;
use rd_core::RdError;

/// What the user reads for `e` (in the interface language).
pub fn plain(e: &RdError) -> String {
    match e {
        RdError::NotFound(path) => match missing(path) {
            Some(name) => tr("{} doesn't exist").replace("{}", &name),
            None => tr("Reddit couldn't find this page.").to_string(),
        },
        RdError::Network(_) => tr("No connection to Reddit. Check the TV's network.").to_string(),
        RdError::RateLimited { reset_secs } => tr("Reddit asked to wait {} s before more requests.").replace("{}", &reset_secs.to_string()),
        e => e.to_string(),
    }
}

/// The subreddit or user a 404 path names ("/r/pics/hot" -> "r/pics").
fn missing(path: &str) -> Option<String> {
    let mut parts = path.trim_start_matches('/').split(['/', '?']);
    let (kind, name) = (parts.next()?, parts.next().filter(|n| !n.is_empty())?);
    match kind {
        "r" => Some(format!("r/{name}")),
        "user" | "u" => Some(format!("u/{name}")),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn errors_read_plainly() {
        assert_eq!(missing("/r/pics/hot").as_deref(), Some("r/pics"));
        assert_eq!(missing("/user/someone/submitted?sort=new").as_deref(), Some("u/someone"));
        assert_eq!(missing("/comments/abc"), None);
        assert_eq!(missing("/r/"), None);
        assert!(plain(&RdError::NotFound("/r/nosuchsub/hot".into())).contains("r/nosuchsub"));
        let net = plain(&RdError::Network("error sending request for url (https://oauth.reddit.com/hot)".into()));
        assert!(!net.contains("oauth.reddit.com"), "{net}");
        assert!(plain(&RdError::RateLimited { reset_secs: 42 }).contains("42"));
        assert_eq!(plain(&RdError::InvalidGrant), RdError::InvalidGrant.to_string());
    }
}
