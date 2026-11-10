//! The preview pane alone on a frosted window, showing a fake 40-page A4
//! paper drawn procedurally (title block, grey text lines, figure boxes,
//! page numbers) with an artificial 30–80 ms render delay.
//!
//! Keys: R recompile (changes the current page) · A add a page · X remove
//! the last page · E a failed compile · J reveal a spot (with the pulse) ·
//! T light/dark. In the pane: ⌘+ ⌘− ⌘0 ⌥⌘0, ⌘-scroll and pinch to zoom,
//! click a page to "jump to source" (printed to stderr).
//!
//! `OYL_SMOKE=1` opens the window without taking focus, runs a short
//! scripted tour (recompile one page, change the page count, zoom, switch
//! theme, reveal), prints what happened, and quits after about 3 seconds.

use std::cell::Cell;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use anyhow::anyhow;
use gpui_kit::*;
use oyl_model::SourceLocation;
use oyl_model::typeset::{PageImage, PagePosition, RenderedDocument};
use oyl_preview::{PreviewEvent, PreviewPane, PreviewStatus};
use oyl_ui::prelude::*;

actions!(
    preview_example,
    [
        Recompile,
        AddPage,
        RemovePage,
        FailCompile,
        RevealSomewhere,
        ToggleTheme
    ]
);

const A4: (f32, f32) = (595.28, 841.89);
const MARGIN: f32 = 72.;
const LINE: f32 = 14.;
const SOURCE: &str = "/example/paper/main.typ";
/// Source lines per page, for the fake click-to-source mapping.
const LINES_PER_PAGE: u32 = 60;

// -- The fake document ----------------------------------------------------

#[derive(Clone, Copy, Debug, Hash)]
struct FakePage {
    seed: u64,
    revision: u32,
}

struct FakeDocument {
    pages: Vec<FakePage>,
    renders: Arc<AtomicUsize>,
}

impl RenderedDocument for FakeDocument {
    fn page_count(&self) -> usize {
        self.pages.len()
    }

    fn page_size_pt(&self, _index: usize) -> (f32, f32) {
        A4
    }

    fn render_page(&self, index: usize, pixels_per_pt: f32) -> anyhow::Result<PageImage> {
        let page = *self
            .pages
            .get(index)
            .ok_or_else(|| anyhow!("page {index} does not exist"))?;
        let mut rng = Rng::new(page.seed ^ u64::from(page.revision).rotate_left(17));
        std::thread::sleep(Duration::from_millis(30 + rng.below(51)));
        self.renders.fetch_add(1, Ordering::SeqCst);
        Ok(draw_page(index, page, self.pages.len(), pixels_per_pt))
    }

    fn pdf(&self) -> anyhow::Result<Arc<Vec<u8>>> {
        Err(anyhow!("the example's fake document cannot be exported"))
    }

    fn page_fingerprint(&self, index: usize) -> u64 {
        let mut hasher = DefaultHasher::new();
        self.pages
            .get(index)
            .map(|page| (page.seed, page.revision))
            .hash(&mut hasher);
        hasher.finish()
    }

    fn source_at(&self, position: PagePosition) -> Option<SourceLocation> {
        let row = ((position.y_pt - MARGIN).max(0.) / LINE) as u32;
        let column = 1 + ((position.x_pt - MARGIN).max(0.) / 5.5) as u32;
        let line = position.page as u32 * LINES_PER_PAGE + row.min(LINES_PER_PAGE - 1) + 1;
        Some(SourceLocation::new(SOURCE, line, column))
    }

    fn position_of(&self, location: &SourceLocation) -> Option<PagePosition> {
        let index = location.line.saturating_sub(1);
        let page = (index / LINES_PER_PAGE) as usize;
        (page < self.pages.len()).then(|| PagePosition {
            page,
            x_pt: MARGIN + location.column.saturating_sub(1) as f32 * 5.5,
            y_pt: MARGIN + (index % LINES_PER_PAGE) as f32 * LINE + LINE * 0.8,
        })
    }
}

/// A tiny deterministic random source (SplitMix64).
struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        Self(seed)
    }

    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    fn below(&mut self, n: u64) -> u64 {
        self.next() % n.max(1)
    }

    fn unit(&mut self) -> f32 {
        (self.next() >> 40) as f32 / (1u64 << 24) as f32
    }
}

/// An opaque RGBA canvas addressed in points.
struct Canvas {
    width: u32,
    height: u32,
    scale: f32,
    rgba: Vec<u8>,
}

impl Canvas {
    fn new(pixels_per_pt: f32) -> Self {
        let width = (A4.0 * pixels_per_pt).ceil().max(1.) as u32;
        let height = (A4.1 * pixels_per_pt).ceil().max(1.) as u32;
        Self {
            width,
            height,
            scale: pixels_per_pt,
            rgba: vec![255; width as usize * height as usize * 4],
        }
    }

    fn rect(&mut self, x: f32, y: f32, w: f32, h: f32, gray: u8) {
        let x0 = ((x * self.scale).round().max(0.) as u32).min(self.width);
        let y0 = ((y * self.scale).round().max(0.) as u32).min(self.height);
        let x1 = (((x + w) * self.scale).round().max(0.) as u32).min(self.width);
        let y1 = (((y + h) * self.scale).round().max(0.) as u32).min(self.height);
        for row in y0..y1 {
            let start = (row as usize * self.width as usize + x0 as usize) * 4;
            let end = (row as usize * self.width as usize + x1 as usize) * 4;
            for pixel in self.rgba[start..end].as_chunks_mut::<4>().0 {
                *pixel = [gray, gray, gray, 255];
            }
        }
    }

    fn frame(&mut self, x: f32, y: f32, w: f32, h: f32, gray: u8) {
        let t = 0.8;
        self.rect(x, y, w, t, gray);
        self.rect(x, y + h - t, w, t, gray);
        self.rect(x, y, t, h, gray);
        self.rect(x + w - t, y, t, h, gray);
    }

    /// Draws a number with a 3×5 pixel font, centered at `cx`.
    fn number(&mut self, value: usize, cx: f32, y: f32, cell: f32, gray: u8) {
        const DIGITS: [[u8; 5]; 10] = [
            [0b111, 0b101, 0b101, 0b101, 0b111],
            [0b010, 0b110, 0b010, 0b010, 0b111],
            [0b111, 0b001, 0b111, 0b100, 0b111],
            [0b111, 0b001, 0b111, 0b001, 0b111],
            [0b101, 0b101, 0b111, 0b001, 0b001],
            [0b111, 0b100, 0b111, 0b001, 0b111],
            [0b111, 0b100, 0b111, 0b101, 0b111],
            [0b111, 0b001, 0b010, 0b010, 0b010],
            [0b111, 0b101, 0b111, 0b101, 0b111],
            [0b111, 0b101, 0b111, 0b001, 0b111],
        ];
        let text = value.to_string();
        let advance = cell * 4.;
        let mut x = cx - advance * text.len() as f32 / 2.;
        for digit in text.bytes().map(|b| usize::from(b - b'0')) {
            for (row, bits) in DIGITS[digit].iter().enumerate() {
                for col in 0..3 {
                    if bits & (0b100 >> col) != 0 {
                        self.rect(
                            x + col as f32 * cell,
                            y + row as f32 * cell,
                            cell,
                            cell,
                            gray,
                        );
                    }
                }
            }
            x += advance;
        }
    }
}

fn draw_page(index: usize, page: FakePage, count: usize, pixels_per_pt: f32) -> PageImage {
    let mut canvas = Canvas::new(pixels_per_pt);
    let mut rng = Rng::new(page.seed);
    let text_w = A4.0 - 2. * MARGIN;
    let bottom = A4.1 - MARGIN - 24.;
    let mut y = MARGIN;

    if index == 0 {
        // Title block.
        canvas.rect(A4.0 / 2. - 150., y + 20., 300., 16., 40);
        canvas.rect(A4.0 / 2. - 110., y + 44., 220., 16., 40);
        canvas.rect(A4.0 / 2. - 80., y + 76., 160., 7., 110);
        canvas.rect(A4.0 / 2. - 60., y + 90., 120., 6., 150);
        y += 130.;
        for _ in 0..7 {
            let w = if rng.below(5) == 0 {
                text_w * 0.55
            } else {
                text_w - 60.
            };
            canvas.rect(MARGIN + 30., y, w, 5.5, 140);
            y += LINE - 2.;
        }
        y += 18.;
    }

    // The revision mark makes a recompiled page visibly different.
    for r in 0..page.revision.min(12) {
        canvas.rect(A4.0 - MARGIN - 6. - r as f32 * 9., MARGIN - 30., 6., 6., 90);
    }

    let has_figure = index % 3 == 1;
    let figure_at = MARGIN + 180. + rng.unit() * 200.;
    let mut figure_done = !has_figure;
    while y < bottom {
        if !figure_done && y >= figure_at && y + 210. < bottom {
            canvas.rect(MARGIN + 20., y, text_w - 40., 170., 238);
            canvas.frame(MARGIN + 20., y, text_w - 40., 170., 150);
            let bars = 3 + rng.below(5);
            for bar in 0..bars {
                let h = 30. + rng.unit() * 110.;
                let x = MARGIN + 50. + bar as f32 * ((text_w - 100.) / bars as f32);
                canvas.rect(x, y + 160. - h, 22., h, 175);
            }
            canvas.rect(MARGIN + 60., y + 182., text_w - 120., 5.5, 120);
            y += 210.;
            figure_done = true;
            continue;
        }
        if rng.below(9) == 0 {
            // A section heading.
            y += 6.;
            canvas.rect(MARGIN, y, 90. + rng.unit() * 120., 8., 50);
            y += LINE + 8.;
            continue;
        }
        let lines = 3 + rng.below(6);
        for line in 0..lines {
            if y >= bottom {
                break;
            }
            let indent = if line == 0 { 14. } else { 0. };
            let w = if line + 1 == lines {
                text_w * (0.25 + rng.unit() * 0.6)
            } else {
                text_w - indent - rng.unit() * 6.
            };
            canvas.rect(MARGIN + indent, y, w, 5.5, 155);
            y += LINE;
        }
        y += 7.;
    }

    canvas.number(index + 1, A4.0 / 2., A4.1 - MARGIN + 12., 1.6, 110);
    // A running head: a short title on the left, the page count on the right.
    if index > 0 {
        canvas.rect(MARGIN, MARGIN - 24., 120., 5., 170);
        canvas.number(count, A4.0 - MARGIN - 8., MARGIN - 25., 1.2, 170);
    }
    canvas.rect(MARGIN, MARGIN - 14., text_w, 0.5, 200);

    PageImage {
        width: canvas.width,
        height: canvas.height,
        rgba: canvas.rgba,
    }
}

// -- The example window -----------------------------------------------------

struct Demo {
    preview: Entity<PreviewPane>,
    pages: Vec<FakePage>,
    next_seed: u64,
    renders: Arc<AtomicUsize>,
    compile_delay: Duration,
    compile: Option<Task<()>>,
    /// Frames drawn, counted by a zero-size canvas (for the smoke tour).
    frames: Rc<Cell<usize>>,
    _subscription: Subscription,
}

impl Demo {
    fn new(smoke: bool, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let preview = cx.new(|cx| PreviewPane::new(window, cx));
        let subscription = cx.subscribe(&preview, |_, _, event: &PreviewEvent, _| match event {
            PreviewEvent::JumpToSource(location) => eprintln!(
                "jump to source: {}:{}:{}",
                location.file.display(),
                location.line,
                location.column
            ),
            PreviewEvent::ShowProblems => eprintln!("show problems"),
            PreviewEvent::ZoomChanged(mode) => eprintln!("zoom changed: {mode:?}"),
        });
        window.focus(&preview.focus_handle(cx), cx);
        let mut demo = Self {
            preview,
            pages: (0..40)
                .map(|i| FakePage {
                    seed: 0x5EED_0000 + i,
                    revision: 0,
                })
                .collect(),
            next_seed: 0x5EED_0000 + 40,
            renders: Arc::new(AtomicUsize::new(0)),
            compile_delay: if smoke {
                Duration::from_millis(60)
            } else {
                Duration::from_millis(450)
            },
            compile: None,
            frames: Rc::new(Cell::new(0)),
            _subscription: subscription,
        };
        demo.publish(cx);
        demo
    }

    fn document(&self) -> Arc<dyn RenderedDocument> {
        Arc::new(FakeDocument {
            pages: self.pages.clone(),
            renders: self.renders.clone(),
        })
    }

    fn publish(&mut self, cx: &mut Context<Self>) {
        let document = self.document();
        self.preview.update(cx, |preview, cx| {
            preview.set_document(Some(document), cx);
            preview.set_status(PreviewStatus::Idle, cx);
        });
    }

    /// Simulates a compile: the status says so for a moment, then the new
    /// document (or the failure) arrives.
    fn compile(&mut self, fail: bool, cx: &mut Context<Self>) {
        self.preview.update(cx, |preview, cx| {
            preview.set_status(PreviewStatus::Compiling, cx)
        });
        let delay = self.compile_delay;
        self.compile = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(delay).await;
            this.update(cx, |demo, cx| {
                if fail {
                    demo.preview.update(cx, |preview, cx| {
                        preview.set_document(None, cx);
                        preview.set_status(PreviewStatus::Failed { errors: 2 }, cx);
                    });
                } else {
                    demo.publish(cx);
                }
            })
            .ok();
        }));
    }

    fn recompile(&mut self, cx: &mut Context<Self>) {
        let page = self.preview.read(cx).current_page();
        if let Some(page) = self.pages.get_mut(page) {
            page.revision += 1;
        }
        self.compile(false, cx);
    }

    fn add_page(&mut self, cx: &mut Context<Self>) {
        self.pages.push(FakePage {
            seed: self.next_seed,
            revision: 0,
        });
        self.next_seed += 1;
        self.compile(false, cx);
    }

    fn remove_page(&mut self, cx: &mut Context<Self>) {
        if self.pages.len() > 1 {
            self.pages.pop();
        }
        self.compile(false, cx);
    }

    fn reveal_somewhere(&mut self, cx: &mut Context<Self>) {
        let count = self.pages.len().max(1);
        let page = (self.next_seed as usize * 7 + self.renders.load(Ordering::SeqCst)) % count;
        let position = PagePosition {
            page,
            x_pt: MARGIN + 40.,
            y_pt: MARGIN + 260.,
        };
        self.preview
            .update(cx, |preview, cx| preview.scroll_to(position, cx));
        self.next_seed += 1;
    }

    fn renders(&self) -> usize {
        self.renders.load(Ordering::SeqCst)
    }
}

impl Render for Demo {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let k = cx.tokens().clone();
        let frames = self.frames.clone();
        div()
            .size_full()
            .relative()
            .key_context("PreviewExample")
            .font_family(k.ui_font.clone())
            .text_color(k.text)
            .on_action(cx.listener(|this, _: &Recompile, _, cx| this.recompile(cx)))
            .on_action(cx.listener(|this, _: &AddPage, _, cx| this.add_page(cx)))
            .on_action(cx.listener(|this, _: &RemovePage, _, cx| this.remove_page(cx)))
            .on_action(cx.listener(|this, _: &FailCompile, _, cx| this.compile(true, cx)))
            .on_action(cx.listener(|this, _: &RevealSomewhere, _, cx| this.reveal_somewhere(cx)))
            .on_action(cx.listener(|_, _: &ToggleTheme, _, cx| {
                oyl_ui::toggle(cx);
            }))
            .child(self.preview.clone())
            .child(canvas(move |_, _, _| frames.set(frames.get() + 1), |_, _, _, _| {}).size_0())
            .child(
                div()
                    .absolute()
                    .top(px(12.))
                    .left(px(84.))
                    .text_xs()
                    .text_color(k.text_faint)
                    .child(
                        "R recompile · A add page · X remove page · E fail · J reveal · T theme",
                    ),
            )
    }
}

fn main() {
    let smoke = std::env::var_os("OYL_SMOKE").is_some_and(|v| v == "1");
    gpui_kit::application()
        .with_assets(gpui_kit::assets::Assets)
        .run(move |cx| {
            gpui_kit::init(cx);
            oyl_ui::install(Appearance::System, cx);
            let context = Some("PreviewExample");
            cx.bind_keys([
                KeyBinding::new("r", Recompile, context),
                KeyBinding::new("a", AddPage, context),
                KeyBinding::new("x", RemovePage, context),
                KeyBinding::new("e", FailCompile, context),
                KeyBinding::new("j", RevealSomewhere, context),
                KeyBinding::new("t", ToggleTheme, context),
            ]);
            let options = WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
                    None,
                    size(px(860.), px(980.)),
                    cx,
                ))),
                titlebar: Some(TitlebarOptions {
                    title: Some("Oyl preview".into()),
                    appears_transparent: true,
                    traffic_light_position: Some(point(px(18.), px(18.))),
                }),
                window_background: WindowBackgroundAppearance::Blurred,
                focus: !smoke,
                // An unfocused normal window opens behind the active app,
                // and an occluded window draws no frames. Floating keeps
                // the smoke window visible (and painting) without taking
                // focus.
                kind: if smoke {
                    WindowKind::Floating
                } else {
                    WindowKind::Normal
                },
                ..Default::default()
            };
            let opened = gpui_kit::open_window(options, cx, |window, cx| {
                cx.new(|cx| Demo::new(smoke, window, cx))
            });
            let (handle, demo) = match opened {
                Ok(opened) => opened,
                Err(error) => {
                    eprintln!("could not open the preview window: {error:#}");
                    cx.quit();
                    return;
                }
            };
            if smoke {
                run_smoke_tour(handle, demo, cx);
            } else {
                cx.activate(true);
            }
        });
}

/// A scripted tour for unattended runs; prints what it observes.
fn run_smoke_tour(handle: AnyWindowHandle, demo: Entity<Demo>, cx: &mut App) {
    cx.spawn(async move |cx| {
        let wait = async |cx: &mut AsyncApp, millis: u64| {
            cx.background_executor()
                .timer(Duration::from_millis(millis))
                .await;
        };

        wait(cx, 700).await;
        let visible = handle.update(cx, |_, window, _| window.is_visible()).unwrap_or(false);
        eprintln!(
            "smoke: window visible: {visible}, {} frames drawn",
            demo.read_with(cx, |demo, _| demo.frames.get())
        );
        let before = demo.update(cx, |demo, cx| {
            let preview = demo.preview.read(cx);
            eprintln!(
                "smoke: {} pages, page {} in view, zoom {:.0}%, {} rasterized so far, {:.1} MB held",
                preview.page_count(),
                preview.current_page() + 1,
                preview.zoom() * 100.,
                demo.renders(),
                preview.memory_used() as f64 / 1_048_576.,
            );
            let renders = demo.renders();
            demo.recompile(cx);
            renders
        });

        wait(cx, 500).await;
        demo.update(cx, |demo, cx| {
            eprintln!(
                "smoke: recompile of one changed page rasterized {} page(s)",
                demo.renders() - before
            );
            demo.add_page(cx);
        });

        wait(cx, 300).await;
        demo.update(cx, |demo, cx| {
            eprintln!("smoke: after adding a page: {} pages", demo.preview.read(cx).page_count());
            demo.remove_page(cx);
            demo.remove_page(cx);
        });

        wait(cx, 300).await;
        demo.update(cx, |demo, cx| {
            eprintln!("smoke: after removing two: {} pages", demo.preview.read(cx).page_count());
            demo.preview.update(cx, |preview, cx| preview.zoom_in(cx));
            demo.compile(true, cx);
        });

        wait(cx, 400).await;
        demo.update(cx, |demo, cx| {
            let preview = demo.preview.read(cx);
            eprintln!(
                "smoke: zoom {:.0}% ({:?}), status {:?}, {} pages still shown",
                preview.zoom() * 100.,
                preview.zoom_mode(),
                preview.status(),
                preview.page_count(),
            );
            oyl_ui::toggle(cx);
            demo.reveal_somewhere(cx);
        });

        wait(cx, 700).await;
        demo.update(cx, |demo, cx| {
            let preview = demo.preview.read(cx);
            eprintln!(
                "smoke: page {} in view after reveal, {} rasterized in total, {:.1} MB held, {} frames drawn",
                preview.current_page() + 1,
                demo.renders(),
                preview.memory_used() as f64 / 1_048_576.,
                demo.frames.get(),
            );
            cx.quit();
        });
    })
    .detach();
}
