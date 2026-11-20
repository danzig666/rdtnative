//! The inbox outside its page: reading Reddit's message listings, telling when the unread count
//! grew, and the note about it: the app's own toast, or the TV's notification while the app is in
//! the background.

use crate::store::Message;

/// How often the unread count is asked while signed in.
pub const POLL_MS: u32 = 120_000;

/// The messages of a `/message/...` listing, newest first.
pub fn parse_messages(json: &serde_json::Value) -> Vec<Message> {
    let Some(items) = json["data"]["children"].as_array() else { return Vec::new() };
    items
        .iter()
        .map(|t| {
            let d = &t["data"];
            let g = |k: &str| d[k].as_str().unwrap_or_default().to_string();
            Message {
                name: g("name"),
                author: g("author"),
                subject: g("subject"),
                body: g("body"),
                created: d["created_utc"].as_f64().unwrap_or(0.0) as i64,
                unread: d["new"].as_bool().unwrap_or(false),
                context: d["context"].as_str().filter(|s| !s.is_empty()).map(str::to_string),
                subreddit: d["subreddit"].as_str().map(str::to_string),
            }
        })
        .collect()
}

/// Who a message is from: "u/name", or "r/sub" for a subreddit's (moderators') message.
pub fn sender(m: &Message) -> Option<String> {
    if !m.author.is_empty() {
        Some(format!("u/{}", m.author))
    } else {
        m.subreddit.as_ref().filter(|s| !s.is_empty()).map(|s| format!("r/{s}"))
    }
}

/// How many messages arrived since the count was `before` (`None` when nothing was known yet:
/// the first count after starting or switching accounts only sets the mark).
pub fn grew(before: Option<i64>, now: i64) -> Option<i64> {
    before.filter(|b| now > *b).map(|b| now - b)
}

/// The note for `more` new messages; `from` is the sender when there is one message.
pub fn alert(more: i64, from: Option<&str>) -> String {
    use crate::i18n::tr;
    match (more, from) {
        (1, Some(f)) => tr("New message from {}").replace("{}", f),
        (1, None) => tr("You have a new message").to_string(),
        (n, _) => tr("{} new messages").replace("{}", &n.to_string()),
    }
}

/// The TV's own notification toast, for news while the app is in the background. It goes through
/// the bus connection that may call any service, whose name ends in "rdtnative<pid>": the
/// notification service takes a source ID that is part of the caller's name. Choosing the toast
/// brings the app back.
#[cfg(not(any(windows, feature = "sim")))]
pub fn system_toast(msg: &str) {
    let msg: String = msg.chars().take(60).collect();
    let spawned = plx_base::task::spawn("notify", move || {
        let luna = match crate::ls2::Luna::wide(plx_base::paths::app_id()) {
            Ok(l) => l,
            Err(e) => {
                crate::log(&format!("notify: no bus: {e}"));
                return;
            }
        };
        let payload = serde_json::json!({
            "sourceId": "rdtnative",
            "message": msg,
            "iconUrl": plx_base::paths::app_dir().join("icon.png").display().to_string(),
            "onclick": { "appId": plx_base::paths::app_id() },
        })
        .to_string();
        let r = luna.call("luna://com.webos.notification/createToast", &payload);
        crate::log(&format!("notify: createToast -> {r}"));
    });
    if spawned.is_none() {
        crate::log("notify: could not start worker thread");
    }
}

/// The simulator has no TV notifications: the note goes to the log.
#[cfg(any(windows, feature = "sim"))]
pub fn system_toast(msg: &str) {
    crate::log(&format!("notify (simulator): {msg}"));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_a_listing_and_its_senders() {
        let v = serde_json::json!({ "data": { "children": [
            { "kind": "t4", "data": { "name": "t4_a", "author": "alice", "subject": "hello", "body": "hi there", "created_utc": 1700000000.0, "new": true, "context": "", "subreddit": null } },
            { "kind": "t1", "data": { "name": "t1_b", "author": "bob", "subject": "comment reply", "body": "ok", "created_utc": 1700000100.0, "new": false, "context": "/r/rust/comments/x/y/z/?context=3", "subreddit": "rust" } },
            { "kind": "t4", "data": { "name": "t4_c", "author": null, "subject": "a note from the moderators", "body": "...", "subreddit": "pics" } }
        ] } });
        let m = parse_messages(&v);
        assert_eq!(m.len(), 3);
        assert_eq!((m[0].name.as_str(), m[0].unread, m[0].context.as_deref()), ("t4_a", true, None));
        assert_eq!((m[1].unread, m[1].context.as_deref()), (false, Some("/r/rust/comments/x/y/z/?context=3")));
        assert_eq!(sender(&m[0]).as_deref(), Some("u/alice"));
        assert_eq!(sender(&m[2]).as_deref(), Some("r/pics"));
        assert!(parse_messages(&serde_json::json!({})).is_empty());
    }

    #[test]
    fn only_a_growing_count_is_news() {
        assert_eq!(grew(None, 3), None, "the first count only sets the mark");
        assert_eq!(grew(Some(3), 3), None);
        assert_eq!(grew(Some(3), 1), None, "messages were read");
        assert_eq!(grew(Some(1), 3), Some(2));
        // (Another test switches the language for a moment: only the filled-in parts are checked.)
        assert!(alert(1, Some("u/alice")).contains("u/alice"));
        assert!(!alert(1, None).contains("{}"));
        let many = alert(4, Some("u/alice"));
        assert!(many.contains('4') && !many.contains("alice"));
    }
}
