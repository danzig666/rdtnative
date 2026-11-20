//! The Reddit accounts signed in on this TV. `reddit.json` stays what it was: the API settings and
//! the identity in use. `accounts.json` lists every signed-in account, the one in use included,
//! so a switch never loses one; a TV that only has `reddit.json` gets the list made from it on
//! first load. Refresh tokens belong to the client ID they were issued to, so all accounts share
//! the API settings of the identity in use and a new client ID ends every one of them.

use crate::store::Store;
use rd_core::session::Saved;

const FILE: &str = "accounts.json";

fn is(a: &Saved, name: &str) -> bool {
    a.username.as_deref().is_some_and(|n| n.eq_ignore_ascii_case(name))
}

/// The account list on disk, kept in step with the identity in use (`reddit.json`).
pub fn load(active: Option<&Saved>) -> Vec<Saved> {
    let read: Option<Vec<Saved>> = std::fs::read_to_string(crate::store::data_file(FILE)).ok().and_then(|s| serde_json::from_str(&s).ok());
    let first = read.is_none();
    let mut list = read.unwrap_or_default();
    reconcile(&mut list, active);
    if first && !list.is_empty() {
        crate::log("accounts: made the account list from reddit.json");
        store(&list);
    }
    list
}

pub fn store(list: &[Saved]) {
    let written = serde_json::to_vec_pretty(list).map_err(|e| e.to_string()).and_then(|b| crate::store::write_private(&crate::store::data_file(FILE), &b).map_err(|e| e.to_string()));
    if let Err(e) = written {
        crate::log(&format!("accounts: save failed: {e}"));
    }
}

/// Drops entries that cannot sign in with the identity in use (another client ID, no token) and
/// puts the identity's own account in, with its newest tokens. Without API settings no account
/// works, so the list is empty.
pub fn reconcile(list: &mut Vec<Saved>, active: Option<&Saved>) {
    let Some(a) = active else {
        list.clear();
        return;
    };
    list.retain(|s| s.client_id == a.client_id && s.refresh_token.is_some() && s.username.is_some());
    if a.refresh_token.is_some() {
        upsert(list, a.clone());
    }
}

/// Adds `s`, or replaces the entry of the same user (names ignore case).
pub fn upsert(list: &mut Vec<Saved>, s: Saved) {
    let Some(name) = s.username.clone() else { return };
    match list.iter_mut().find(|a| is(a, &name)) {
        Some(a) => *a = s,
        None => list.push(s),
    }
}

pub fn remove(list: &mut Vec<Saved>, name: &str) -> Option<Saved> {
    let i = list.iter().position(|a| is(a, name))?;
    Some(list.remove(i))
}

/// The identity to use for `name`: its tokens with the API settings and device ID in use. An empty
/// name browses without an account; an unknown one is `None`.
pub fn switch_to(list: &[Saved], active: &Saved, name: &str) -> Option<Saved> {
    if name.is_empty() {
        return Some(signed_out(active));
    }
    let a = list.iter().find(|a| is(a, name))?;
    Some(Saved { refresh_token: a.refresh_token.clone(), username: a.username.clone(), access_token: a.access_token.clone(), expires_at: a.expires_at, ..active.clone() })
}

/// `s` without its account: only the API settings and the device ID are left.
pub fn signed_out(s: &Saved) -> Saved {
    Saved { refresh_token: None, username: None, access_token: None, expires_at: 0, ..s.clone() }
}

/// The account names, in the order they were added.
pub fn names(store: &Store) -> Vec<String> {
    store.accounts.iter().filter_map(|a| a.username.clone()).collect()
}

/// Whether `name` is the account in use.
pub fn in_use(store: &Store, name: &str) -> bool {
    store.username().is_some_and(|u| u.eq_ignore_ascii_case(name))
}

/// Saving the API settings being edited would sign every account out: the client ID changes while
/// some account is on this TV.
pub fn new_client_signs_out(store: &Store) -> bool {
    let changes = match (store.saved.as_ref(), store.draft.validate()) {
        (Some(s), Ok(cfg)) => s.client_id != cfg.client_id,
        _ => false,
    };
    changes && (store.signed_in() || !store.accounts.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn account(name: &str, rt: &str) -> Saved {
        Saved { client_id: "abcdefghijklmn".into(), device_id: "d".repeat(30), refresh_token: Some(rt.into()), username: Some(name.into()), ..Saved::default() }
    }

    #[test]
    fn the_list_follows_the_identity_in_use() {
        // First load of a single-account TV: the list is made from reddit.json.
        let alice = account("alice", "RT1");
        let mut list = Vec::new();
        reconcile(&mut list, Some(&alice));
        assert_eq!(list, [alice.clone()]);
        // The identity in use has the newest tokens; another client ID's accounts cannot work.
        let newer = Saved { access_token: Some("AT".into()), ..alice.clone() };
        let other_app = Saved { client_id: "zyxwvutsrqponm".into(), ..account("bob", "RT2") };
        list.push(other_app);
        reconcile(&mut list, Some(&newer));
        assert_eq!(list, [newer.clone()]);
        // Browsing without an account keeps the accounts; without API settings none works.
        reconcile(&mut list, Some(&signed_out(&newer)));
        assert_eq!(list.len(), 1);
        reconcile(&mut list, None);
        assert!(list.is_empty());
    }

    #[test]
    fn add_switch_and_remove() {
        let mut list = vec![account("alice", "RT1")];
        upsert(&mut list, account("Bob", "RT2"));
        upsert(&mut list, account("ALICE", "RT3"));
        assert_eq!(list.len(), 2, "the same user is replaced, whatever the case");
        assert_eq!(list[0].refresh_token.as_deref(), Some("RT3"));

        // Switching keeps the API settings in use, takes the account's tokens.
        let active = Saved { user_agent: Some("webos:x.y:v2 (by /u/me)".into()), device_id: "e".repeat(30), ..signed_out(&list[0]) };
        let bob = switch_to(&list, &active, "bob").unwrap();
        assert_eq!((bob.username.as_deref(), bob.refresh_token.as_deref()), (Some("Bob"), Some("RT2")));
        assert_eq!((bob.user_agent.as_deref(), bob.device_id.as_str()), (active.user_agent.as_deref(), active.device_id.as_str()));
        assert!(switch_to(&list, &active, "carol").is_none());
        let anon = switch_to(&list, &bob, "").unwrap();
        assert_eq!((anon.refresh_token, anon.username, anon.client_id), (None, None, bob.client_id.clone()));

        assert_eq!(remove(&mut list, "BOB").and_then(|s| s.refresh_token).as_deref(), Some("RT2"));
        assert!(remove(&mut list, "bob").is_none());
        assert_eq!(list.len(), 1);
    }
}
