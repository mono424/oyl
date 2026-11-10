//! The figures panel: the images in `figures/`.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::time::SystemTime;

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::InputState;
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::{Icon, IconName, Sizable as _, StyledExt as _};
use gpui_kit::prelude::*;
use gpui_kit::{
    AnyElement, App, Context, Entity, EventEmitter, ExternalPaths, FocusHandle, Focusable,
    FontWeight, ListAlignment, ListState, ObjectFit, PathPromptOptions, Pixels, RenderImage,
    Subscription, Window, div, img, list, px,
};
use oyl_model::Engine;
use oyl_sources::Figure;
use oyl_sources::figures;
use oyl_ui::prelude::*;

use crate::model::figures::{
    THUMBNAIL_SIDE, derived_label, display_name, extension, figure_block, has_thumbnail,
    is_importable, size_label,
};
use crate::model::{file_name, plural};
use crate::thumbnail::thumbnail;
use crate::ui::{self, PAD_X, Tone};
use crate::{
    Confirm, Dismiss, FIGURES_KEY_CONTEXT, KEY_CONTEXT, SelectLeft, SelectNext, SelectPrev,
    SelectRight,
};

/// What the figures panel tells the shell.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FiguresEvent {
    /// Insert this figure block at the cursor.
    Insert {
        /// The block, written for the paper's engine.
        text: String,
    },
    /// Images were added to `figures/`.
    Changed,
    /// The panel asks to be closed (Esc, or its close button).
    Close,
}

/// A thumbnail, for the file as it was when it was made.
pub(crate) struct Thumb {
    stamp: Stamp,
    /// `None` when the file could not be decoded.
    pub(crate) image: Option<Arc<RenderImage>>,
}

/// What identifies a file's content cheaply: its size and modified time.
type Stamp = (u64, Option<SystemTime>);

/// Inserting one figure: its caption and label.
struct Composer {
    path: PathBuf,
    caption: Entity<InputState>,
    label: Entity<InputState>,
}

/// The images in the paper's `figures/` folder, two to a row, each with
/// its name, size and whether the paper uses it. Clicking one (or Enter)
/// opens a small form — a caption, a label made from the file name — that
/// inserts a figure block. Images are added with "Add image…" or by
/// dropping them on the panel.
///
/// Feed it the text of the paper's source files with
/// [`refresh`](Self::refresh) (to tell which images are used); subscribe to
/// [`FiguresEvent`]s.
pub struct FiguresPanel {
    root: PathBuf,
    engine: Engine,
    focus: FocusHandle,
    texts: Arc<Vec<String>>,
    figures: Rc<Vec<Figure>>,
    stamps: HashMap<PathBuf, Stamp>,
    loaded: bool,
    error: Option<String>,
    pub(crate) thumbs: HashMap<PathBuf, Thumb>,
    pending_thumbs: HashSet<PathBuf>,
    /// Thumbnails no longer shown, whose GPU textures go at the next frame.
    retired: Vec<Arc<RenderImage>>,
    selected: Option<PathBuf>,
    composer: Option<Composer>,
    importing: usize,
    notice: Option<(Tone, String)>,
    list: ListState,
    load_generation: u64,
    _subscriptions: Vec<Subscription>,
}

impl FiguresPanel {
    /// The figures of the paper at `root`, listed in the background.
    /// Figure blocks are written for `engine`.
    pub fn new(root: PathBuf, engine: Engine, _window: &mut Window, cx: &mut Context<Self>) -> Self {
        crate::init(cx);
        let subscriptions = vec![
            cx.observe_global::<oyl_ui::OylTheme>(|_, cx| cx.notify()),
            cx.on_release(|panel: &mut Self, cx: &mut App| {
                for image in panel.retired.drain(..) {
                    cx.drop_image(image, None);
                }
                for (_, thumb) in panel.thumbs.drain() {
                    if let Some(image) = thumb.image {
                        cx.drop_image(image, None);
                    }
                }
            }),
        ];
        let mut panel = FiguresPanel {
            root,
            engine,
            focus: cx.focus_handle(),
            texts: Arc::new(Vec::new()),
            figures: Rc::new(Vec::new()),
            stamps: HashMap::new(),
            loaded: false,
            error: None,
            thumbs: HashMap::new(),
            pending_thumbs: HashSet::new(),
            retired: Vec::new(),
            selected: None,
            composer: None,
            importing: 0,
            notice: None,
            list: ListState::new(0, ListAlignment::Top, px(400.)),
            load_generation: 0,
            _subscriptions: subscriptions,
        };
        panel.reload(cx);
        panel
    }

    /// The width the card should have.
    pub fn preferred_width(&self) -> Pixels {
        px(420.)
    }

    /// Lists the figures again; `source_texts` (the text of every source
    /// file) tells which images the paper uses.
    pub fn refresh(&mut self, source_texts: Vec<String>, cx: &mut Context<Self>) {
        self.texts = Arc::new(source_texts);
        self.reload(cx);
    }

    /// The figures shown, in path order.
    pub fn figures(&self) -> &[Figure] {
        &self.figures
    }

    /// The path of the selected figure.
    pub fn selected_path(&self) -> Option<&Path> {
        self.selected.as_deref()
    }

    /// Whether the insert form is open.
    pub fn is_composing(&self) -> bool {
        self.composer.is_some()
    }

    fn reload(&mut self, cx: &mut Context<Self>) {
        self.load_generation += 1;
        let generation = self.load_generation;
        let root = self.root.clone();
        let texts = self.texts.clone();
        let task = cx.background_spawn(async move {
            let figures = figures::list(&root, &texts).map_err(|e| e.to_string())?;
            let stamps: HashMap<PathBuf, Stamp> = figures
                .iter()
                .map(|f| {
                    let modified = std::fs::metadata(&f.path).and_then(|m| m.modified()).ok();
                    (f.path.clone(), (f.bytes, modified))
                })
                .collect();
            Ok::<_, String>((figures, stamps))
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |this, cx| {
                if generation != this.load_generation {
                    return;
                }
                this.loaded = true;
                match result {
                    Ok((figures, stamps)) => {
                        this.error = None;
                        this.set_figures(figures, stamps, cx);
                    }
                    Err(message) => this.error = Some(message),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn set_figures(&mut self, figures: Vec<Figure>, stamps: HashMap<PathBuf, Stamp>, cx: &mut Context<Self>) {
        // Thumbnails of files that are gone or changed go.
        let stale: Vec<PathBuf> = self
            .thumbs
            .iter()
            .filter(|(path, thumb)| stamps.get(*path) != Some(&thumb.stamp))
            .map(|(path, _)| path.clone())
            .collect();
        for path in stale {
            if let Some(image) = self.thumbs.remove(&path).and_then(|t| t.image) {
                self.retired.push(image);
            }
        }
        let rows_before = self.list.item_count();
        self.figures = Rc::new(figures);
        self.stamps = stamps;
        if self
            .selected
            .as_ref()
            .is_some_and(|path| !self.figures.iter().any(|f| &f.path == path))
        {
            self.selected = None;
            self.composer = None;
        }
        let rows = self.figures.len().div_ceil(2);
        ui::sync_list(&self.list, rows, rows == rows_before);
        self.request_thumbnails(cx);
        cx.notify();
    }

    fn request_thumbnails(&mut self, cx: &mut Context<Self>) {
        let wanted: Vec<(PathBuf, Stamp)> = self
            .figures
            .iter()
            .filter(|f| has_thumbnail(&f.path))
            .filter(|f| !self.thumbs.contains_key(&f.path) && !self.pending_thumbs.contains(&f.path))
            .filter_map(|f| self.stamps.get(&f.path).map(|stamp| (f.path.clone(), *stamp)))
            .collect();
        for (path, stamp) in wanted {
            self.pending_thumbs.insert(path.clone());
            let source = path.clone();
            let task = cx.background_spawn(async move { thumbnail(&source, THUMBNAIL_SIDE) });
            cx.spawn(async move |this, cx| {
                let result = task.await;
                this.update(cx, |this, cx| {
                    this.pending_thumbs.remove(&path);
                    let image = match result {
                        Ok(image) => Some(image),
                        Err(error) => {
                            log::warn!("no thumbnail for {}: {error:#}", path.display());
                            None
                        }
                    };
                    if this.stamps.get(&path) == Some(&stamp) {
                        if let Some(old) = this
                            .thumbs
                            .insert(path, Thumb { stamp, image })
                            .and_then(|t| t.image)
                        {
                            this.retired.push(old);
                        }
                    } else if let Some(image) = image {
                        this.retired.push(image);
                    }
                    cx.notify();
                })
                .ok();
            })
            .detach();
        }
    }

    // -- Actions ------------------------------------------------------------

    fn index_of(&self, path: &Path) -> Option<usize> {
        self.figures.iter().position(|f| f.path == path)
    }

    fn select_index(&mut self, ix: usize, cx: &mut Context<Self>) {
        let Some(figure) = self.figures.get(ix) else {
            return;
        };
        if self.selected.as_ref() != Some(&figure.path) {
            self.composer = None;
        }
        self.selected = Some(figure.path.clone());
        self.list.scroll_to_reveal_item(ix / 2);
        cx.notify();
    }

    fn composer_focused(&self, window: &Window, cx: &App) -> bool {
        self.composer.as_ref().is_some_and(|c| {
            c.caption.focus_handle(cx).is_focused(window) || c.label.focus_handle(cx).is_focused(window)
        })
    }

    fn move_selection(&mut self, step: isize, window: &mut Window, cx: &mut Context<Self>) {
        if self.composer_focused(window, cx) || self.figures.is_empty() {
            return;
        }
        let last = self.figures.len() as isize - 1;
        let next = match self.selected.as_ref().and_then(|p| self.index_of(p)) {
            Some(ix) => (ix as isize + step).clamp(0, last),
            None if step > 0 => 0,
            None => last,
        };
        self.select_index(next as usize, cx);
    }

    fn open_composer(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.select_index(ix, cx);
        let Some(figure) = self.figures.get(ix).cloned() else {
            return;
        };
        if self.composer.as_ref().is_some_and(|c| c.path == figure.path) {
            return;
        }
        let label = derived_label(self.engine, &figure.rel_path);
        let caption = cx.new(|cx| InputState::new(window, cx).placeholder("What the figure shows"));
        let label = cx.new(|cx| InputState::new(window, cx).default_value(label));
        let focus = caption.focus_handle(cx);
        self.composer = Some(Composer {
            path: figure.path,
            caption,
            label,
        });
        window.focus(&focus, cx);
        cx.notify();
    }

    fn close_composer(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.composer = None;
        window.focus(&self.focus, cx);
        cx.notify();
    }

    fn insert(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(composer) = &self.composer else {
            return;
        };
        let Some(figure) = self.figures.iter().find(|f| f.path == composer.path) else {
            return;
        };
        let caption = composer.caption.read(cx).value().to_string();
        let label = composer.label.read(cx).value().to_string();
        let text = figure_block(self.engine, &figure.rel_path, &caption, &label);
        cx.emit(FiguresEvent::Insert { text });
        self.close_composer(window, cx);
    }

    fn confirm(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.composer.is_some() {
            self.insert(window, cx);
        } else if let Some(ix) = self.selected.as_ref().and_then(|p| self.index_of(p)) {
            self.open_composer(ix, window, cx);
        } else if !self.figures.is_empty() {
            self.select_index(0, cx);
        }
    }

    fn dismiss(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.composer.is_some() {
            self.close_composer(window, cx);
        } else {
            cx.emit(FiguresEvent::Close);
        }
    }

    fn pick_images(&mut self, cx: &mut Context<Self>) {
        let answer = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: true,
            prompt: Some("Add".into()),
        });
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(paths))) = answer.await else {
                return;
            };
            this.update(cx, |this, cx| this.import(paths, cx)).ok();
        })
        .detach();
    }

    /// Imports images into `figures/` (others are pointed out).
    fn import(&mut self, paths: Vec<PathBuf>, cx: &mut Context<Self>) {
        let (images, others): (Vec<PathBuf>, Vec<PathBuf>) =
            paths.into_iter().partition(|p| is_importable(p));
        self.notice = others.first().map(|other| {
            (
                Tone::Quiet,
                format!(
                    "{} is not an image. Figures can be PNG, JPEG, PDF, SVG, EPS, HEIC, TIFF, GIF, WebP or BMP.",
                    file_name(other)
                ),
            )
        });
        if images.is_empty() {
            cx.notify();
            return;
        }
        self.importing += images.len();
        cx.notify();
        let root = self.root.clone();
        let count = images.len();
        let task = cx.background_spawn(async move {
            images
                .iter()
                .map(|path| {
                    figures::import_file(&root, path)
                        .map_err(|e| format!("{}: {e}", file_name(path)))
                })
                .collect::<Vec<_>>()
        });
        cx.spawn(async move |this, cx| {
            let results = task.await;
            this.update(cx, |this, cx| {
                this.importing = this.importing.saturating_sub(count);
                let mut added = Vec::new();
                let mut errors = Vec::new();
                for result in results {
                    match result {
                        Ok(figure) => added.push(figure),
                        Err(message) => errors.push(message),
                    }
                }
                if let Some(error) = errors.first() {
                    this.notice = Some((Tone::Error, error.clone()));
                } else if !added.is_empty() {
                    this.notice = Some((
                        Tone::Quiet,
                        format!("Added {}.", plural(added.len(), "image", "images")),
                    ));
                }
                if let Some(first) = added.first() {
                    this.selected = Some(first.path.clone());
                    this.composer = None;
                }
                if !added.is_empty() {
                    cx.emit(FiguresEvent::Changed);
                }
                this.reload(cx);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    // -- Rendering ----------------------------------------------------------

    fn render_card(&self, ix: usize, figure: &Figure, cx: &mut Context<Self>) -> AnyElement {
        let k = cx.tokens().clone();
        let selected = self.selected.as_ref() == Some(&figure.path);
        let hover = k.surface_hover;
        let thumb = self.thumbs.get(&figure.path);
        let preview: AnyElement = match thumb.and_then(|t| t.image.clone()) {
            Some(image) => div()
                .size_full()
                .relative()
                .bg(k.paper)
                .child(img(image).size_full().object_fit(ObjectFit::Contain))
                .child(div().absolute().inset_0().bg(k.paper_dim))
                .into_any_element(),
            None if self.pending_thumbs.contains(&figure.path) => div()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .child(Spinner::new().small().color(k.text_faint))
                .into_any_element(),
            None => div()
                .size_full()
                .v_flex()
                .items_center()
                .justify_center()
                .gap(px(4.))
                .child(Icon::new(IconName::File).text_color(k.text_faint))
                .child(
                    div()
                        .text_size(px(11.))
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(k.text_faint)
                        .child(extension(&figure.path).to_uppercase()),
                )
                .into_any_element(),
        };
        div()
            .id(("figure", ix))
            .v_flex()
            .flex_1()
            .min_w_0()
            .gap(px(5.))
            .p(px(6.))
            .rounded(k.radius_m)
            .cursor_pointer()
            .when(selected, |card| card.bg(k.accent_soft))
            .when(!selected, |card| card.hover(move |style| style.bg(hover)))
            .child(
                div()
                    .h(px(116.))
                    .w_full()
                    .rounded(k.radius_s)
                    .overflow_hidden()
                    .bg(k.surface)
                    .border_1()
                    .border_color(k.hairline)
                    .child(preview),
            )
            .child(
                div()
                    .px(px(2.))
                    .text_size(px(12.5))
                    .text_color(k.text)
                    .truncate()
                    .child(display_name(figure)),
            )
            .child(
                div()
                    .h_flex()
                    .px(px(2.))
                    .gap(px(6.))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_size(px(11.5))
                            .text_color(k.text_faint)
                            .child(size_label(figure)),
                    )
                    .when(!figure.used, |line| {
                        line.child(
                            oyl_ui::chip("Not used", cx)
                                .h(px(17.))
                                .text_size(px(10.5)),
                        )
                    }),
            )
            .on_click(cx.listener(move |this, _, window, cx| this.open_composer(ix, window, cx)))
            .into_any_element()
    }

    fn render_composer(&self, composer: &Composer, cx: &mut Context<Self>) -> AnyElement {
        let k = cx.tokens().clone();
        let Some(figure) = self.figures.iter().find(|f| f.path == composer.path) else {
            return div().into_any_element();
        };
        let label = composer.label.read(cx).value().to_string();
        let label = if label.trim().is_empty() {
            derived_label(self.engine, &figure.rel_path)
        } else {
            label.trim().to_string()
        };
        let reference = match self.engine {
            Engine::Typst => format!("Refer to it with @{label}."),
            Engine::Latex => format!("Refer to it with \\ref{{{label}}}."),
        };
        let path = figure.path.clone();
        div()
            .mt(px(6.))
            .mx(px(6.))
            .v_flex()
            .gap(px(10.))
            .p(px(12.))
            .rounded(k.radius_m)
            .bg(k.surface)
            .border_1()
            .border_color(k.hairline)
            .child(
                div()
                    .h_flex()
                    .gap(px(6.))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_size(px(12.5))
                            .font_weight(FontWeight::MEDIUM)
                            .child(format!("Insert {}", figure.rel_path)),
                    )
                    .child(
                        Button::new("figure-reveal")
                            .ghost()
                            .xsmall()
                            .icon(IconName::FolderOpen)
                            .tooltip("Show in Finder")
                            .on_click(cx.listener(move |_, _, _, cx| cx.reveal_path(&path))),
                    ),
            )
            .child(ui::field("Caption", &composer.caption, cx))
            .child(
                div()
                    .v_flex()
                    .gap(px(3.))
                    .child(ui::field("Label", &composer.label, cx))
                    .child(
                        div()
                            .text_size(px(11.5))
                            .text_color(k.text_faint)
                            .child(reference),
                    ),
            )
            .child(
                div()
                    .h_flex()
                    .justify_end()
                    .gap(px(6.))
                    .child(
                        Button::new("figure-cancel")
                            .ghost()
                            .small()
                            .label("Cancel")
                            .on_click(cx.listener(|this, _, window, cx| this.close_composer(window, cx))),
                    )
                    .child(
                        Button::new("figure-insert")
                            .primary()
                            .small()
                            .label("Insert figure")
                            .on_click(cx.listener(|this, _, window, cx| this.insert(window, cx))),
                    ),
            )
            .into_any_element()
    }

    fn render_row(&mut self, row: usize, _window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let figures = self.figures.clone();
        let first = row * 2;
        let Some(left) = figures.get(first) else {
            return div().into_any_element();
        };
        let right = figures.get(first + 1);
        let composer = self
            .composer
            .as_ref()
            .filter(|c| c.path == left.path || right.is_some_and(|r| r.path == c.path))
            .map(|c| self.render_composer(c, cx));
        let last = row + 1 == figures.len().div_ceil(2);
        div()
            .px(px(PAD_X - 6.))
            .pb(px(if last { 16. } else { 4. }))
            .child(
                div()
                    .h_flex()
                    .items_start()
                    .gap(px(6.))
                    .child(self.render_card(first, left, cx))
                    .child(match right {
                        Some(right) => self.render_card(first + 1, right, cx),
                        None => div().flex_1().into_any_element(),
                    }),
            )
            .children(composer)
            .into_any_element()
    }
}

impl Render for FiguresPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        for image in std::mem::take(&mut self.retired) {
            cx.drop_image(image, Some(window));
        }
        let k = cx.tokens().clone();
        let this = cx.entity().downgrade();
        let count = (!self.figures.is_empty()).then(|| self.figures.len().to_string());
        let add_button = Button::new("figures-add")
            .ghost()
            .xsmall()
            .icon(IconName::Plus)
            .label("Add image…")
            .loading(self.importing > 0)
            .on_click(cx.listener(|this, _, _, cx| this.pick_images(cx)))
            .into_any_element();
        let header = ui::header(
            "Figures",
            count,
            if self.figures.is_empty() {
                Vec::new()
            } else {
                vec![add_button]
            },
            Rc::new(move |_, cx| {
                this.update(cx, |_, cx| cx.emit(FiguresEvent::Close)).ok();
            }),
            cx,
        );
        let notice = self
            .notice
            .clone()
            .map(|(tone, text)| div().flex_none().px(px(PAD_X)).pb(px(8.)).child(ui::notice(tone, text, cx)));
        let body = if !self.loaded {
            div().flex_1().into_any_element()
        } else if let Some(error) = &self.error {
            div()
                .px(px(PAD_X))
                .child(ui::notice(Tone::Error, error.clone(), cx))
                .into_any_element()
        } else if self.figures.is_empty() {
            ui::empty_state(
                "No figures yet. Drop images here, or add one.",
                Some(
                    Button::new("figures-empty-add")
                        .small()
                        .label("Add image…")
                        .loading(self.importing > 0)
                        .on_click(cx.listener(|this, _, _, cx| this.pick_images(cx)))
                        .into_any_element(),
                ),
                cx,
            )
            .into_any_element()
        } else {
            div()
                .flex_1()
                .min_h_0()
                .relative()
                .child(list(self.list.clone(), cx.processor(Self::render_row)).size_full())
                .vertical_scrollbar(&self.list)
                .into_any_element()
        };
        let drop_bg = k.accent_soft;
        div()
            .id("oyl-figures")
            .key_context(format!("{KEY_CONTEXT} {FIGURES_KEY_CONTEXT}").as_str())
            .track_focus(&self.focus)
            .size_full()
            .v_flex()
            .rounded(k.radius_l)
            .font_family(k.ui_font.clone())
            .text_color(k.text)
            .on_action(cx.listener(|this, _: &SelectPrev, window, cx| this.move_selection(-2, window, cx)))
            .on_action(cx.listener(|this, _: &SelectNext, window, cx| this.move_selection(2, window, cx)))
            .on_action(cx.listener(|this, _: &SelectLeft, window, cx| this.move_selection(-1, window, cx)))
            .on_action(cx.listener(|this, _: &SelectRight, window, cx| this.move_selection(1, window, cx)))
            .on_action(cx.listener(|this, _: &Confirm, window, cx| this.confirm(window, cx)))
            .on_action(cx.listener(|this, _: &Dismiss, window, cx| this.dismiss(window, cx)))
            .drag_over::<ExternalPaths>(move |style, _, _, _| style.bg(drop_bg))
            .on_drop(cx.listener(|this, paths: &ExternalPaths, _, cx| {
                this.import(paths.paths().to_vec(), cx)
            }))
            .child(header)
            .children(notice)
            .child(body)
    }
}

impl Focusable for FiguresPanel {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl EventEmitter<FiguresEvent> for FiguresPanel {}
