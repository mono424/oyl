//! The library card: the welcome screen when nothing is open, and ⌘O.
//!
//! Recent papers (missing folders greyed, with "Forget"), a filter, the
//! new-paper form (name, engine, template, folder) and "Open folder…", the
//! signed-in account's papers in Oyl Cloud that are not on this Mac (with
//! "Open"), and one quiet line of setup truth at the bottom with "Sign in
//! to Oyl Cloud". The card only shows and asks; the shell does the work
//! ([`LibraryEvent`]).

use std::path::{Path, PathBuf};

use chrono::Utc;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::{Disableable as _, IconName, Sizable as _, StyledExt as _};
use gpui_kit::prelude::*;
use std::sync::Arc;

use gpui_kit::{
    AnyElement, App, Context, Entity, EventEmitter, FocusHandle, Focusable, Image, ImageFormat,
    ObjectFit, Render, SharedString, Subscription, Window, div, img, px,
};
use oyl_core::LibraryEntry;
use oyl_core::templates::{self, DEFAULT_TEMPLATE_ID};
use oyl_model::Engine;
use oyl_ui::prelude::*;

use crate::fuzzy;

/// Oyl's logo (artwork on transparency).
const LOGO: &[u8] = include_bytes!("../../../assets/logo.png");
/// The logo's size in the card's header.
const LOGO_SIZE: f32 = 48.;

/// What the card asks the shell to do.
#[derive(Clone, Debug, PartialEq)]
pub enum LibraryEvent {
    /// Open the paper in this folder.
    Open(PathBuf),
    /// Remove a (missing) paper from the library.
    Forget(PathBuf),
    /// Create a paper.
    Create {
        /// Its name (empty: "Untitled paper").
        name: String,
        /// Typst or LaTeX.
        engine: Engine,
        /// `Template::id`.
        template: &'static str,
        /// The folder the paper's folder is created in.
        parent: PathBuf,
    },
    /// Pick a folder to open as a paper.
    OpenFolder,
    /// Pick the folder new papers go in.
    ChooseFolder,
    /// Download tectonic.
    InstallLatex,
    /// Sign this Mac in to Oyl Cloud.
    SignInToCloud,
    /// Download this cloud paper (its id) into a folder and open it.
    OpenFromCloud(String),
}

/// A paper in Oyl Cloud that is not on this Mac.
#[derive(Clone, Debug, PartialEq)]
pub struct CloudPaper {
    /// Its cloud id.
    pub id: String,
    /// Its name.
    pub name: String,
    /// Typst or LaTeX.
    pub engine: Engine,
    /// When it last changed.
    pub updated: Option<chrono::DateTime<Utc>>,
    /// Where it last changed ("MacBook Pro", "the web").
    pub from: Option<String>,
}

/// What the card shows of Oyl Cloud.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct CloudData {
    /// The signed-in account ("Ada"), if any.
    pub account: Option<String>,
    /// Its papers that are not on this Mac.
    pub papers: Vec<CloudPaper>,
    /// The paper being downloaded.
    pub downloading: Option<String>,
}

/// "Changed 3 hours ago on MacBook Pro".
pub fn cloud_line(paper: &CloudPaper, now: chrono::DateTime<Utc>) -> String {
    let when = paper
        .updated
        .map(|then| oyl_chat::thread::relative_time(then, now))
        .unwrap_or_default();
    match (when.is_empty(), &paper.from) {
        (false, Some(from)) => format!("Changed {when} on {from}"),
        (false, None) => format!("Changed {when}"),
        (true, Some(from)) => format!("Changed on {from}"),
        (true, None) => "In Oyl Cloud".to_string(),
    }
}

/// What the shell shows in the card.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct LibraryData {
    /// The papers, most recently opened first.
    pub entries: Vec<LibraryEntry>,
    /// Where new papers go.
    pub papers_dir: PathBuf,
    /// The paper open right now.
    pub current: Option<PathBuf>,
    /// "Typst built in · LaTeX: latexmk · Agent: …".
    pub setup: String,
    /// LaTeX can be installed from here.
    pub can_install_latex: bool,
    /// LaTeX is being installed.
    pub installing_latex: bool,
    /// A paper is being created.
    pub creating: bool,
    /// Oyl Cloud: the account and its papers not on this Mac.
    pub cloud: CloudData,
}

/// The library card.
pub struct LibraryView {
    data: LibraryData,
    /// Oyl's logo, above the welcome line.
    logo: Arc<Image>,
    filter: Entity<InputState>,
    name: Entity<InputState>,
    form_open: bool,
    engine: Engine,
    template: &'static str,
    focus_handle: FocusHandle,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<LibraryEvent> for LibraryView {}

impl Focusable for LibraryView {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        if self.form_open {
            self.name.focus_handle(cx)
        } else if self.data.entries.is_empty() {
            self.focus_handle.clone()
        } else {
            self.filter.focus_handle(cx)
        }
    }
}

/// `~/Papers/owls` for a path in the home folder.
pub fn short_path(path: &Path) -> String {
    if let Some(home) = dirs::home_dir()
        && let Ok(rest) = path.strip_prefix(&home)
    {
        return if rest.as_os_str().is_empty() {
            "~".into()
        } else {
            format!("~/{}", rest.display())
        };
    }
    path.display().to_string()
}

/// The entries matching a filter, best first (all of them, in order, for an
/// empty filter).
pub fn filter_entries<'a>(entries: &'a [LibraryEntry], filter: &str) -> Vec<&'a LibraryEntry> {
    let filter = filter.trim();
    if filter.is_empty() {
        return entries.iter().collect();
    }
    let mut scored: Vec<(i32, usize, &LibraryEntry)> = entries
        .iter()
        .enumerate()
        .filter_map(|(index, entry)| {
            let name = fuzzy::score(filter, &entry.name);
            let path = fuzzy::score(filter, &entry.path.display().to_string()).map(|s| s - 8);
            let best = match (name, path) {
                (Some(a), Some(b)) => Some(a.max(b)),
                (a, b) => a.or(b),
            }?;
            Some((best, index, entry))
        })
        .collect();
    scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
    scored.into_iter().map(|(_, _, entry)| entry).collect()
}

impl LibraryView {
    /// An empty card.
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let filter = cx.new(|cx| InputState::new(window, cx).placeholder("Filter papers"));
        let name = cx.new(|cx| InputState::new(window, cx).placeholder("Untitled paper"));
        let subscriptions = vec![
            cx.subscribe_in(&filter, window, |_, _, event: &InputEvent, _, cx| {
                if matches!(event, InputEvent::Change) {
                    cx.notify();
                }
            }),
            cx.subscribe_in(
                &name,
                window,
                |this: &mut Self, _, event: &InputEvent, _, cx| {
                    if matches!(event, InputEvent::PressEnter { .. }) {
                        this.create(cx);
                    }
                },
            ),
        ];
        Self {
            data: LibraryData::default(),
            filter,
            name,
            form_open: false,
            engine: Engine::Typst,
            template: DEFAULT_TEMPLATE_ID,
            logo: Arc::new(Image::from_bytes(ImageFormat::Png, LOGO.to_vec())),
            focus_handle: cx.focus_handle(),
            _subscriptions: subscriptions,
        }
    }

    /// Shows fresh data.
    pub fn set_data(&mut self, data: LibraryData, cx: &mut Context<Self>) {
        if self.data != data {
            self.data = data;
            cx.notify();
        }
    }

    /// Opens or closes the new-paper form; opening it focuses the name.
    pub fn set_form_open(&mut self, open: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.form_open = open;
        if open {
            self.engine = Engine::Typst;
            self.template = DEFAULT_TEMPLATE_ID;
            self.name.update(cx, |name, cx| {
                name.set_value("", window, cx);
                name.focus(window, cx);
            });
        } else {
            self.filter
                .update(cx, |filter, cx| filter.focus(window, cx));
        }
        cx.notify();
    }

    /// Clears the filter (each time the card opens).
    pub fn reset(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.form_open = false;
        self.filter
            .update(cx, |filter, cx| filter.set_value("", window, cx));
        cx.notify();
    }

    fn create(&mut self, cx: &mut Context<Self>) {
        if self.data.creating {
            return;
        }
        let name = self.name.read(cx).value().trim().to_string();
        cx.emit(LibraryEvent::Create {
            name,
            engine: self.engine,
            template: self.template,
            parent: self.data.papers_dir.clone(),
        });
    }

    fn render_header(&self, k: &Tokens) -> AnyElement {
        let first_run = self.data.entries.is_empty();
        let (title, line) = if first_run {
            (
                "A quiet place to write papers.",
                "A paper is a folder. Start a new one, or open one you already have.",
            )
        } else {
            (
                "Your papers",
                "Pick one up where you left it, or start another.",
            )
        };
        div()
            .v_flex()
            .gap(px(6.))
            .child(
                img(self.logo.clone())
                    .size(px(LOGO_SIZE))
                    .object_fit(ObjectFit::Contain)
                    .mb(px(6.)),
            )
            .child(
                div()
                    .font_family(k.serif_font.clone())
                    .text_2xl()
                    .text_color(k.text)
                    .child(title),
            )
            .child(div().text_sm().text_color(k.text_soft).child(line))
            .into_any_element()
    }

    fn render_actions(&self, cx: &mut Context<Self>) -> AnyElement {
        let has_entries = !self.data.entries.is_empty();
        div()
            .h_flex()
            .gap(px(8.))
            .when(has_entries, |this| {
                this.child(
                    div().flex_1().child(
                        Input::new(&self.filter)
                            .small()
                            .cleanable(true)
                            .prefix(gpui_kit::component::Icon::new(IconName::Search).small()),
                    ),
                )
            })
            .when(!has_entries, |this| this.child(div().flex_1()))
            .child(
                Button::new("library-new")
                    .primary()
                    .small()
                    .icon(IconName::Plus)
                    .label("New paper")
                    .on_click(cx.listener(|this, _, window, cx| {
                        let open = !this.form_open;
                        this.set_form_open(open, window, cx)
                    })),
            )
            .child(
                Button::new("library-open-folder")
                    .ghost()
                    .small()
                    .icon(IconName::FolderOpen)
                    .label("Open folder…")
                    .on_click(cx.listener(|_, _, _, cx| cx.emit(LibraryEvent::OpenFolder))),
            )
            .into_any_element()
    }

    fn render_form(&self, k: &Tokens, cx: &mut Context<Self>) -> AnyElement {
        let engine_button = |engine: Engine, cx: &mut Context<Self>| {
            let active = self.engine == engine;
            let (accent, accent_soft, soft, hover) =
                (k.accent, k.accent_soft, k.text_soft, k.surface_hover);
            div()
                .id(SharedString::from(format!("engine-{}", engine.label())))
                .px(px(12.))
                .h(px(26.))
                .flex()
                .items_center()
                .rounded(k.radius_s)
                .text_sm()
                .cursor_pointer()
                .when(active, |this| this.bg(accent_soft).text_color(accent))
                .when(!active, |this| {
                    this.text_color(soft).hover(move |style| style.bg(hover))
                })
                .child(engine.label())
                .on_click(cx.listener(move |this, _, _, cx| {
                    if this.engine != engine {
                        this.engine = engine;
                        this.template = DEFAULT_TEMPLATE_ID;
                        cx.notify();
                    }
                }))
        };
        let cards: Vec<AnyElement> = templates::templates(self.engine)
            .iter()
            .map(|template| {
                let selected = template.id == self.template;
                let id = template.id;
                let (accent, accent_soft, hairline, hover) =
                    (k.accent, k.accent_soft, k.hairline, k.surface_hover);
                oyl_ui::surface(cx)
                    .id(SharedString::from(format!("template-{id}")))
                    .rounded(k.radius_m)
                    .p(px(10.))
                    .v_flex()
                    .gap(px(2.))
                    .cursor_pointer()
                    .when(selected, |this| this.border_color(accent).bg(accent_soft))
                    .when(!selected, |this| {
                        this.border_color(hairline)
                            .hover(move |style| style.bg(hover))
                    })
                    .child(
                        div()
                            .text_sm()
                            .font_weight(gpui_kit::FontWeight::MEDIUM)
                            .text_color(if selected { k.accent } else { k.text })
                            .child(template.name),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(k.text_soft)
                            .child(template.description),
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.template = id;
                        cx.notify();
                    }))
                    .into_any_element()
            })
            .collect();
        let mut grid = div().v_flex().gap(px(8.));
        let mut cards = cards.into_iter();
        while let Some(first) = cards.next() {
            let second = cards.next();
            grid = grid.child(
                div()
                    .h_flex()
                    .gap(px(8.))
                    .child(div().flex_1().min_w_0().child(first))
                    .child(div().flex_1().min_w_0().children(second)),
            );
        }
        let creating = self.data.creating;
        div()
            .v_flex()
            .gap(px(12.))
            .p(px(14.))
            .rounded(k.radius_l)
            .bg(k.surface)
            .border_1()
            .border_color(k.hairline)
            .child(oyl_ui::eyebrow("New paper", cx))
            .child(Input::new(&self.name))
            .child(
                div()
                    .h_flex()
                    .gap(px(2.))
                    .p(px(2.))
                    .rounded(k.radius_m)
                    .bg(k.surface)
                    .border_1()
                    .border_color(k.hairline)
                    .child(engine_button(Engine::Typst, cx))
                    .child(engine_button(Engine::Latex, cx)),
            )
            .child(grid)
            .child(
                div()
                    .h_flex()
                    .gap(px(6.))
                    .text_xs()
                    .text_color(k.text_faint)
                    .child(format!("Created in {}", short_path(&self.data.papers_dir)))
                    .child(
                        Button::new("library-choose-folder")
                            .ghost()
                            .xsmall()
                            .label("Change…")
                            .on_click(
                                cx.listener(|_, _, _, cx| cx.emit(LibraryEvent::ChooseFolder)),
                            ),
                    ),
            )
            .child(
                div()
                    .h_flex()
                    .gap(px(8.))
                    .child(
                        Button::new("library-create")
                            .primary()
                            .small()
                            .label(if creating { "Creating…" } else { "Create" })
                            .loading(creating)
                            .disabled(creating)
                            .on_click(cx.listener(|this, _, _, cx| this.create(cx))),
                    )
                    .child(
                        Button::new("library-cancel")
                            .ghost()
                            .small()
                            .label("Cancel")
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.set_form_open(false, window, cx)
                            })),
                    ),
            )
            .into_any_element()
    }

    fn render_entries(&self, k: &Tokens, cx: &mut Context<Self>) -> AnyElement {
        let filter = self.filter.read(cx).value().to_string();
        let shown = filter_entries(&self.data.entries, &filter);
        if self.data.entries.is_empty() {
            return div().into_any_element();
        }
        if shown.is_empty() {
            return div()
                .py(px(18.))
                .text_sm()
                .text_color(k.text_faint)
                .child(format!("No paper matches “{}”.", filter.trim()))
                .into_any_element();
        }
        let now = Utc::now();
        let rows: Vec<AnyElement> = shown
            .into_iter()
            .enumerate()
            .map(|(index, entry)| self.render_row(index, entry, now, k, cx))
            .collect();
        div()
            .id("library-papers")
            .v_flex()
            .gap(px(1.))
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .children(rows)
            .into_any_element()
    }

    fn render_row(
        &self,
        index: usize,
        entry: &LibraryEntry,
        now: chrono::DateTime<Utc>,
        k: &Tokens,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let missing = entry.missing;
        let current = self.data.current.as_deref() == Some(entry.path.as_path());
        let when = match (missing, entry.last_opened) {
            (true, _) => "Folder not found".to_string(),
            (false, Some(then)) => oyl_chat::thread::relative_time(then, now),
            (false, None) => "Never opened".to_string(),
        };
        let path = entry.path.clone();
        let hover = k.surface_hover;
        let name_color = if missing { k.text_faint } else { k.text };
        div()
            .id(("library-paper", index))
            .h_flex()
            .gap(px(10.))
            .px(px(10.))
            .py(px(7.))
            .rounded(k.radius_m)
            .when(!missing, |this| {
                let path = path.clone();
                this.cursor_pointer()
                    .hover(move |style| style.bg(hover))
                    .on_click(
                        cx.listener(move |_, _, _, cx| cx.emit(LibraryEvent::Open(path.clone()))),
                    )
            })
            .child(
                div()
                    .v_flex()
                    .flex_1()
                    .min_w_0()
                    .gap(px(1.))
                    .child(
                        div()
                            .truncate()
                            .text_sm()
                            .text_color(name_color)
                            .child(entry.name.clone()),
                    )
                    .child(
                        div()
                            .truncate()
                            .text_xs()
                            .text_color(k.text_faint)
                            .child(short_path(&entry.path)),
                    ),
            )
            .when(current, |this| this.child(oyl_ui::accent_chip("Open", cx)))
            .child(oyl_ui::chip(entry.engine.label(), cx))
            .child(
                div()
                    .w(px(96.))
                    .flex()
                    .justify_end()
                    .text_xs()
                    .text_color(k.text_faint)
                    .child(when),
            )
            .when(missing, |this| {
                this.child(
                    Button::new(("library-forget", index))
                        .ghost()
                        .xsmall()
                        .label("Forget")
                        .on_click(cx.listener(move |_, _, _, cx| {
                            cx.emit(LibraryEvent::Forget(path.clone()))
                        })),
                )
            })
            .into_any_element()
    }

    /// The account's papers in Oyl Cloud that are not on this Mac.
    fn render_cloud(&self, k: &Tokens, cx: &mut Context<Self>) -> Option<AnyElement> {
        let cloud = &self.data.cloud;
        if cloud.account.is_none() || cloud.papers.is_empty() {
            return None;
        }
        let now = Utc::now();
        let hover = k.surface_hover;
        let rows = cloud.papers.iter().enumerate().map(|(index, paper)| {
            let id = paper.id.clone();
            let downloading = cloud.downloading.as_deref() == Some(paper.id.as_str());
            div()
                .id(("library-cloud-paper", index))
                .h_flex()
                .gap(px(10.))
                .px(px(10.))
                .py(px(6.))
                .rounded(k.radius_m)
                .hover(move |style| style.bg(hover))
                .child(
                    div()
                        .v_flex()
                        .flex_1()
                        .min_w_0()
                        .gap(px(1.))
                        .child(
                            div()
                                .truncate()
                                .text_sm()
                                .text_color(k.text)
                                .child(paper.name.clone()),
                        )
                        .child(
                            div()
                                .truncate()
                                .text_xs()
                                .text_color(k.text_faint)
                                .child(cloud_line(paper, now)),
                        ),
                )
                .child(oyl_ui::chip(paper.engine.label(), cx))
                .child(
                    Button::new(("library-cloud-open", index))
                        .ghost()
                        .xsmall()
                        .label(if downloading {
                            "Downloading…"
                        } else {
                            "Open"
                        })
                        .loading(downloading)
                        .disabled(cloud.downloading.is_some())
                        .tooltip("Download it into a folder you pick, and open it")
                        .on_click(cx.listener(move |_, _, _, cx| {
                            cx.emit(LibraryEvent::OpenFromCloud(id.clone()))
                        })),
                )
        });
        Some(
            div()
                .v_flex()
                .gap(px(4.))
                .child(oyl_ui::eyebrow("Oyl Cloud", cx))
                .child(
                    div()
                        .id("library-cloud")
                        .v_flex()
                        .gap(px(1.))
                        .max_h(px(180.))
                        .overflow_y_scroll()
                        .children(rows),
                )
                .into_any_element(),
        )
    }

    fn render_setup(&self, k: &Tokens, cx: &mut Context<Self>) -> AnyElement {
        let account = self.data.cloud.account.clone();
        div()
            .h_flex()
            .gap(px(8.))
            .pt(px(10.))
            .border_t_1()
            .border_color(k.hairline)
            .text_xs()
            .text_color(k.text_faint)
            .child(div().flex_1().min_w_0().child(self.data.setup.clone()))
            .map(|this| match account {
                Some(name) => this.child(
                    div()
                        .flex_none()
                        .max_w(px(220.))
                        .truncate()
                        .child(format!("Oyl Cloud: {name}")),
                ),
                None => this.child(
                    Button::new("library-cloud-sign-in")
                        .ghost()
                        .xsmall()
                        .label("Sign in to Oyl Cloud")
                        .tooltip("Back up papers and open them on your other Macs and the web")
                        .on_click(cx.listener(|_, _, _, cx| cx.emit(LibraryEvent::SignInToCloud))),
                ),
            })
            .when(self.data.can_install_latex, |this| {
                this.child(
                    Button::new("library-install-latex")
                        .ghost()
                        .xsmall()
                        .label("Install LaTeX")
                        .tooltip("Downloads tectonic, a self-contained LaTeX (about 60 MB)")
                        .on_click(cx.listener(|_, _, _, cx| cx.emit(LibraryEvent::InstallLatex))),
                )
            })
            .when(self.data.installing_latex, |this| {
                this.child(
                    Button::new("library-installing")
                        .ghost()
                        .xsmall()
                        .loading(true)
                        .disabled(true)
                        .label("Installing…"),
                )
            })
            .into_any_element()
    }
}

impl Render for LibraryView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let k = cx.tokens().clone();
        let header = self.render_header(&k);
        let actions = self.render_actions(cx);
        let form = self.form_open.then(|| self.render_form(&k, cx));
        let entries = self.render_entries(&k, cx);
        let cloud = self.render_cloud(&k, cx);
        let setup = self.render_setup(&k, cx);
        div()
            .id("oyl-library")
            .track_focus(&self.focus_handle)
            .v_flex()
            .size_full()
            .min_h_0()
            .gap(px(16.))
            .p(px(24.))
            .text_color(k.text)
            .child(header)
            .child(actions)
            .children(form)
            .child(entries)
            .children(cloud)
            .child(setup)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(name: &str, path: &str) -> LibraryEntry {
        LibraryEntry {
            path: path.into(),
            name: name.into(),
            engine: Engine::Typst,
            last_opened: None,
            missing: false,
        }
    }

    #[test]
    fn the_filter_finds_by_name_and_folder() {
        let entries = vec![
            entry("Owls at night", "/p/owls"),
            entry("Attention, briefly", "/p/attention"),
            entry("Thesis", "/p/phd-thesis"),
        ];
        let names = |filter: &str| -> Vec<String> {
            filter_entries(&entries, filter)
                .into_iter()
                .map(|e| e.name.clone())
                .collect()
        };
        assert_eq!(names("").len(), 3);
        assert_eq!(names("owl"), vec!["Owls at night"]);
        assert_eq!(names("phd"), vec!["Thesis"]);
        assert!(names("zzz").is_empty());
    }

    #[test]
    fn cloud_papers_say_when_and_where_they_changed() {
        let now = Utc::now();
        let mut paper = CloudPaper {
            id: "paper_sync:k1".into(),
            name: "Owls".into(),
            engine: Engine::Typst,
            updated: Some(now - chrono::TimeDelta::seconds(5)),
            from: Some("MacBook Pro".into()),
        };
        assert_eq!(cloud_line(&paper, now), "Changed just now on MacBook Pro");
        paper.from = None;
        assert_eq!(cloud_line(&paper, now), "Changed just now");
        paper.updated = None;
        assert_eq!(cloud_line(&paper, now), "In Oyl Cloud");
    }

    #[test]
    fn home_paths_are_shortened() {
        if let Some(home) = dirs::home_dir() {
            assert_eq!(short_path(&home.join("Papers/owls")), "~/Papers/owls");
            assert_eq!(short_path(&home), "~");
        }
        assert_eq!(short_path(Path::new("/opt/x")), "/opt/x");
    }
}
