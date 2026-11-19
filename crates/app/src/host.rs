//! The app's `machine::Host`: screen arguments, effects, messages and views, plus the `Rig` the
//! dispatcher drives (mounting screens, running effects on worker threads, per-frame preparation).
//! Same shape as ytnative's `host.rs`, with Reddit's state.

use crate::screens;
use crate::store::{self, Field, PhoneView, PostRow, Store};
use plx_machine::machine::{
    Addr, Canon, Chrome, Cx, Effects, EntryId, Handled, Host, InputOwner, InstanceId, LogicalState, MachineId, RequestId, ScreenId,
    TimerId,
};
use plx_machine::present::Present;
use plx_ui::dispatch::{CxParts, Rig, Split};
use plx_ui::frame::Budget;
use plx_ui::screen::{Mounter, ReturnState, Screen, ScreenArg};
use rd_core::config::{self, ApiConfig};
use rd_core::ratelimit::Priority;
use rd_core::session::{Saved, Session};
use std::sync::{Arc, Mutex};

pub struct RdHost;

/// What a navigation entry shows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RdArg {
    Home,
    /// Settings › Reddit API: client ID, redirect URI, user agent; typed here or on a phone.
    ApiSettings,
    /// The on-screen keyboard editing one API field.
    Edit(Field),
}

impl ScreenArg for RdArg {
    fn chrome(&self) -> Chrome {
        Chrome::None
    }
    fn id(&self) -> ScreenId {
        ScreenId(match self {
            RdArg::Home => 1,
            RdArg::ApiSettings => 2,
            RdArg::Edit(_) => 3,
        })
    }
    fn title(&self) -> Option<&str> {
        None
    }
    fn same_instance(&self, other: &Self) -> bool {
        self == other
    }
}

impl LogicalState for RdArg {
    fn write(&self, c: &mut Canon) {
        c.u32(self.id().0);
        c.str(&format!("{self:?}"));
    }
    fn probe(&self, out: &mut String) {
        out.push_str(&format!("{self:?}"));
    }
}

pub enum RdFx {
    Toast(String),
    /// The keyboard changed a field of the draft.
    SetField(Field, String),
    /// Validate the draft; when it is valid, save it and connect.
    SaveApi,
    /// Open / close the one-time phone form for the API settings.
    PhoneStart,
    PhoneStop,
    /// (Re)load the front page.
    LoadFront,
}

pub enum RdMsg {
    /// The store changed (a page arrived, the phone sent the settings).
    Changed,
}

#[derive(Clone, Copy)]
pub struct Views<'a> {
    pub store: &'a Store,
    pub now_ms: u32,
}

#[derive(Default)]
pub struct Init;
impl LogicalState for Init {
    fn write(&self, _: &mut Canon) {}
    fn probe(&self, _: &mut String) {}
}

impl Host for RdHost {
    type Arg = RdArg;
    type Fx = RdFx;
    type Msg = RdMsg;
    type Elem = u32;
    type Views<'a> = Views<'a>;
    type Init = Init;
    type Memory = ();
}

struct Mount;

impl Mounter<RdHost> for Mount {
    fn mount(
        &mut self,
        _id: InstanceId,
        arg: &RdArg,
        _ret: &ReturnState<u32, ()>,
        cx: &Cx<'_, RdHost>,
        _fx: &mut Effects<'_, RdHost>,
    ) -> Box<dyn Screen<RdHost>> {
        let entry = match cx.owner {
            InputOwner::Entry(e) => e,
            _ => EntryId(0),
        };
        match arg {
            RdArg::Home => Box::new(screens::home::HomeScreen::new(entry)),
            RdArg::ApiSettings => Box::new(screens::api::ApiScreen::new(entry)),
            RdArg::Edit(f) => Box::new(screens::keyboard::KeyboardScreen::new(entry, *f, cx.views.store.draft.get(*f))),
        }
    }
}

/// Results workers post back to the main thread.
enum Work {
    Front { epoch: u64, result: Result<(String, Vec<PostRow>), String>, saved: Option<Saved> },
}

pub struct AppRig {
    mounter: Mount,
    pub store: Store,
    pub now_ms: u32,
    measure: plx_gfx::text::TtfMeasure,
    next_req: u32,
    done: Arc<Mutex<Vec<(Addr, Work)>>>,
    /// Bumped when the API settings change: results of an older session are dropped.
    epoch: u64,
    phone: Option<rd_phone::PhoneForm>,
    /// The machine that opened the phone form (gets `Changed` when it delivers).
    phone_from: Option<MachineId>,
    /// Notifications made on the main thread, delivered with the next frame.
    pending: Vec<(Addr, RdMsg)>,
}

impl AppRig {
    pub fn new() -> Self {
        AppRig {
            mounter: Mount,
            store: Store::new(),
            now_ms: 0,
            measure: plx_gfx::text::TtfMeasure,
            next_req: 0,
            done: Default::default(),
            epoch: 0,
            phone: None,
            phone_from: None,
            pending: Vec::new(),
        }
    }

    fn next_addr(&mut self, to: MachineId) -> Addr {
        self.next_req += 1;
        Addr { to, req: RequestId(self.next_req) }
    }

    fn spawn(&mut self, from: MachineId, name: &'static str, job: impl FnOnce() -> Work + Send + 'static) {
        let addr = self.next_addr(from);
        let done = self.done.clone();
        let spawned = plx_base::task::spawn(name, move || {
            let r = job();
            done.lock().unwrap().push((addr, r));
            plx_machine::present::wake_from_worker();
        });
        if spawned.is_none() {
            crate::log(&format!("{name}: could not start worker thread"));
        }
    }

    /// Main thread, once per frame: poll the phone form, move finished worker results into the
    /// store, and return the notifications for the screens that asked.
    pub fn drain(&mut self, now_ms: u32) -> Vec<(Addr, RdMsg)> {
        self.now_ms = now_ms;
        if self.store.toast.as_ref().is_some_and(|(_, until)| now_ms.wrapping_sub(*until) < u32::MAX / 2) {
            self.store.toast = None;
            plx_machine::present::wake_from_worker();
        }
        self.poll_phone();
        let mut out = std::mem::take(&mut self.pending);
        let done = std::mem::take(&mut *self.done.lock().unwrap());
        for (addr, w) in done {
            match w {
                Work::Front { epoch, result, saved } => {
                    if epoch != self.epoch {
                        continue;
                    }
                    // The session refreshed its token: keep it so the next start does not mint
                    // one again.
                    if let Some(s) = saved {
                        let _ = store::store_saved(&s);
                        self.store.saved = Some(s);
                    }
                    let f = &mut self.store.front;
                    f.loading = false;
                    match result {
                        Ok((title, posts)) => {
                            f.title = title;
                            f.posts = posts;
                            f.error = None;
                        }
                        Err(e) => f.error = Some(e),
                    }
                }
            }
            out.push((addr, RdMsg::Changed));
        }
        out
    }

    /// The phone form: is it still open, was it opened, did it deliver?
    fn poll_phone(&mut self) {
        let Some(p) = &self.phone else { return };
        if let Some(values) = p.take() {
            let get = |k: &str| values.get(k).cloned().unwrap_or_default();
            for f in Field::ALL {
                self.store.draft.set(f, get(f.key()));
            }
            self.close_phone();
            // The page validated with the same rules, so this saves.
            self.save_api(true);
            self.notify_phone_owner();
            return;
        }
        let opened = p.opened();
        if !p.running() {
            self.close_phone();
            self.store.toast("The phone link expired", self.now_ms);
            self.notify_phone_owner();
        } else if let Some(v) = &mut self.store.phone {
            if v.opened != opened {
                v.opened = opened;
                self.notify_phone_owner();
            }
        }
    }

    fn notify_phone_owner(&mut self) {
        if let Some(m) = self.phone_from {
            let a = self.next_addr(m);
            self.pending.push((a, RdMsg::Changed));
        }
    }

    fn close_phone(&mut self) {
        self.phone = None;
        self.store.phone = None;
    }

    fn open_phone(&mut self) {
        let mut form = rd_phone::Form::new("rdtnative: Reddit API settings");
        form.intro = config::SETUP_STEPS.iter().map(|s| s.to_string()).collect();
        form.links.push(("Open reddit.com/prefs/apps".into(), config::REDDIT_APPS_URL.into()));
        for f in Field::ALL {
            let s = f.spec(&self.store.draft);
            form.fields.push(
                rd_phone::Field::new(s.key, s.label)
                    .value(&s.value)
                    .placeholder(&s.placeholder)
                    .help(s.help)
                    .kind(if s.url { rd_phone::Kind::Url } else { rd_phone::Kind::Text })
                    .max_len(s.max_len),
            );
        }
        let validate = |v: &rd_phone::Values| {
            config::from_values(|k| v.get(k).cloned().unwrap_or_default())
                .map(|_| ())
                .map_err(|e| e.into_iter().map(|e| (e.field.to_string(), e.message)).collect())
        };
        match rd_phone::PhoneForm::start(form, Box::new(validate)) {
            Ok(p) => {
                crate::log("phone form: open");
                self.store.phone = Some(PhoneView { url: p.url.clone(), opened: false });
                self.phone = Some(p);
            }
            Err(e) => self.store.toast(format!("Couldn't open the phone link: {e}"), self.now_ms),
        }
    }

    /// Validates the draft; when valid, stores it (a new client ID starts a new identity, other
    /// changes keep the tokens) and reloads the front page.
    fn save_api(&mut self, from_phone: bool) {
        match self.store.draft.validate() {
            Err(errs) => {
                self.store.errors = errs;
                self.store.toast("Check the highlighted settings", self.now_ms);
            }
            Ok(cfg) => {
                self.store.errors.clear();
                let saved = merge(self.store.saved.as_ref(), &cfg);
                if let Err(e) = store::store_saved(&saved) {
                    crate::log(&format!("api: save failed: {e}"));
                    self.store.toast(format!("Couldn't save: {e}"), self.now_ms);
                    return;
                }
                self.store.draft = store::Draft::from_saved(Some(&saved));
                self.store.saved = Some(saved);
                self.store.toast(if from_phone { "Received from your phone and saved" } else { "Saved" }, self.now_ms);
                self.epoch += 1;
                self.store.front = Default::default();
                self.load_front(MachineId::Nav);
            }
        }
    }

    fn load_front(&mut self, from: MachineId) {
        let Some(saved) = self.store.saved.clone() else { return };
        self.store.front.loading = true;
        self.store.front.error = None;
        let epoch = self.epoch;
        self.spawn(from, "front", move || {
            let platform = rd_core::ua::host_platform();
            let Some(s) = Session::from_saved(rd_core::session::http_client(), &saved, platform) else {
                return Work::Front { epoch, result: Err("The saved client ID is not valid".into()), saved: None };
            };
            let (path, title) = if s.is_signed_in() { ("/best", "Best") } else { ("/hot", "Hot") };
            let result = s
                .get(path, &[("limit", "50")], Priority::Page)
                .and_then(|r| rd_core::model::parse_posts(&r.json))
                .map(|l| {
                    let rows = l
                        .items
                        .into_iter()
                        .map(|p| PostRow { title: p.title, subreddit: p.subreddit, score: p.score, comments: p.num_comments, over_18: p.over_18 })
                        .collect();
                    (title.to_string(), rows)
                })
                .map_err(|e| e.to_string());
            if let Err(e) = &result {
                crate::log(&format!("front: {e}"));
            }
            Work::Front { epoch, result, saved: Some(s.to_saved(&saved.device_id)) }
        });
    }
}

/// The saved account after the user saved `cfg`: a different client ID is a different app, so
/// its tokens and device ID start over; otherwise only the redirect URI and user agent change.
fn merge(old: Option<&Saved>, cfg: &ApiConfig) -> Saved {
    match old {
        Some(o) if o.client_id == cfg.client_id => {
            Saved { redirect_uri: Some(cfg.redirect_uri.clone()), user_agent: cfg.user_agent.clone(), ..o.clone() }
        }
        _ => Saved::from_config(cfg),
    }
}

impl Rig<RdHost> for AppRig {
    fn split(&mut self) -> Split<'_, RdHost> {
        Split { mounter: &mut self.mounter, views: Views { store: &self.store, now_ms: self.now_ms }, measure: &self.measure }
    }
    fn deliver(&mut self, _to: MachineId, _msg: &RdMsg, _p: &CxParts<u32>, _fx: &mut Effects<'_, RdHost>) -> Handled {
        Handled::No
    }
    fn timer(&mut self, _o: MachineId, _id: TimerId, _p: &CxParts<u32>, _fx: &mut Effects<'_, RdHost>) {}
    fn app_fx(&mut self, from: MachineId, fx: RdFx, _p: &CxParts<u32>, _out: &mut Effects<'_, RdHost>) {
        match fx {
            RdFx::Toast(m) => self.store.toast(m, self.now_ms),
            RdFx::SetField(f, v) => {
                self.store.draft.set(f, v);
                self.store.errors.retain(|e| e.field != f.key());
            }
            RdFx::SaveApi => self.save_api(false),
            RdFx::PhoneStart => {
                if self.phone.is_none() {
                    self.phone_from = Some(from);
                    self.open_phone();
                }
            }
            RdFx::PhoneStop => self.close_phone(),
            RdFx::LoadFront => {
                if !self.store.front.loading {
                    self.load_front(from);
                }
            }
        }
    }
    fn log(&mut self, line: &str) {
        plx_base::eventlog::log(line)
    }
    fn prepare(&mut self, b: &mut Budget, present: &mut Present) {
        crate::images::prepare(b, present);
    }
    fn ls2_pump(&mut self) {
        crate::wl::ls2_pump()
    }
    fn opaque_route(&mut self, _video: bool) {}
    fn clear_opaque_region(&mut self) {
        crate::wl::clear_opaque_region()
    }
    fn now_us(&self) -> u64 {
        crate::now_us()
    }
    fn back_at_root(&mut self) {
        plx_platform::tv::home::go_home()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn saving_keeps_tokens_unless_the_client_id_changes() {
        let a = ApiConfig::from_fields("abcdefghijklmn", "", "").unwrap();
        let mut old = Saved::from_config(&a);
        old.refresh_token = Some("RT".into());
        let ua = ApiConfig::from_fields("abcdefghijklmn", "", "webos:x.y:v1 (by /u/me)").unwrap();
        let m = merge(Some(&old), &ua);
        assert_eq!((m.refresh_token.as_deref(), m.device_id.as_str()), (Some("RT"), old.device_id.as_str()));
        assert_eq!(m.user_agent.as_deref(), Some("webos:x.y:v1 (by /u/me)"));
        let other = ApiConfig::from_fields("zyxwvutsrqponm", "", "").unwrap();
        let m = merge(Some(&old), &other);
        assert_eq!(m.refresh_token, None);
        assert_ne!(m.device_id, old.device_id);
    }
}
