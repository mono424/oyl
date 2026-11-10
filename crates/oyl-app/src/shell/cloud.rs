//! Oyl Cloud: signing in and out, the per-paper "Sync to Oyl Cloud", the
//! open paper's background sync, its reviews in the Agent tab, and papers
//! opened from the cloud. Nothing leaves the Mac until the user signs in
//! and turns sync on for a paper.

use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use gpui_kit::prelude::*;
use gpui_kit::{Context, Entity, Focusable as _, Hsla, PathPromptOptions, Task, Window, px};
use oyl_chat::ReviewChoice;
use oyl_core::{CloudAccount, LayoutMode};
use oyl_sync::sign_in::{self, Finished};
use oyl_sync::{
    Client, Event, Keychain, PaperInfo, PaperSync, RemotePaper, Runner, Status, SyncState, Timing,
    TokenStore,
};
use oyl_ui::ActiveTokens as _;

use super::{Shell, ToastAction, ToastKind};
use crate::cloud_view::{CloudCardEvent, CloudSignInView, SignInStage};
use crate::layout;
use crate::overlays::Placement;

/// The id of the sign-in card in the overlay host.
pub(crate) const SIGN_IN: &str = "cloud-sign-in";

/// What the shell's Oyl Cloud side starts with.
pub struct CloudOptions {
    /// The cloud's address, over the settings and `OYL_CLOUD_URL` (the
    /// self-test's mock cloud).
    pub url: Option<String>,
    /// Where the key is kept: the Keychain, or memory for the self-test.
    pub tokens: Arc<dyn TokenStore>,
    /// The background loop's timing.
    pub timing: Timing,
}

impl Default for CloudOptions {
    fn default() -> Self {
        CloudOptions {
            url: None,
            tokens: Arc::new(Keychain),
            timing: Timing::default(),
        }
    }
}

/// The shell's Oyl Cloud state.
pub(crate) struct CloudState {
    pub(crate) options: CloudOptions,
    /// The key of the signed-in account, once read from the Keychain.
    pub(crate) token: Option<String>,
    /// The sign-in under way: set to stop waiting.
    pub(crate) signing_in: Option<Arc<AtomicBool>>,
    /// The account's papers that are not on this Mac, for the library.
    pub(crate) remote: Vec<RemotePaper>,
    /// A paper being downloaded.
    pub(crate) downloading: Option<String>,
    pub(crate) remote_task: Option<Task<()>>,
    /// The sign-in card.
    pub(crate) card: Entity<CloudSignInView>,
}

impl CloudState {
    pub(crate) fn new(options: CloudOptions, card: Entity<CloudSignInView>) -> CloudState {
        CloudState {
            options,
            token: None,
            signing_in: None,
            remote: Vec::new(),
            downloading: None,
            remote_task: None,
            card,
        }
    }
}

/// The open paper's sync.
pub(crate) struct PaperCloud {
    pub(crate) runner: Runner,
    pub(crate) status: Option<Status>,
    _events: Task<()>,
}

/// The header's cloud glyph: which icon, its color, its tooltip.
pub(crate) struct Glyph {
    pub(crate) icon: gpui_kit::assets::IconName,
    pub(crate) color: Hsla,
    pub(crate) tooltip: String,
}

/// "Signed in to Oyl Cloud as Ada".
pub(crate) fn signed_in_line(account: &CloudAccount) -> String {
    format!("Signed in to Oyl Cloud as {}", account.display())
}

/// "From MacBook Pro · 2 files updated".
pub(crate) fn applied_line(label: &str, files: usize) -> String {
    match files {
        0 => label.to_string(),
        1 => format!("{label} · 1 file updated"),
        n => format!("{label} · {n} files updated"),
    }
}

impl Shell {
    /// The cloud's address.
    pub(crate) fn cloud_url(&self) -> String {
        match &self.cloud.options.url {
            Some(url) => url.trim_end_matches('/').to_string(),
            None => oyl_sync::cloud_url(self.settings.cloud_url.as_deref()),
        }
    }

    /// The account signed in to this cloud, with its key at hand.
    pub(crate) fn cloud_account(&self) -> Option<&CloudAccount> {
        self.cloud.token.as_ref()?;
        self.settings
            .cloud_account
            .as_ref()
            .filter(|account| account.url == self.cloud_url())
    }

    fn cloud_client(&self) -> Option<Client> {
        self.cloud_account()?;
        Some(Client::new(&self.cloud_url(), self.cloud.token.clone()))
    }

    /// At startup: the key of the account signed in last time, read off
    /// the main thread (the Keychain may ask the user first).
    pub(crate) fn init_cloud(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let url = self.cloud_url();
        if self
            .settings
            .cloud_account
            .as_ref()
            .is_none_or(|account| account.url != url)
        {
            return;
        }
        let tokens = self.cloud.options.tokens.clone();
        let key = oyl_sync::token_key(&url);
        cx.spawn_in(window, async move |this, cx| {
            let token = cx.background_spawn(async move { tokens.load(&key) }).await;
            let _ = this.update_in(cx, |this, window, cx| {
                match token {
                    Some(token) => {
                        this.cloud.token = Some(token);
                        this.start_paper_sync(window, cx);
                        this.refresh_cloud_papers(cx);
                    }
                    None => {
                        // The key is gone (removed from the Keychain): signed out.
                        this.settings.cloud_account = None;
                        this.save_settings(cx);
                    }
                }
                this.sync_library(cx);
                cx.notify();
            });
        })
        .detach();
    }

    // -- Signing in and out ---------------------------------------------------

    /// Shows the sign-in card and starts a device sign-in.
    pub(crate) fn start_cloud_sign_in(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(account) = self.cloud_account() {
            let line = signed_in_line(account);
            self.toast(ToastKind::Info, line, window, cx);
            return;
        }
        let card = self.cloud.card.clone();
        let focus = card.focus_handle(cx);
        self.overlays.update(cx, |host, cx| {
            host.show(
                SIGN_IN,
                card.clone().into(),
                Some(focus),
                Placement::Center {
                    width: px(layout::CLOUD_CARD_WIDTH),
                },
                window,
                cx,
            )
        });
        self.begin_sign_in(window, cx);
    }

    fn begin_sign_in(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(cancel) = self.cloud.signing_in.take() {
            cancel.store(true, Ordering::Relaxed);
        }
        let cancel = Arc::new(AtomicBool::new(false));
        self.cloud.signing_in = Some(cancel.clone());
        self.cloud
            .card
            .update(cx, |card, cx| card.set_stage(SignInStage::Starting, cx));
        let url = self.cloud_url();
        cx.spawn_in(window, async move |this, cx| {
            let client = Client::new(&url, None);
            let started = cx
                .background_spawn({
                    let client = client.clone();
                    async move { sign_in::start(&client, &oyl_sync::device_name()) }
                })
                .await;
            let start = match started {
                Ok(start) => start,
                Err(err) => {
                    let _ = this.update(cx, |this, cx| {
                        this.cloud.card.update(cx, |card, cx| {
                            card.set_stage(SignInStage::Failed(err.to_string()), cx)
                        })
                    });
                    return;
                }
            };
            let stage = SignInStage::Waiting {
                code: start.user_code.clone(),
                url: start.verify_url.clone(),
            };
            let still = this
                .update(cx, |this, cx| {
                    if cancel.load(Ordering::Relaxed) {
                        return false;
                    }
                    this.cloud
                        .card
                        .update(cx, |card, cx| card.set_stage(stage, cx));
                    true
                })
                .unwrap_or(false);
            if !still {
                return;
            }
            let waited = cx
                .background_spawn({
                    let cancel = cancel.clone();
                    async move { sign_in::wait(&client, &start, &cancel) }
                })
                .await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.sign_in_finished(url, waited, window, cx)
            });
        })
        .detach();
    }

    fn sign_in_finished(
        &mut self,
        url: String,
        finished: oyl_sync::Result<Finished>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let stage = match finished {
            Ok(Finished::Approved { token, account }) => {
                self.cloud.signing_in = None;
                let account = CloudAccount {
                    id: account.id,
                    name: account.name,
                    email: account.email,
                    url: url.clone(),
                };
                if let Err(err) = self
                    .cloud
                    .options
                    .tokens
                    .save(&oyl_sync::token_key(&url), &token)
                {
                    self.toast(
                        ToastKind::Error,
                        format!("Signed in, but the key could not be kept: {err}"),
                        window,
                        cx,
                    );
                }
                self.cloud.token = Some(token);
                let line = signed_in_line(&account);
                self.settings.cloud_account = Some(account);
                self.save_settings(cx);
                self.overlays
                    .update(cx, |host, cx| host.dismiss_if(SIGN_IN, window, cx));
                self.toast(ToastKind::Success, line, window, cx);
                self.start_paper_sync(window, cx);
                self.refresh_cloud_papers(cx);
                self.sync_library(cx);
                cx.notify();
                return;
            }
            Ok(Finished::Cancelled) => return,
            Ok(Finished::Denied) => SignInStage::Declined,
            Ok(Finished::Expired) => SignInStage::Expired,
            Err(err) => SignInStage::Failed(err.to_string()),
        };
        self.cloud.signing_in = None;
        self.cloud
            .card
            .update(cx, |card, cx| card.set_stage(stage, cx));
    }

    /// The sign-in card's buttons.
    pub(crate) fn on_cloud_card_event(
        &mut self,
        _card: &Entity<CloudSignInView>,
        event: &CloudCardEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            CloudCardEvent::OpenBrowser => {
                if let SignInStage::Waiting { url, .. } = self.cloud.card.read(cx).stage() {
                    cx.open_url(url);
                }
            }
            CloudCardEvent::Retry => self.begin_sign_in(window, cx),
            CloudCardEvent::Cancel => {
                self.cancel_sign_in();
                self.overlays
                    .update(cx, |host, cx| host.dismiss_if(SIGN_IN, window, cx));
            }
        }
    }

    /// Stops waiting for a sign-in (the card went away).
    pub(crate) fn cancel_sign_in(&mut self) {
        if let Some(cancel) = self.cloud.signing_in.take() {
            cancel.store(true, Ordering::Relaxed);
        }
    }

    /// Forgets the key and the account; papers stop syncing (nothing in the
    /// cloud is deleted).
    pub(crate) fn sign_out_of_cloud(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(account) = self.settings.cloud_account.take() else {
            return;
        };
        self.forget_cloud_key(&account.url, cx);
        self.save_settings(cx);
        self.stop_paper_sync(cx);
        self.cloud.remote.clear();
        self.sync_library(cx);
        self.toast(
            ToastKind::Info,
            "Signed out of Oyl Cloud. Your papers stay on this Mac.",
            window,
            cx,
        );
        cx.notify();
    }

    fn forget_cloud_key(&mut self, url: &str, cx: &mut Context<Self>) {
        self.cloud.token = None;
        let tokens = self.cloud.options.tokens.clone();
        let key = oyl_sync::token_key(url);
        cx.background_spawn(async move {
            if let Err(err) = tokens.delete(&key) {
                log::warn!("forgetting the Oyl Cloud key: {err}");
            }
        })
        .detach();
    }

    // -- The open paper's sync ------------------------------------------------

    /// Whether the open paper syncs (sync turned on for it).
    pub(crate) fn paper_syncs(&self) -> Option<bool> {
        let paper = self.paper.as_ref()?;
        Some(paper.cloud.is_some() || SyncState::is_enabled(paper.root()))
    }

    /// Turns "Sync to Oyl Cloud" on or off for the open paper.
    pub(crate) fn toggle_cloud_sync(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(paper) = self.paper.as_ref() else {
            return;
        };
        let root = paper.root().to_path_buf();
        let name = paper.project.name().to_string();
        let enabled = SyncState::is_enabled(&root);
        if !enabled && self.cloud_account().is_none() {
            let action: ToastAction = Rc::new(
                |shell: &mut Shell, window: &mut Window, cx: &mut Context<Shell>| {
                    shell.start_cloud_sign_in(window, cx)
                },
            );
            self.toast_with_action(
                ToastKind::Info,
                "Sign in to Oyl Cloud first, then turn sync on for this paper.",
                "Sign in",
                action,
                window,
                cx,
            );
            return;
        }
        if let Err(err) = SyncState::set_enabled(&root, !enabled) {
            self.toast(
                ToastKind::Error,
                format!("Sync could not be changed for this paper: {err}"),
                window,
                cx,
            );
            return;
        }
        if enabled {
            self.stop_paper_sync(cx);
            self.toast(
                ToastKind::Info,
                format!("“{name}” no longer syncs. Nothing was removed from Oyl Cloud."),
                window,
                cx,
            );
        } else {
            self.toast(
                ToastKind::Success,
                format!("“{name}” syncs to Oyl Cloud"),
                window,
                cx,
            );
            self.start_paper_sync(window, cx);
        }
        cx.notify();
    }

    /// Starts the open paper's background sync when it syncs and an account
    /// is signed in. Does nothing when it runs already.
    pub(crate) fn start_paper_sync(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(client) = self.cloud_client() else {
            return;
        };
        let Some(account) = self.cloud_account().map(|account| account.id.clone()) else {
            return;
        };
        let timing = self.cloud.options.timing;
        let Some(paper) = self.paper.as_mut() else {
            return;
        };
        if paper.cloud.is_some() || !SyncState::is_enabled(paper.root()) {
            return;
        }
        let Some(history) = paper.history.clone() else {
            return;
        };
        let main = paper
            .project
            .main_path()
            .strip_prefix(paper.root())
            .map(|path| path.to_string_lossy().replace('\\', "/"))
            .unwrap_or_else(|_| paper.engine().default_main().to_string());
        let info = PaperInfo {
            name: paper.project.name().to_string(),
            engine: paper.engine(),
            main,
        };
        let (runner, events) =
            Runner::start(PaperSync::new(history, client, &account, info), timing);
        let epoch = paper.epoch;
        let task = cx.spawn_in(window, async move |this, cx| {
            while let Ok(event) = events.recv().await {
                let alive = this
                    .update_in(cx, |this, window, cx| {
                        this.cloud_event(epoch, event, window, cx)
                    })
                    .is_ok();
                if !alive {
                    break;
                }
            }
        });
        paper.cloud = Some(PaperCloud {
            runner,
            status: Some(Status::Syncing),
            _events: task,
        });
        cx.notify();
    }

    /// Stops the open paper's background sync and takes its review away.
    pub(crate) fn stop_paper_sync(&mut self, cx: &mut Context<Self>) {
        if let Some(paper) = self.paper.as_mut() {
            paper.cloud = None;
        }
        self.chat
            .update(cx, |chat, cx| chat.set_cloud_review(None, None, cx));
        cx.notify();
    }

    /// Files of the paper changed (a save, or anything else on disk): push
    /// a little later.
    pub(crate) fn cloud_saved(&self) {
        if let Some(cloud) = self.paper.as_ref().and_then(|paper| paper.cloud.as_ref()) {
            cloud.runner.saved();
        }
    }

    /// Pull and push now.
    pub(crate) fn sync_now(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match self.paper.as_ref().and_then(|paper| paper.cloud.as_ref()) {
            Some(cloud) => cloud.runner.sync_now(),
            None => self.toggle_cloud_sync(window, cx),
        }
    }

    fn cloud_event(
        &mut self,
        epoch: u64,
        event: Event,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.paper.as_ref().map(|paper| paper.epoch) != Some(epoch) {
            return;
        }
        match event {
            Event::Status(status) => {
                if let Some(cloud) = self.paper.as_mut().and_then(|paper| paper.cloud.as_mut()) {
                    cloud.status = Some(status);
                }
                cx.notify();
            }
            Event::Applied(applied) => {
                self.paper_changed_by_agent(window, cx);
                self.request_compile(window, cx);
                let line = applied_line(&applied.label, applied.files.len());
                self.toast(ToastKind::Info, line, window, cx);
            }
            Event::Review { review, label } => {
                let shown = self.chat.read(cx).cloud_review().is_some();
                let note = shown
                    .then(|| "The paper changed again, so the review was updated.".to_string());
                let view = super::branches::review_view(&review, note);
                self.chat.update(cx, |chat, cx| {
                    chat.set_cloud_review(Some(view), Some(label.clone()), cx)
                });
                if !shown {
                    let action: ToastAction = Rc::new(
                        |shell: &mut Shell, window: &mut Window, cx: &mut Context<Shell>| {
                            shell.set_mode(LayoutMode::Agent, window, cx)
                        },
                    );
                    self.toast_with_action(
                        ToastKind::Info,
                        format!("Changes from Oyl Cloud need review ({label})"),
                        "Review",
                        action,
                        window,
                        cx,
                    );
                }
            }
            Event::ReviewSettled => {
                self.chat
                    .update(cx, |chat, cx| chat.set_cloud_review(None, None, cx));
                self.paper_changed_by_agent(window, cx);
                self.request_compile(window, cx);
            }
            Event::Pushed(n) => log::info!("sync: pushed revision {n}"),
            Event::SignedOut => {
                if let Some(account) = self.settings.cloud_account.take() {
                    self.forget_cloud_key(&account.url, cx);
                    self.save_settings(cx);
                }
                // Stopped from a task of its own: this event arrives
                // through the sync's own task, which stopping drops.
                cx.spawn(async move |this, cx| {
                    let _ = this.update(cx, |this, cx| this.stop_paper_sync(cx));
                })
                .detach();
                self.cloud.remote.clear();
                self.sync_library(cx);
                let action: ToastAction = Rc::new(
                    |shell: &mut Shell, window: &mut Window, cx: &mut Context<Shell>| {
                        shell.start_cloud_sign_in(window, cx)
                    },
                );
                self.toast_with_action(
                    ToastKind::Error,
                    "Signed out of Oyl Cloud: this Mac was disconnected. Sign in again to sync.",
                    "Sign in",
                    action,
                    window,
                    cx,
                );
            }
        }
    }

    /// The user applies the review of changes from Oyl Cloud.
    pub(crate) fn apply_cloud_review(
        &mut self,
        choices: &[(String, ReviewChoice)],
        cx: &mut Context<Self>,
    ) {
        let resolution = super::branches::resolution(choices);
        match self.paper.as_ref().and_then(|paper| paper.cloud.as_ref()) {
            Some(cloud) => cloud.runner.apply_review(resolution),
            None => self
                .chat
                .update(cx, |chat, cx| chat.set_cloud_review(None, None, cx)),
        }
    }

    /// The user discards the review of changes from Oyl Cloud: the paper
    /// stays as it is and goes up as the latest revision.
    pub(crate) fn discard_cloud_review(&mut self, cx: &mut Context<Self>) {
        match self.paper.as_ref().and_then(|paper| paper.cloud.as_ref()) {
            Some(cloud) => cloud.runner.discard_review(),
            None => self
                .chat
                .update(cx, |chat, cx| chat.set_cloud_review(None, None, cx)),
        }
    }

    /// The header's glyph for the open paper's sync; nothing when it does
    /// not sync.
    pub(crate) fn cloud_glyph(&self, cx: &gpui_kit::App) -> Option<Glyph> {
        use gpui_kit::assets::IconName;
        let status = self.paper.as_ref()?.cloud.as_ref()?.status.clone()?;
        let k = cx.tokens();
        let (icon, color) = match &status {
            Status::Syncing => (IconName::CloudUpload, k.text_faint),
            Status::Synced => (IconName::CloudCheck, k.text_faint),
            Status::Offline { .. } => (IconName::CloudOff, k.text_faint),
            Status::NeedsReview => (IconName::CloudAlert, k.accent),
            Status::SignedOut => (IconName::CloudOff, k.danger),
            Status::Error(_) => (IconName::CloudAlert, k.danger),
        };
        Some(Glyph {
            icon,
            color,
            tooltip: status.describe(),
        })
    }

    /// The glyph was clicked: to the review, or sync now.
    pub(crate) fn cloud_glyph_clicked(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let status = self
            .paper
            .as_ref()
            .and_then(|paper| paper.cloud.as_ref())
            .and_then(|cloud| cloud.status.clone());
        match status {
            Some(Status::NeedsReview) => self.set_mode(LayoutMode::Agent, window, cx),
            Some(Status::SignedOut) => self.start_cloud_sign_in(window, cx),
            _ => self.sync_now(window, cx),
        }
    }

    // -- Papers in the cloud ------------------------------------------------

    /// What the library card shows of Oyl Cloud.
    pub(crate) fn library_cloud_data(&self) -> crate::library_view::CloudData {
        let Some(account) = self.cloud_account() else {
            return crate::library_view::CloudData::default();
        };
        let papers = self
            .cloud
            .remote
            .iter()
            .map(|paper| crate::library_view::CloudPaper {
                id: paper.id.clone(),
                name: paper.name.clone(),
                engine: paper.engine(),
                updated: chrono::DateTime::parse_from_rfc3339(&paper.updated)
                    .ok()
                    .map(|time| time.with_timezone(&chrono::Utc)),
                from: paper.updated_by.as_deref().and_then(|author| match author {
                    "web" => Some("the web".to_string()),
                    author => author
                        .strip_prefix("device:")
                        .map(str::to_string)
                        .filter(|name| !name.is_empty()),
                }),
            })
            .collect();
        crate::library_view::CloudData {
            account: Some(account.display()),
            papers,
            downloading: self.cloud.downloading.clone(),
        }
    }

    /// Re-reads the account's papers that are not on this Mac (for the
    /// library), off the main thread.
    pub(crate) fn refresh_cloud_papers(&mut self, cx: &mut Context<Self>) {
        let Some(client) = self.cloud_client() else {
            self.cloud.remote.clear();
            return;
        };
        let local: Vec<PathBuf> = self
            .library
            .entries()
            .iter()
            .map(|entry| entry.path.clone())
            .collect();
        self.cloud.remote_task = Some(cx.spawn(async move |this, cx| {
            let listed = cx
                .background_spawn(async move {
                    let here: std::collections::HashSet<String> = local
                        .iter()
                        .filter_map(|path| SyncState::paper_id_of(path))
                        .collect();
                    client.papers().map(|papers| {
                        papers
                            .into_iter()
                            .filter(|paper| !paper.deleted && !here.contains(&paper.id))
                            .collect::<Vec<_>>()
                    })
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                match listed {
                    Ok(papers) => this.cloud.remote = papers,
                    Err(err) => log::info!("listing the papers in Oyl Cloud: {err}"),
                }
                this.sync_library(cx);
            });
        }));
    }

    /// "Open" on a paper of the cloud: pick where it goes, then download it.
    pub(crate) fn open_from_cloud(
        &mut self,
        id: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let answer = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Download Here".into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            let picked = answer.await;
            let _ = this.update_in(cx, |this, window, cx| match picked {
                Ok(Ok(Some(paths))) => {
                    if let Some(parent) = paths.into_iter().next() {
                        this.download_cloud_paper(id, parent, window, cx);
                    }
                }
                Ok(Ok(None)) | Err(_) => {}
                Ok(Err(err)) => this.toast(
                    ToastKind::Error,
                    format!("The folder picker could not open: {err:#}"),
                    window,
                    cx,
                ),
            });
        })
        .detach();
    }

    /// Downloads the cloud paper `id` into a new folder inside `parent` and
    /// opens it.
    pub(crate) fn download_cloud_paper(
        &mut self,
        id: String,
        parent: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(client) = self.cloud_client() else {
            return;
        };
        let Some(account) = self.cloud_account().map(|account| account.id.clone()) else {
            return;
        };
        let Some(remote) = self
            .cloud
            .remote
            .iter()
            .find(|paper| paper.id == id)
            .cloned()
        else {
            return;
        };
        if self.cloud.downloading.is_some() {
            return;
        }
        self.cloud.downloading = Some(id);
        self.sync_library(cx);
        cx.spawn_in(window, async move |this, cx| {
            let name = remote.name.clone();
            let downloaded = cx
                .background_spawn(async move {
                    oyl_sync::download_paper(&client, &account, &remote, &parent)
                })
                .await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.cloud.downloading = None;
                match downloaded {
                    Ok(folder) => {
                        this.toast(
                            ToastKind::Success,
                            format!("Downloaded “{name}” from Oyl Cloud"),
                            window,
                            cx,
                        );
                        this.open_paper(folder, window, cx);
                        this.refresh_cloud_papers(cx);
                    }
                    Err(err) => this.toast(
                        ToastKind::Error,
                        format!("“{name}” could not be downloaded: {err}"),
                        window,
                        cx,
                    ),
                }
                this.sync_library(cx);
            });
        })
        .detach();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lines_read_plainly() {
        let account = CloudAccount {
            name: "Ada".into(),
            ..CloudAccount::default()
        };
        assert_eq!(signed_in_line(&account), "Signed in to Oyl Cloud as Ada");
        assert_eq!(
            applied_line("From MacBook Pro", 1),
            "From MacBook Pro · 1 file updated"
        );
        assert_eq!(
            applied_line("From the web", 3),
            "From the web · 3 files updated"
        );
        assert_eq!(applied_line("From the web", 0), "From the web");
    }
}
