//! The editor pane alone, in Oyl's frosted window.
//!
//! ```text
//! cargo run -p oyl-editor --example editor [FILE…]
//! ```
//!
//! Without files, a small sample paper is written to a temporary folder (a
//! Typst file, a LaTeX file with a second chapter file, and a bibliography)
//! and opened. Keys: ⌘1 zen, ⌘2 light/dark, ⌘3 next file; inside the editor
//! ⌘S saves, ⌘/ comments, ⌘F searches, ⌘B polishes the selection (or the
//! sentence at the cursor), ⇧⌘B opens a box to say what to do with it, ⌘I
//! checks its facts (or the paragraph's); Esc closes the box or calls off
//! what runs.
//! Citations of keys that are not in the sample bibliography are flagged
//! inline, live.
//!
//! The AI is a deterministic stand-in: after a short pause the polish fixes
//! "teh" and doubled spaces; ⇧⌘B understands "uppercase" (and gives the text
//! back unchanged for anything else); the fact check flags "1998" as
//! incorrect (suggesting 2014) and "spread quickly" as questionable. A
//! card's Rewrite takes the year from the remark ("use 2015"). The sample
//! paper's last paragraph has all of it.
//!
//! With `OYL_SMOKE=1` the window is not activated, the sample paper (never
//! files given on the command line) is opened and the pane is exercised
//! (typing with completion, autosave, outside edits with and without a merge,
//! comments, diagnostics, go-to, ⌘B and its undo, ⇧⌘B with an instruction,
//! ⌘I and its findings) and the example quits after about six seconds,
//! exiting non-zero if any check failed.

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use gpui_kit::prelude::*;
use gpui_kit::{
    AnyWindowHandle, App, AsyncApp, Context, Entity, KeyBinding, Keystroke, Modifiers, Render,
    Subscription, TitlebarOptions, Window, WindowBackgroundAppearance, WindowOptions, div, point,
    px,
};
use oyl_editor::{CustomPrompt, EditorEvent, EditorPane, FactCheck, Polish, ToggleLineComment};
use oyl_model::agent::{
    AgentEvent, FactCheckEvent, FactCheckHandle, FactCheckRequest, FactChecker, FactFinding,
    FindingLevel, RewriteKind, RewriteRequest, Rewriter, TurnHandle,
};
use oyl_model::{CiteCandidate, Diagnostic, SourceLocation};
use oyl_ui::prelude::*;

gpui_kit::actions!(
    editor_example,
    [
        /// Zen on and off.
        ToggleZen,
        /// Light and dark.
        ToggleAppearance,
        /// Show the next open file.
        NextFile
    ]
);

const CONTEXT: &str = "EditorExample";

fn main() {
    let smoke = std::env::var("OYL_SMOKE").is_ok_and(|value| value == "1");
    let args: Vec<PathBuf> = std::env::args()
        .skip(1)
        .map(|arg| std::path::absolute(&arg).unwrap_or_else(|_| PathBuf::from(arg)))
        .collect();

    gpui_kit::application()
        .with_assets(gpui_kit::assets::Assets)
        .run(move |cx| {
            gpui_kit::init(cx);
            oyl_ui::install(Appearance::System, cx);
            cx.bind_keys([
                KeyBinding::new("cmd-1", ToggleZen, Some(CONTEXT)),
                KeyBinding::new("cmd-2", ToggleAppearance, Some(CONTEXT)),
                KeyBinding::new("cmd-3", NextFile, Some(CONTEXT)),
            ]);

            let (files, sample_dir) = if args.is_empty() || smoke {
                match write_sample() {
                    Ok((files, dir)) => (files, Some(dir)),
                    Err(error) => {
                        eprintln!("could not write the sample paper: {error:#}");
                        std::process::exit(1);
                    }
                }
            } else {
                (args.clone(), None)
            };

            let options = WindowOptions {
                titlebar: Some(TitlebarOptions {
                    title: None,
                    appears_transparent: true,
                    traffic_light_position: Some(point(px(18.), px(18.))),
                }),
                window_background: WindowBackgroundAppearance::Blurred,
                focus: !smoke,
                ..Default::default()
            };
            // The stand-in AI answers quickly in the smoke run, at a pace
            // that shows the working state otherwise.
            let pause = Duration::from_millis(if smoke { 250 } else { 1200 });
            let opened = gpui_kit::open_window(options, cx, |window, cx| {
                cx.new(|cx| Demo::new(files.clone(), pause, window, cx))
            });
            let (handle, demo) = match opened {
                Ok(opened) => opened,
                Err(error) => {
                    eprintln!("could not open a window: {error:#}");
                    std::process::exit(1);
                }
            };

            if smoke {
                let pane = demo.read(cx).pane.clone();
                let log = demo.read(cx).log.clone();
                cx.spawn(async move |cx| {
                    let failures = run_smoke(handle, pane, log, files, cx).await;
                    if let Some(dir) = &sample_dir {
                        let _ = std::fs::remove_dir_all(dir);
                    }
                    if failures.is_empty() {
                        eprintln!("smoke: all checks passed");
                        cx.update(|cx| cx.quit());
                    } else {
                        for failure in &failures {
                            eprintln!("smoke: FAILED {failure}");
                        }
                        std::process::exit(1);
                    }
                })
                .detach();
            } else {
                cx.on_window_closed(move |cx, _| {
                    // The generated sample is not worth keeping.
                    if let Some(dir) = &sample_dir {
                        let _ = std::fs::remove_dir_all(dir);
                    }
                    cx.quit();
                })
                .detach();
                cx.activate(true);
            }
        });
}

/// The window's content: the pane, and a line of key hints.
struct Demo {
    pane: Entity<EditorPane>,
    files: Vec<PathBuf>,
    current: usize,
    log: Rc<RefCell<Vec<EditorEvent>>>,
    _subscriptions: Vec<Subscription>,
}

impl Demo {
    fn new(
        files: Vec<PathBuf>,
        pause: Duration,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let pane = cx.new(|cx| EditorPane::new(window, cx));
        let bibliography = files
            .iter()
            .find(|file| file.extension().is_some_and(|e| e == "bib"))
            .cloned();
        pane.update(cx, |pane, cx| {
            pane.set_citations(sample_citations(), cx);
            pane.set_rewriter(Some(Arc::new(SampleRewriter { pause })), cx);
            pane.set_fact_checker(Some(Arc::new(SampleChecker { pause })), cx);
            pane.set_bibliography_provider(Rc::new(move |_| {
                bibliography
                    .as_deref()
                    .and_then(|path| std::fs::read_to_string(path).ok())
                    .unwrap_or_default()
            }));
            for file in files.iter().rev() {
                if let Err(error) = pane.open(file, window, cx) {
                    eprintln!("cannot open {}: {error:#}", file.display());
                }
            }
        });
        let subscription = cx.subscribe_in(&pane, window, Self::pane_event);
        let mut demo = Self {
            pane,
            files,
            current: 0,
            log: Rc::default(),
            _subscriptions: vec![subscription],
        };
        demo.check_citations(window, cx);
        demo
    }

    fn pane_event(
        &mut self,
        _pane: &Entity<EditorPane>,
        event: &EditorEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.log.borrow_mut().push(event.clone());
        match event {
            EditorEvent::Edited { .. } => self.check_citations(window, cx),
            EditorEvent::PastedImage { extension, bytes } => {
                eprintln!("pasted a {extension} image ({} bytes)", bytes.len());
            }
            EditorEvent::DroppedFiles { paths } => eprintln!("dropped {paths:?}"),
            EditorEvent::SaveFailed { message, .. } => eprintln!("save failed: {message}"),
            EditorEvent::FactCheckFinished {
                findings, claims, ..
            } => eprintln!("fact check: {findings} of {claims} claims flagged"),
            _ => {}
        }
    }

    /// Flags citations of keys that are not in the sample bibliography, the
    /// way the app's compile loop would report them.
    fn check_citations(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let keys: Vec<String> = sample_citations().into_iter().map(|c| c.key).collect();
        let pane = self.pane.read(cx);
        let mut diagnostics = Vec::new();
        for path in pane.open_paths() {
            if let Some(text) = pane.text(&path, cx) {
                diagnostics.extend(unknown_citations(&path, &text, &keys));
            }
        }
        self.pane.update(cx, |pane, cx| {
            pane.set_diagnostics(&diagnostics, window, cx)
        });
    }

    fn toggle_zen(&mut self, _: &ToggleZen, _window: &mut Window, cx: &mut Context<Self>) {
        self.pane.update(cx, |pane, cx| {
            let zen = !pane.is_zen();
            pane.set_zen(zen, cx);
        });
    }

    fn toggle_appearance(&mut self, _: &ToggleAppearance, _: &mut Window, cx: &mut Context<Self>) {
        oyl_ui::toggle(cx);
    }

    fn next_file(&mut self, _: &NextFile, window: &mut Window, cx: &mut Context<Self>) {
        if self.files.is_empty() {
            return;
        }
        self.current = (self.current + 1) % self.files.len();
        let file = self.files[self.current].clone();
        self.pane.update(cx, |pane, cx| {
            if let Err(error) = pane.open(&file, window, cx) {
                eprintln!("cannot open {}: {error:#}", file.display());
            }
        });
    }
}

impl Render for Demo {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let tokens = cx.tokens().clone();
        div()
            .key_context(CONTEXT)
            .on_action(cx.listener(Self::toggle_zen))
            .on_action(cx.listener(Self::toggle_appearance))
            .on_action(cx.listener(Self::next_file))
            .size_full()
            .flex()
            .flex_col()
            // Room for the traffic lights, where the app has its island.
            .pt(px(40.))
            .child(div().flex_1().min_h_0().child(self.pane.clone()))
            .child(
                div()
                    .flex_none()
                    .flex()
                    .flex_row()
                    .justify_end()
                    .px(px(18.))
                    .pb(px(10.))
                    .font_family(tokens.ui_font.clone())
                    .text_xs()
                    .text_color(tokens.text_faint)
                    .child(
                        "⌘1 Zen   ⌘2 Light/Dark   ⌘3 Next file   \
                         ⌘B Polish   ⇧⌘B Ask AI   ⌘I Fact check",
                    ),
            )
    }
}

// -- The sample paper --------------------------------------------------------

const SAMPLE_TYPST: &str = r#"#set document(title: "Attention, briefly")
#set text(font: "Charter", size: 11pt)
#set heading(numbering: "1.")

= Introduction <intro>

Sequence models _used to_ be recurrent. The Transformer @vaswani2017 replaced
recurrence with *attention*, and large-scale pre-training @devlin2019 made it
the default. Residual connections @he2016 keep the deep stacks trainable; see
@fig:attention for the core operation.

$ "Attention"(Q, K, V) = "softmax"((Q K^T) / sqrt(d_k)) V $

#figure(
  rect(width: 60%, height: 3em),
  caption: [Scaled dot-product attention.],
) <fig:attention>

// This key is not in refs.bib, so it is flagged.
Some claims still need a source @missing2024.

Attention was first proposed in 1998, and teh idea  spread quickly.
"#;

/// The line of the sample Typst file with work for ⌘B, ⇧⌘B and ⌘I.
const AI_LINE: u32 = 22;

const SAMPLE_LATEX: &str = r#"\documentclass{article}
\usepackage{graphicx}

\title{Attention, briefly}
\begin{document}
\maketitle

% The introduction lives in its own file.
\input{intro}

\section{Method}\label{sec:method}
Self-attention~\cite{vaswani2017} weighs every token against every other;
pre-training~\citep[see][]{devlin2019} and residual stacks~\cite{he2016} did
the rest. Section~\ref{sec:intro} has the background.

\begin{equation}
  \mathrm{Attention}(Q, K, V) = \mathrm{softmax}\left(\frac{QK^\top}{\sqrt{d_k}}\right)V
\end{equation}

This key is not in the bibliography: \cite{missing2024}.

\end{document}
"#;

const SAMPLE_INTRO: &str = r#"\section{Introduction}\label{sec:intro}
Sequence models \emph{used to} be recurrent.
"#;

const SAMPLE_BIB: &str = r#"@inproceedings{vaswani2017,
  title     = {Attention Is All You Need},
  author    = {Vaswani, Ashish and Shazeer, Noam and Parmar, Niki},
  booktitle = {Advances in Neural Information Processing Systems},
  year      = {2017},
}

@inproceedings{devlin2019,
  title     = {{BERT}: Pre-training of Deep Bidirectional Transformers},
  author    = {Devlin, Jacob and Chang, Ming-Wei and Lee, Kenton},
  booktitle = {NAACL},
  year      = {2019},
}

@inproceedings{he2016,
  title     = {Deep Residual Learning for Image Recognition},
  author    = {He, Kaiming and Zhang, Xiangyu and Ren, Shaoqing and Sun, Jian},
  booktitle = {CVPR},
  year      = {2016},
}
"#;

/// Writes the sample paper into a fresh temporary folder. Returns its files
/// (Typst first) and the folder.
fn write_sample() -> anyhow::Result<(Vec<PathBuf>, PathBuf)> {
    let dir = tempfile::Builder::new()
        .prefix("oyl-editor-sample-")
        .tempdir()?
        .keep();
    let files = [
        ("main.typ", SAMPLE_TYPST),
        ("main.tex", SAMPLE_LATEX),
        ("intro.tex", SAMPLE_INTRO),
        ("refs.bib", SAMPLE_BIB),
    ];
    let mut paths = Vec::new();
    for (name, text) in files {
        let path = dir.join(name);
        std::fs::write(&path, text)?;
        paths.push(path);
    }
    Ok((paths, dir))
}

fn sample_citations() -> Vec<CiteCandidate> {
    vec![
        CiteCandidate {
            key: "vaswani2017".into(),
            title: "Attention Is All You Need".into(),
            authors: "Vaswani et al.".into(),
            year: Some(2017),
        },
        CiteCandidate {
            key: "devlin2019".into(),
            title: "BERT: Pre-training of Deep Bidirectional Transformers".into(),
            authors: "Devlin et al.".into(),
            year: Some(2019),
        },
        CiteCandidate {
            key: "he2016".into(),
            title: "Deep Residual Learning for Image Recognition".into(),
            authors: "He et al.".into(),
            year: Some(2016),
        },
    ]
}

/// Warnings for cited keys that are neither in `keys` nor labels of the file
/// (Typst `@key`, LaTeX `\cite{a, b}`).
fn unknown_citations(path: &Path, text: &str, keys: &[String]) -> Vec<Diagnostic> {
    let is_key = |c: char| c.is_alphanumeric() || matches!(c, '_' | '-' | ':' | '.');
    let typst = path.extension().is_some_and(|extension| extension == "typ");
    let latex = path.extension().is_some_and(|extension| extension == "tex");
    let mut found = Vec::new();
    for (row, line) in text.lines().enumerate() {
        let code = line.split("//").next().unwrap_or_default();
        let code = if latex {
            line.split('%').next().unwrap_or_default()
        } else {
            code
        };
        if typst {
            for (at, _) in code.match_indices('@') {
                let before = code[..at].chars().next_back();
                if before.is_some_and(|c| c.is_alphanumeric()) {
                    continue;
                }
                let key: String = code[at + 1..].chars().take_while(|&c| is_key(c)).collect();
                let key = key.trim_end_matches(['.', ':']).to_string();
                let is_label = text.contains(&format!("<{key}>"));
                if !key.is_empty() && !is_label && !keys.contains(&key) {
                    let column = code[..at].chars().count() as u32 + 1;
                    found.push((row, column, key));
                }
            }
        }
        if latex {
            for (at, _) in code.match_indices("\\cite") {
                let Some(open) = code[at..].find('{') else {
                    continue;
                };
                let inner_start = at + open + 1;
                let Some(close) = code[inner_start..].find('}') else {
                    continue;
                };
                let mut offset = inner_start;
                for part in code[inner_start..inner_start + close].split(',') {
                    let key = part.trim();
                    let lead = part.len() - part.trim_start().len();
                    if !key.is_empty() && !keys.iter().any(|known| known == key) {
                        let column = code[..offset + lead].chars().count() as u32 + 1;
                        found.push((row, column, key.to_string()));
                    }
                    offset += part.len() + 1;
                }
            }
        }
    }
    found
        .into_iter()
        .map(|(row, column, key)| {
            let mut diagnostic = Diagnostic::warning(format!(
                "No source with key `{key}` in refs.bib"
            ))
            .at(path, row as u32 + 1, column);
            diagnostic
                .hints
                .push("Add it from the Sources panel (⌘⇧B).".into());
            diagnostic
        })
        .collect()
}

// -- The stand-in AI -----------------------------------------------------------

/// A deterministic rewriter: a polish fixes "teh" and doubled spaces; a
/// custom rewrite upper-cases the text when asked to ("uppercase") and
/// gives it back as it is otherwise; a fact fix puts in the year of the
/// remark (or 2014) for 1998, and a firmer phrase for "spread quickly".
/// Answers after `pause`, in two pieces.
struct SampleRewriter {
    pause: Duration,
}

impl Rewriter for SampleRewriter {
    fn name(&self) -> String {
        "Sample rewriter".into()
    }

    fn rewrite(&self, request: RewriteRequest) -> anyhow::Result<TurnHandle> {
        let answer = match request.kind {
            RewriteKind::Polish => {
                let mut text = request.text.replace("teh", "the").replace("Teh", "The");
                while text.contains("  ") {
                    text = text.replace("  ", " ");
                }
                text
            }
            RewriteKind::Custom => {
                if request.guidance.to_lowercase().contains("uppercase") {
                    request.text.to_uppercase()
                } else {
                    request.text.clone()
                }
            }
            RewriteKind::FactFix => {
                let year = request
                    .guidance
                    .lines()
                    .find_map(|line| line.strip_prefix("Remarks: "))
                    .and_then(|remark| {
                        remark
                            .split(|c: char| !c.is_ascii_digit())
                            .find(|word| word.len() == 4)
                    })
                    .unwrap_or("2014");
                request
                    .text
                    .replace("1998", year)
                    .replace("spread quickly", "caught on within a few years")
            }
        };
        let (sender, events) = async_channel::unbounded();
        let stopped = Arc::new(AtomicBool::new(false));
        let stop = stopped.clone();
        let pause = self.pause;
        std::thread::spawn(move || {
            std::thread::sleep(pause);
            if stopped.load(Ordering::SeqCst) {
                let _ = sender.send_blocking(AgentEvent::Failed {
                    message: "Stopped".into(),
                });
                return;
            }
            let half = answer
                .char_indices()
                .nth(answer.chars().count() / 2)
                .map_or(answer.len(), |(index, _)| index);
            for piece in [&answer[..half], &answer[half..]] {
                let _ = sender.send_blocking(AgentEvent::TextDelta { text: piece.into() });
            }
            let _ = sender.send_blocking(AgentEvent::Completed {
                success: true,
                summary: answer,
                session: None,
            });
        });
        Ok(TurnHandle::new(events, move || {
            stop.store(true, Ordering::SeqCst)
        }))
    }
}

/// A deterministic fact checker: "1998" is incorrect, "spread quickly"
/// questionable. Answers after `pause`.
struct SampleChecker {
    pause: Duration,
}

impl FactChecker for SampleChecker {
    fn name(&self) -> String {
        "Sample fact checker".into()
    }

    fn check(&self, request: FactCheckRequest) -> anyhow::Result<FactCheckHandle> {
        let (sender, events) = async_channel::unbounded();
        let stopped = Arc::new(AtomicBool::new(false));
        let stop = stopped.clone();
        let pause = self.pause;
        std::thread::spawn(move || {
            let _ = sender.send_blocking(FactCheckEvent::Status {
                message: "Searching: history of attention mechanisms".into(),
            });
            std::thread::sleep(pause);
            if stopped.load(Ordering::SeqCst) {
                let _ = sender.send_blocking(FactCheckEvent::Failed {
                    message: "Stopped".into(),
                });
                return;
            }
            let mut claims = 1;
            if request.text.contains("1998") {
                claims += 1;
                let _ = sender.send_blocking(FactCheckEvent::Finding(FactFinding {
                    quote: "1998".into(),
                    level: FindingLevel::Incorrect,
                    problem: "Attention in neural networks was introduced in 2014 \
                              (Bahdanau, Cho and Bengio), not in 1998."
                        .into(),
                    suggestion: Some("2014".into()),
                    sources: vec![
                        "https://arxiv.org/abs/1409.0473".into(),
                        "vaswani2017".into(),
                    ],
                }));
            }
            if request.text.contains("spread quickly") {
                claims += 1;
                let _ = sender.send_blocking(FactCheckEvent::Finding(FactFinding {
                    quote: "spread quickly".into(),
                    level: FindingLevel::Questionable,
                    problem: "“Quickly” is vague; the Transformer (2017) is what made it \
                              the default."
                        .into(),
                    suggestion: Some("became the default with the Transformer".into()),
                    sources: vec!["vaswani2017".into()],
                }));
            }
            let _ = sender.send_blocking(FactCheckEvent::Done { claims });
        });
        Ok(FactCheckHandle::new(events, move || {
            stop.store(true, Ordering::SeqCst)
        }))
    }
}

// -- Smoke run ----------------------------------------------------------------

type Log = Rc<RefCell<Vec<EditorEvent>>>;

async fn wait(cx: &AsyncApp, millis: u64) {
    cx.background_executor()
        .timer(Duration::from_millis(millis))
        .await;
}

/// Types text as keystrokes, the way a user would.
fn type_text(text: &str, window: &mut Window, cx: &mut App) {
    for c in text.chars() {
        let key = if c == ' ' {
            "space".to_string()
        } else {
            c.to_string()
        };
        window.dispatch_keystroke(
            Keystroke {
                modifiers: Modifiers::default(),
                key,
                key_char: Some(c.to_string()),
            },
            cx,
        );
    }
}

/// Presses Enter. Dispatched as the action the key is bound to: simulating
/// the keystroke itself would also feed a newline to the input handler,
/// which AppKit does not do for an Enter the editor has handled. Like a real
/// key press, it first brings the frame up to date (the completion menu
/// opens when it is drawn).
fn press_enter(window: &mut Window, cx: &mut App) {
    window.draw(cx).clear(cx);
    window.dispatch_action(
        Box::new(gpui_kit::base::input::Enter {
            secondary: false,
            shift: false,
        }),
        cx,
    );
}

struct Checks {
    failures: Vec<String>,
}

impl Checks {
    fn check(&mut self, ok: bool, what: impl Into<String>) {
        if !ok {
            self.failures.push(what.into());
        }
    }
}

/// Exercises the pane through its public API and real keystrokes.
async fn run_smoke(
    handle: AnyWindowHandle,
    pane: Entity<EditorPane>,
    log: Log,
    files: Vec<PathBuf>,
    cx: &mut AsyncApp,
) -> Vec<String> {
    let mut checks = Checks {
        failures: Vec::new(),
    };
    let [typst, latex, intro, _bib] = files.as_slice() else {
        return vec!["the sample paper has four files".into()];
    };
    let (typst, latex, intro) = (typst.clone(), latex.clone(), intro.clone());
    let read = |path: &Path| std::fs::read_to_string(path).unwrap_or_default();

    // Let the window lay out.
    wait(cx, 300).await;

    // 1. Completion with a snippet, in LaTeX: \begin{fig → Enter.
    let step = handle.update(cx, |_, window, cx| {
        pane.update(cx, |pane, cx| {
            pane.goto(&SourceLocation::new(latex.clone(), 21, 1), window, cx);
        });
        type_text("\\begin{fig", window, cx);
    });
    checks.check(step.is_ok(), "the window is open");
    wait(cx, 200).await;
    let _ = handle.update(cx, |_, window, cx| press_enter(window, cx));
    wait(cx, 150).await;
    let _ = handle.update(cx, |_, window, cx| type_text("plot.pdf", window, cx));
    wait(cx, 100).await;
    let text = cx
        .update(|cx| pane.read(cx).text(&latex, cx))
        .unwrap_or_default();
    checks.check(
        text.contains("\\begin{figure}[t]")
            && text.contains("\\includegraphics[width=\\linewidth]{plot.pdf}")
            && text.contains("\\end{figure}"),
        format!("environment snippet with cursor inside it, got:\n{text}"),
    );

    // 2. Reference completion reads labels from the other LaTeX file. Line
    //    19 is the empty line before "This key is not…".
    let _ = handle.update(cx, |_, window, cx| {
        pane.update(cx, |pane, cx| {
            pane.goto(&SourceLocation::new(latex.clone(), 19, 1), window, cx);
        });
        type_text("See \\ref{sec:i", window, cx);
    });
    wait(cx, 200).await;
    let _ = handle.update(cx, |_, window, cx| press_enter(window, cx));
    wait(cx, 100).await;
    let text = cx
        .update(|cx| pane.read(cx).text(&latex, cx))
        .unwrap_or_default();
    checks.check(
        text.contains("See \\ref{sec:intro}"),
        format!("label completion from intro.tex, got:\n{text}"),
    );
    checks.check(
        cx.update(|cx| pane.read(cx).unsaved(cx).contains_key(&latex)),
        "typing makes the buffer unsaved",
    );

    // 3. ⌘/ comments the line, and again uncomments it.
    let _ = handle.update(cx, |_, window, cx| {
        window.dispatch_action(Box::new(ToggleLineComment), cx)
    });
    let text = cx
        .update(|cx| pane.read(cx).text(&latex, cx))
        .unwrap_or_default();
    checks.check(
        text.contains("% See \\ref{sec:intro}"),
        format!("⌘/ comments the line, got:\n{text}"),
    );
    let _ = handle.update(cx, |_, window, cx| {
        window.dispatch_action(Box::new(ToggleLineComment), cx)
    });

    // 4. Autosave writes it 400 ms after the last change.
    wait(cx, 700).await;
    let buffer = cx
        .update(|cx| pane.read(cx).text(&latex, cx))
        .unwrap_or_default();
    checks.check(read(&latex) == buffer, "autosave wrote the buffer");
    checks.check(
        cx.update(|cx| pane.read(cx).unsaved(cx).is_empty()),
        "nothing unsaved after autosave",
    );
    checks.check(
        log.borrow()
            .iter()
            .any(|event| matches!(event, EditorEvent::Saved { path } if *path == latex)),
        "a Saved event",
    );

    // 5. The editor's own write coming back: nothing happens.
    let edits_before = log.borrow().len();
    let _ = handle.update(cx, |_, window, cx| {
        pane.update(cx, |pane, cx| pane.reload_from_disk(&latex, window, cx));
    });
    checks.check(log.borrow().len() == edits_before, "an echo is ignored");

    // 6. An outside change to a clean buffer is taken in.
    let changed = format!("{}\n% Appended by the agent.\n", read(&intro));
    let _ = std::fs::write(&intro, &changed);
    let _ = handle.update(cx, |_, window, cx| {
        pane.update(cx, |pane, cx| pane.reload_from_disk(&intro, window, cx));
    });
    let text = cx
        .update(|cx| pane.read(cx).text(&intro, cx))
        .unwrap_or_default();
    checks.check(text == changed, "a clean buffer takes the outside change");
    checks.check(
        cx.update(|cx| !pane.read(cx).unsaved(cx).contains_key(&intro)),
        "and stays clean",
    );

    // 7. An outside change to a dirty buffer is merged with the user's.
    let _ = handle.update(cx, |_, window, cx| {
        pane.update(cx, |pane, cx| {
            pane.goto(&SourceLocation::new(typst.clone(), 7, 1), window, cx);
            pane.insert_at_cursor("Typed by you. ", window, cx);
        });
    });
    let disk = read(&typst).replace("Some claims still", "Some claims, the agent says, still");
    let _ = std::fs::write(&typst, &disk);
    let _ = handle.update(cx, |_, window, cx| {
        pane.update(cx, |pane, cx| pane.reload_from_disk(&typst, window, cx));
    });
    let merged = cx
        .update(|cx| pane.read(cx).text(&typst, cx))
        .unwrap_or_default();
    checks.check(
        merged.contains("Typed by you. ") && merged.contains("the agent says"),
        format!("both edits survive the merge, got:\n{merged}"),
    );
    let cursor = cx.update(|cx| pane.read(cx).cursor(cx));
    checks.check(
        cursor
            .as_ref()
            .is_some_and(|c| c.line == 7 && c.column == 15),
        format!("the cursor stays after the typed text, got {cursor:?}"),
    );

    // 8. Typst citation completion after @.
    let _ = handle.update(cx, |_, window, cx| type_text("@vas", window, cx));
    wait(cx, 200).await;
    let _ = handle.update(cx, |_, window, cx| press_enter(window, cx));
    wait(cx, 100).await;
    let text = cx
        .update(|cx| pane.read(cx).text(&typst, cx))
        .unwrap_or_default();
    checks.check(
        text.contains("Typed by you. @vaswani2017"),
        "citation completion after @",
    );

    // 9. Diagnostics set and cleared; go-to lands on the line.
    let _ = handle.update(cx, |_, window, cx| {
        pane.update(cx, |pane, cx| {
            pane.set_diagnostics(
                &[Diagnostic::error("unknown variable: x").at(typst.clone(), 3, 2)],
                window,
                cx,
            );
            pane.set_diagnostics(&[], window, cx);
            pane.goto(&SourceLocation::new(typst.clone(), 12, 3), window, cx);
        });
    });
    let cursor = cx.update(|cx| pane.read(cx).cursor(cx));
    checks.check(
        cursor
            .as_ref()
            .is_some_and(|c| c.file == typst && c.line == 12 && c.column == 3),
        format!("go-to places the cursor, got {cursor:?}"),
    );

    // 10. The merged text and the completion reach the disk too.
    wait(cx, 700).await;
    let buffer = cx
        .update(|cx| pane.read(cx).text(&typst, cx))
        .unwrap_or_default();
    checks.check(read(&typst) == buffer, "the merged buffer is saved");
    checks.check(
        log.borrow()
            .iter()
            .any(|event| matches!(event, EditorEvent::CursorMoved { .. })),
        "CursorMoved is reported",
    );

    // 11. ⌘B polishes the sentence at the cursor, as one undoable edit.
    let ai_text = |cx: &mut AsyncApp| {
        cx.update(|cx| pane.read(cx).text(&typst, cx))
            .unwrap_or_default()
            .lines()
            .nth(AI_LINE as usize - 1)
            .unwrap_or_default()
            .to_string()
    };
    let _ = handle.update(cx, |_, window, cx| {
        pane.update(cx, |pane, cx| {
            pane.goto(&SourceLocation::new(typst.clone(), AI_LINE, 12), window, cx);
        });
        window.dispatch_action(Box::new(Polish), cx);
    });
    wait(cx, 700).await;
    let polished = ai_text(cx);
    checks.check(
        polished == "Attention was first proposed in 1998, and the idea spread quickly.",
        format!("⌘B polishes the sentence, got {polished:?}"),
    );
    let _ = handle.update(cx, |_, window, cx| {
        window.dispatch_action(Box::new(gpui_kit::base::input::Undo), cx)
    });
    let undone = ai_text(cx);
    checks.check(
        undone == "Attention was first proposed in 1998, and teh idea  spread quickly.",
        format!("⌘Z restores the original, got {undone:?}"),
    );

    // 12. ⇧⌘B opens the prompt box; the instruction typed in it is carried
    //     out on the sentence, and undone like any edit.
    let _ = handle.update(cx, |_, window, cx| {
        window.dispatch_action(Box::new(CustomPrompt), cx);
    });
    wait(cx, 100).await;
    let _ = handle.update(cx, |_, window, cx| {
        // Like a real key press, the typing lands in what is on screen: the
        // box drawn, its field taking keys.
        window.draw(cx).clear(cx);
        type_text("uppercase", window, cx);
    });
    wait(cx, 100).await;
    let _ = handle.update(cx, |_, window, cx| press_enter(window, cx));
    wait(cx, 700).await;
    let shouted = ai_text(cx);
    checks.check(
        shouted == "ATTENTION WAS FIRST PROPOSED IN 1998, AND TEH IDEA  SPREAD QUICKLY.",
        format!("⇧⌘B rewrites as instructed, got {shouted:?}"),
    );
    let _ = handle.update(cx, |_, window, cx| {
        window.dispatch_action(Box::new(gpui_kit::base::input::Undo), cx)
    });
    checks.check(
        ai_text(cx) == undone,
        format!("⌘Z undoes the rewrite, got {:?}", ai_text(cx)),
    );

    // 13. ⌘I underlines two findings; editing one's text removes it.
    let _ = handle.update(cx, |_, window, cx| {
        window.dispatch_action(Box::new(FactCheck), cx)
    });
    wait(cx, 700).await;
    let findings = cx.update(|cx| pane.read(cx).findings_count());
    checks.check(findings == 2, format!("two findings, got {findings}"));
    checks.check(
        log.borrow().iter().any(|event| {
            matches!(
                event,
                EditorEvent::FactCheckFinished {
                    findings: 2,
                    claims: 3,
                    ..
                }
            )
        }),
        "FactCheckFinished reports two findings of three claims",
    );
    let _ = handle.update(cx, |_, window, cx| {
        pane.update(cx, |pane, cx| {
            // Inside "1998".
            pane.goto(&SourceLocation::new(typst.clone(), AI_LINE, 35), window, cx);
            pane.insert_at_cursor("9", window, cx);
        });
    });
    let findings = cx.update(|cx| pane.read(cx).findings_count());
    checks.check(
        findings == 1,
        format!("an edited finding goes, got {findings}"),
    );

    // 14. Closing saves and empties the pane.
    let closed = handle
        .update(cx, |_, window, cx| {
            pane.update(cx, |pane, cx| pane.close_all(window, cx))
        })
        .ok()
        .and_then(|result| result.ok());
    checks.check(closed.is_some(), "close_all succeeds");
    checks.check(
        cx.update(|cx| pane.read(cx).open_paths().is_empty()),
        "close_all closes everything",
    );

    checks.failures
}
