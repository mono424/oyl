//! The writing aids in a headless GPUI window: ⌘B, ⇧⌘B and ⌘I through the
//! real key bindings, typing through the window's input handler, the card
//! through clicks and the pointer, with a fake rewriter and fact checker
//! that answer when the test says so.

use std::cell::RefCell;
use std::ops::Range;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use gpui_kit::{
    AnyWindowHandle, AppContext as _, Bounds, Entity, Keystroke, Modifiers, Point, TestAppContext,
    VisualTestContext, WindowBounds, WindowOptions, px, size,
};
use oyl_model::Engine;
use oyl_model::agent::{
    AgentEvent, FactCheckEvent, FactCheckHandle, FactCheckRequest, FactChecker, FactFinding,
    FindingLevel, RewriteKind, RewriteRequest, Rewriter, TurnHandle,
};

use super::{
    ALREADY_FINE, CHANGED_WHILE_POLISHING, NO_AI_FACTS, NO_AI_POLISH, NO_AI_REWRITE, POLISHED,
    PROMPT_HISTORY, REWRITTEN, Spot,
};
use crate::pane::{EditorEvent, EditorPane};

/// A rewriter that records each request and answers on cue.
#[derive(Default)]
struct FakeRewriter {
    calls: Mutex<Vec<(RewriteRequest, async_channel::Sender<AgentEvent>)>>,
    cancelled: Arc<AtomicUsize>,
}

impl FakeRewriter {
    fn count(&self) -> usize {
        self.calls.lock().unwrap().len()
    }

    fn request(&self, call: usize) -> RewriteRequest {
        self.calls.lock().unwrap()[call].0.clone()
    }

    /// Sends an event of a call; once the pane has let go of the call (it
    /// was called off), nobody listens and the event is dropped.
    fn send(&self, call: usize, event: AgentEvent) {
        self.calls.lock().unwrap()[call].1.try_send(event).ok();
    }

    fn listened_to(&self, call: usize) -> bool {
        !self.calls.lock().unwrap()[call].1.is_closed()
    }

    fn complete(&self, call: usize, text: &str) {
        self.send(
            call,
            AgentEvent::Completed {
                success: true,
                summary: text.into(),
                session: None,
            },
        );
    }

    fn cancelled(&self) -> usize {
        self.cancelled.load(Ordering::SeqCst)
    }
}

impl Rewriter for FakeRewriter {
    fn name(&self) -> String {
        "Fake rewriter".into()
    }

    fn rewrite(&self, request: RewriteRequest) -> anyhow::Result<TurnHandle> {
        let (sender, events) = async_channel::unbounded();
        self.calls.lock().unwrap().push((request, sender));
        let cancelled = self.cancelled.clone();
        Ok(TurnHandle::new(events, move || {
            cancelled.fetch_add(1, Ordering::SeqCst);
        }))
    }
}

/// A fact checker that records each request and answers on cue.
#[derive(Default)]
struct FakeChecker {
    calls: Mutex<Vec<(FactCheckRequest, async_channel::Sender<FactCheckEvent>)>>,
}

impl FakeChecker {
    fn request(&self, call: usize) -> FactCheckRequest {
        self.calls.lock().unwrap()[call].0.clone()
    }

    fn send(&self, call: usize, event: FactCheckEvent) {
        self.calls.lock().unwrap()[call].1.try_send(event).unwrap();
    }
}

impl FactChecker for FakeChecker {
    fn name(&self) -> String {
        "Fake checker".into()
    }

    fn check(&self, request: FactCheckRequest) -> anyhow::Result<FactCheckHandle> {
        let (sender, events) = async_channel::unbounded();
        self.calls.lock().unwrap().push((request, sender));
        Ok(FactCheckHandle::new(events, || {}))
    }
}

fn finding(quote: &str, level: FindingLevel, suggestion: Option<&str>) -> FactCheckEvent {
    FactCheckEvent::Finding(FactFinding {
        quote: quote.into(),
        level,
        problem: format!("“{quote}” is not right."),
        suggestion: suggestion.map(Into::into),
        sources: vec![
            "https://arxiv.org/abs/1409.0473".into(),
            "bahdanau2014".into(),
        ],
    })
}

struct Harness {
    window: AnyWindowHandle,
    pane: Entity<EditorPane>,
    path: PathBuf,
    rewriter: Arc<FakeRewriter>,
    checker: Arc<FakeChecker>,
    events: Rc<RefCell<Vec<EditorEvent>>>,
    _dir: tempfile::TempDir,
}

impl Harness {
    fn open(cx: &mut TestAppContext, name: &str, text: &str) -> Self {
        cx.update(|cx| {
            gpui_kit::init(cx);
            oyl_ui::install(oyl_ui::Appearance::Light, cx);
        });
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(name);
        std::fs::write(&path, text).unwrap();
        let (window, pane) = cx.update(|cx| {
            gpui_kit::open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(Bounds {
                        origin: Point::default(),
                        size: size(px(900.), px(700.)),
                    })),
                    ..Default::default()
                },
                cx,
                |window, cx| cx.new(|cx| EditorPane::new(window, cx)),
            )
            .unwrap()
        });
        let rewriter = Arc::new(FakeRewriter::default());
        let checker = Arc::new(FakeChecker::default());
        let events = Rc::new(RefCell::new(Vec::new()));
        let sink = events.clone();
        cx.update_window(window, |_, window, cx| {
            pane.update(cx, |pane, cx| {
                pane.open(&path, window, cx).unwrap();
                let rewriter: Arc<dyn Rewriter> = rewriter.clone();
                let checker: Arc<dyn FactChecker> = checker.clone();
                pane.set_rewriter(Some(rewriter), cx);
                pane.set_fact_checker(Some(checker), cx);
                pane.set_bibliography_provider(Rc::new(|_| "@misc{bahdanau2014}".to_string()));
            });
            cx.subscribe(&pane, move |_, event: &EditorEvent, _| {
                sink.borrow_mut().push(event.clone())
            })
            .detach();
        })
        .unwrap();
        let harness = Self {
            window,
            pane,
            path,
            rewriter,
            checker,
            events,
            _dir: dir,
        };
        harness.frame(cx);
        harness
    }

    /// Draws two frames (the second places what the first measured) and
    /// lets every task run.
    fn frame(&self, cx: &mut TestAppContext) {
        for _ in 0..2 {
            cx.update_window(self.window, |_, window, cx| {
                window.refresh();
                window.draw(cx).clear(cx);
            })
            .unwrap();
            cx.run_until_parked();
        }
    }

    fn visual(&self, cx: &TestAppContext) -> VisualTestContext {
        VisualTestContext::from_window(self.window, cx)
    }

    fn text(&self, cx: &TestAppContext) -> String {
        self.pane
            .read_with(cx, |pane, cx| pane.text(&self.path, cx).unwrap())
    }

    fn select(&self, range: Range<usize>, cx: &mut TestAppContext) {
        cx.update_window(self.window, |_, _, cx| {
            let state = self.pane.read(cx).active_state().unwrap();
            state.update(cx, |state, cx| state.set_selected_range(range, cx));
        })
        .unwrap();
        cx.run_until_parked();
        self.frame(cx);
    }

    fn selection(&self, cx: &TestAppContext) -> Range<usize> {
        self.pane.read_with(cx, |pane, cx| {
            pane.active_state().unwrap().read(cx).selected_range()
        })
    }

    fn keys(&self, keys: &str, cx: &mut TestAppContext) {
        self.visual(cx).simulate_keystrokes(keys);
        self.frame(cx);
    }

    /// Types text as key presses, as a user would.
    fn type_text(&self, text: &str, cx: &mut TestAppContext) {
        cx.update_window(self.window, |_, window, cx| {
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
        })
        .unwrap();
        cx.run_until_parked();
        self.frame(cx);
    }

    fn click(&self, selector: &'static str, cx: &mut TestAppContext) {
        let mut visual = self.visual(cx);
        let bounds = visual
            .debug_bounds(selector)
            .unwrap_or_else(|| panic!("{selector} is on screen"));
        visual.simulate_click(bounds.center(), Modifiers::default());
        self.frame(cx);
    }

    fn on_screen(&self, selector: &'static str, cx: &TestAppContext) -> bool {
        self.visual(cx).debug_bounds(selector).is_some()
    }

    fn jobs(&self, cx: &TestAppContext) -> usize {
        self.pane
            .read_with(cx, |pane, _| pane.buffers[0].assist.jobs.len())
    }

    fn notes(&self, cx: &TestAppContext) -> Vec<String> {
        self.pane.read_with(cx, |pane, _| {
            pane.buffers[0]
                .assist
                .notes
                .iter()
                .map(|note| note.text.clone())
                .collect()
        })
    }

    fn findings(&self, cx: &TestAppContext) -> Vec<(String, FindingLevel)> {
        self.pane.read_with(cx, |pane, _| {
            pane.buffers[0]
                .assist
                .findings
                .iter()
                .map(|finding| (finding.quote.clone(), finding.level))
                .collect()
        })
    }

    fn prompt_open(&self, cx: &TestAppContext) -> bool {
        self.pane
            .read_with(cx, |pane, _| pane.assist.prompt.is_some())
    }

    /// What the prompt box's field holds.
    fn prompt_text(&self, cx: &TestAppContext) -> String {
        self.pane.read_with(cx, |pane, cx| {
            let prompt = pane.assist.prompt.as_ref().unwrap();
            prompt.input.read(cx).value().to_string()
        })
    }

    fn card_open(&self, cx: &TestAppContext) -> bool {
        self.pane
            .read_with(cx, |pane, _| pane.assist.card.is_some())
    }

    /// Where the text of `range` is on screen.
    fn bounds_of(&self, range: Range<usize>, cx: &TestAppContext) -> Bounds<gpui_kit::Pixels> {
        self.pane.read_with(cx, |pane, cx| {
            pane.active_state()
                .unwrap()
                .read(cx)
                .range_to_bounds(&range)
                .unwrap()
        })
    }
}

fn range_of(text: &str, part: &str) -> Range<usize> {
    let start = text.find(part).expect("part of the text");
    start..start + part.len()
}

// -- ⌘B ---------------------------------------------------------------------------

#[gpui::test]
fn polish_replaces_the_selection_as_one_undoable_edit(cx: &mut TestAppContext) {
    let original = "We has teh results.\n\nAnother paragraph here.\n";
    let h = Harness::open(cx, "main.typ", original);
    h.select(0..19, cx);
    h.keys("cmd-b", cx);

    assert_eq!(h.rewriter.count(), 1);
    let request = h.rewriter.request(0);
    assert_eq!(request.kind, RewriteKind::Polish);
    assert_eq!(request.text, "We has teh results.");
    assert_eq!(request.before, "");
    assert_eq!(request.after, "\n\nAnother paragraph here.\n");
    assert_eq!(request.engine, Some(Engine::Typst));
    assert!(request.guidance.is_empty());

    // The working state: a fill over the text and a label under it.
    let (fills, label) = h.pane.read_with(cx, |pane, cx| {
        let assist = &pane.buffers[0].assist;
        let id = assist.jobs[0].id;
        (
            assist.fills.get_ranges(cx),
            pane.assist.geometry.borrow().contains_key(&Spot::Label(id)),
        )
    });
    assert!(fills.contains(&(0..19)), "{fills:?}");
    assert!(label, "the label is placed under the line");

    h.rewriter.send(
        0,
        AgentEvent::TextDelta {
            text: "We have".into(),
        },
    );
    h.rewriter.complete(0, "We have the results.");
    h.frame(cx);

    assert_eq!(
        h.text(cx),
        "We have the results.\n\nAnother paragraph here.\n"
    );
    assert_eq!(h.selection(cx), 0..20, "the new text is selected");
    assert_eq!(h.jobs(cx), 0);
    assert_eq!(h.notes(cx), vec![POLISHED.to_string()]);
    let (flashes, marks) = h.pane.read_with(cx, |pane, cx| {
        let assist = &pane.buffers[0].assist;
        let flashes: Vec<Range<usize>> = assist.flashes.iter().map(|f| f.range.clone()).collect();
        (flashes, assist.marks.get_ranges(cx))
    });
    assert_eq!(flashes, vec![3..11], "“have the” changed");
    assert_eq!(marks, vec![3..11]);

    // The underline fades out within two seconds; the label stays longer.
    cx.executor().advance_clock(Duration::from_secs(2));
    cx.run_until_parked();
    let marks = h
        .pane
        .read_with(cx, |pane, cx| pane.buffers[0].assist.marks.get_ranges(cx));
    assert!(marks.is_empty(), "{marks:?}");
    assert_eq!(h.notes(cx), vec![POLISHED.to_string()]);

    // ⌘Z puts the original back exactly, and the label goes with it.
    h.keys("cmd-z", cx);
    assert_eq!(h.text(cx), original);
    assert!(h.notes(cx).is_empty());
}

#[gpui::test]
fn polish_without_a_selection_takes_the_sentence_at_the_cursor(cx: &mut TestAppContext) {
    let text = "= Results\n\nWe has teh results. They is good.\n";
    let h = Harness::open(cx, "main.typ", text);
    let they = text.find("They").unwrap();
    h.select(they + 2..they + 2, cx);
    h.keys("cmd-b", cx);
    assert_eq!(h.rewriter.request(0).text, "They is good.");
    assert_eq!(
        h.rewriter.request(0).before,
        "= Results\n\nWe has teh results. "
    );

    h.rewriter.complete(0, "They are good.");
    h.frame(cx);
    assert_eq!(
        h.text(cx),
        "= Results\n\nWe has teh results. They are good.\n"
    );
    assert_eq!(
        h.selection(cx),
        they..they + "They are good.".len(),
        "the cursor had not moved: the result is selected"
    );

    // On a blank line there is nothing to polish.
    h.select(10..10, cx);
    h.keys("cmd-b", cx);
    assert_eq!(h.rewriter.count(), 1);

    // Without a rewriter the label says so.
    h.pane.update(cx, |pane, cx| pane.set_rewriter(None, cx));
    h.select(0..5, cx);
    h.keys("cmd-b", cx);
    assert_eq!(h.rewriter.count(), 1);
    assert!(h.notes(cx).contains(&NO_AI_POLISH.to_string()));
}

#[gpui::test]
fn typing_inside_a_running_polish_calls_it_off(cx: &mut TestAppContext) {
    let text = "One sentence here. Teh second one.\n";
    let h = Harness::open(cx, "notes.tex", text);
    let second = range_of(text, "Teh second one.");
    h.select(second.clone(), cx);
    h.keys("cmd-b", cx);
    assert_eq!(h.rewriter.request(0).engine, Some(Engine::Latex));

    h.select(second.start + 2..second.start + 2, cx);
    h.type_text("x", cx);
    assert_eq!(h.text(cx), "One sentence here. Texh second one.\n");
    assert_eq!(h.jobs(cx), 0);
    assert_eq!(h.rewriter.cancelled(), 1);
    assert_eq!(h.notes(cx), vec![CHANGED_WHILE_POLISHING.to_string()]);

    // The pane stopped listening; a late answer changes nothing.
    assert!(!h.rewriter.listened_to(0));
    h.rewriter.complete(0, "The second one.");
    h.frame(cx);
    assert_eq!(h.text(cx), "One sentence here. Texh second one.\n");
}

#[gpui::test]
fn typing_elsewhere_moves_the_running_polish_along(cx: &mut TestAppContext) {
    let text = "One sentence here. Teh second one.\n";
    let h = Harness::open(cx, "main.typ", text);
    h.select(range_of(text, "Teh second one."), cx);
    h.keys("cmd-b", cx);

    h.select(0..0, cx);
    h.type_text("Hey ", cx);
    assert_eq!(h.jobs(cx), 1, "still running");
    let range = h
        .pane
        .read_with(cx, |pane, _| pane.buffers[0].assist.jobs[0].range.clone());
    assert_eq!(range, 23..38);

    h.rewriter.complete(0, "The second one.");
    h.frame(cx);
    assert_eq!(h.text(cx), "Hey One sentence here. The second one.\n");
    assert_eq!(
        h.selection(cx),
        4..4,
        "the cursor stays where the user typed"
    );
}

#[gpui::test]
fn already_fine_failures_second_cmd_b_and_escape(cx: &mut TestAppContext) {
    let text = "A fine sentence.\n";
    let h = Harness::open(cx, "main.typ", text);
    let all = 0..16;

    h.select(all.clone(), cx);
    h.keys("cmd-b", cx);
    h.rewriter.complete(0, "A fine sentence.");
    h.frame(cx);
    assert_eq!(h.text(cx), text);
    assert_eq!(h.notes(cx), vec![ALREADY_FINE.to_string()]);

    h.keys("cmd-b", cx);
    h.rewriter.send(
        1,
        AgentEvent::Failed {
            message: "rate limited".into(),
        },
    );
    h.frame(cx);
    assert_eq!(h.text(cx), text);
    assert_eq!(
        h.notes(cx),
        vec!["Polishing failed: rate limited".to_string()]
    );

    // A second ⌘B on the running polish calls it off.
    h.keys("cmd-b", cx);
    assert_eq!(h.jobs(cx), 1);
    h.keys("cmd-b", cx);
    assert_eq!(h.jobs(cx), 0);
    assert_eq!(h.rewriter.cancelled(), 1);
    assert_eq!(h.rewriter.count(), 3);

    // So does Esc.
    h.keys("cmd-b", cx);
    h.keys("escape", cx);
    assert_eq!(h.jobs(cx), 0);
    assert_eq!(h.rewriter.cancelled(), 2);
    assert_eq!(h.text(cx), text);
}

#[gpui::test]
fn several_polishes_run_at_once(cx: &mut TestAppContext) {
    let text = "Teh first. Teh second.\n";
    let h = Harness::open(cx, "main.typ", text);
    h.select(0..10, cx);
    h.keys("cmd-b", cx);
    h.select(11..22, cx);
    h.keys("cmd-b", cx);
    assert_eq!(h.jobs(cx), 2);
    h.rewriter.complete(1, "The second.");
    h.rewriter.complete(0, "The first.");
    h.frame(cx);
    assert_eq!(h.text(cx), "The first. The second.\n");
}

// -- ⇧⌘B -------------------------------------------------------------------------

#[gpui::test]
fn the_prompt_box_rewrites_as_instructed(cx: &mut TestAppContext) {
    let original = "Results were good. We has teh numbers.\n";
    let h = Harness::open(cx, "main.tex", original);
    let sentence = range_of(original, "We has teh numbers.");
    h.select(sentence.clone(), cx);
    h.keys("cmd-shift-b", cx);
    assert!(h.prompt_open(cx));
    assert!(h.on_screen("oyl-prompt", cx));
    assert_eq!(h.rewriter.count(), 0, "nothing is asked before Enter");
    let fills = h
        .pane
        .read_with(cx, |pane, cx| pane.buffers[0].assist.fills.get_ranges(cx));
    assert_eq!(
        fills,
        vec![sentence.clone()],
        "the text keeps a selection tint"
    );

    // Enter on an empty box does nothing.
    h.keys("enter", cx);
    assert!(h.prompt_open(cx));
    assert_eq!(h.rewriter.count(), 0);

    h.type_text("make it formal", cx);
    assert_eq!(h.prompt_text(cx), "make it formal");
    h.keys("enter", cx);
    assert!(!h.prompt_open(cx));
    assert_eq!(h.rewriter.count(), 1);
    let request = h.rewriter.request(0);
    assert_eq!(request.kind, RewriteKind::Custom);
    assert_eq!(request.guidance, "make it formal");
    assert_eq!(request.text, "We has teh numbers.");
    assert_eq!(request.before, "Results were good. ");
    assert_eq!(request.after, "\n");
    assert_eq!(request.engine, Some(Engine::Latex));
    assert_eq!(h.jobs(cx), 1, "the same working state as ⌘B");

    h.rewriter.complete(0, "We have the figures.");
    h.frame(cx);
    assert_eq!(h.text(cx), "Results were good. We have the figures.\n");
    assert_eq!(h.selection(cx), 19..39, "the new text is selected");
    assert_eq!(h.notes(cx), vec![REWRITTEN.to_string()]);

    h.keys("cmd-z", cx);
    assert_eq!(h.text(cx), original);
}

#[gpui::test]
fn escape_or_a_click_outside_closes_the_prompt_box(cx: &mut TestAppContext) {
    let text = "One sentence here. Another one there.\n";
    let h = Harness::open(cx, "main.typ", text);
    // Without a selection: the sentence at the cursor.
    h.select(25..25, cx);
    h.keys("cmd-shift-b", cx);
    let range = h.pane.read_with(cx, |pane, _| {
        pane.assist.prompt.as_ref().unwrap().range.clone()
    });
    assert_eq!(&text[range], "Another one there.");
    h.type_text("shorter", cx);
    h.keys("escape", cx);
    assert!(!h.prompt_open(cx));
    assert_eq!(h.rewriter.count(), 0);
    // The editor has the keys again.
    h.type_text("x", cx);
    assert_eq!(h.text(cx), "One sentence here. Anothexr one there.\n");

    h.keys("cmd-shift-b", cx);
    assert!(h.prompt_open(cx));
    let elsewhere = h.bounds_of(0..3, cx).center();
    h.visual(cx).simulate_click(elsewhere, Modifiers::default());
    h.frame(cx);
    assert!(!h.prompt_open(cx), "a click outside closes it");
    assert_eq!(h.rewriter.count(), 0);
    let fills = h
        .pane
        .read_with(cx, |pane, cx| pane.buffers[0].assist.fills.get_ranges(cx));
    assert!(fills.is_empty());

    // ⇧⌘B again closes it too; without a rewriter it does not open.
    h.keys("cmd-shift-b", cx);
    h.keys("cmd-shift-b", cx);
    assert!(!h.prompt_open(cx));
    h.pane.update(cx, |pane, cx| pane.set_rewriter(None, cx));
    h.keys("cmd-shift-b", cx);
    assert!(!h.prompt_open(cx));
    assert!(h.notes(cx).contains(&NO_AI_REWRITE.to_string()));
}

#[gpui::test]
fn up_in_the_empty_box_recalls_earlier_instructions(cx: &mut TestAppContext) {
    let text = "Some text to work on.\n";
    let h = Harness::open(cx, "main.typ", text);
    let all = 0..21;
    for (call, instruction) in ["first", "second"].into_iter().enumerate() {
        h.select(all.clone(), cx);
        h.keys("cmd-shift-b", cx);
        h.type_text(instruction, cx);
        h.keys("enter", cx);
        h.rewriter.complete(call, "Some text to work on.");
        h.frame(cx);
    }

    h.select(all.clone(), cx);
    h.keys("cmd-shift-b", cx);
    h.keys("up", cx);
    assert_eq!(h.prompt_text(cx), "second");
    h.keys("up", cx);
    assert_eq!(h.prompt_text(cx), "first");
    h.keys("up", cx);
    assert_eq!(h.prompt_text(cx), "first", "the oldest stays");
    h.keys("down", cx);
    assert_eq!(h.prompt_text(cx), "second");
    h.keys("down", cx);
    assert_eq!(h.prompt_text(cx), "", "past the newest: empty again");

    // A recalled instruction is sent like a typed one.
    h.keys("up", cx);
    h.keys("enter", cx);
    assert_eq!(h.rewriter.request(2).guidance, "second");
    h.rewriter.complete(2, "Some text to work on.");
    h.frame(cx);

    // Only the last ten are kept, each once.
    h.pane.update(cx, |pane, _| {
        for n in 0..12 {
            pane.assist.history.push(format!("instruction {n}"));
        }
        pane.assist
            .history
            .drain(..pane.assist.history.len() - PROMPT_HISTORY);
    });
    h.select(all.clone(), cx);
    h.keys("cmd-shift-b", cx);
    h.type_text("instruction 5", cx);
    h.keys("enter", cx);
    let history = h.pane.read_with(cx, |pane, _| pane.assist.history.clone());
    assert_eq!(history.len(), PROMPT_HISTORY);
    assert_eq!(history.last().map(String::as_str), Some("instruction 5"));
    assert_eq!(history.iter().filter(|i| *i == "instruction 5").count(), 1);
}

#[gpui::test]
fn the_prompt_box_stays_when_its_text_scrolls_out_of_view(cx: &mut TestAppContext) {
    let mut text: String = (1..=120)
        .map(|n| format!("Line number {n} here.\n"))
        .collect();
    text.push_str("The last sentence.\n");
    let h = Harness::open(cx, "main.typ", &text);
    let last = range_of(&text, "The last sentence.");
    h.select(last.clone(), cx);
    h.keys("cmd-shift-b", cx);
    assert!(h.on_screen("oyl-prompt", cx));

    // Scrolled to the top, the text is out of view; the box is still there
    // and still has the keys.
    cx.update_window(h.window, |_, _, cx| {
        let state = h.pane.read(cx).active_state().unwrap();
        state.update(cx, |state, cx| {
            state.set_scroll_offset(gpui_kit::point(px(0.), px(0.)), cx)
        });
    })
    .unwrap();
    h.frame(cx);
    assert!(h.on_screen("oyl-prompt", cx));
    h.type_text("shorter", cx);
    assert_eq!(h.prompt_text(cx), "shorter");
    h.keys("enter", cx);
    assert_eq!(h.rewriter.request(0).text, "The last sentence.");
}

// -- ⌘I ---------------------------------------------------------------------------

#[gpui::test]
fn fact_check_underlines_findings_then_apply_and_dismiss(cx: &mut TestAppContext) {
    let text = "Attention was first proposed in 1998, and the idea spread quickly.\n\nUnrelated.\n";
    let h = Harness::open(cx, "main.typ", text);
    h.select(5..5, cx);
    h.keys("cmd-i", cx);

    let request = h.checker.request(0);
    assert_eq!(
        request.text,
        "Attention was first proposed in 1998, and the idea spread quickly."
    );
    assert_eq!(request.after, "\n\nUnrelated.\n");
    assert_eq!(request.bibliography, "@misc{bahdanau2014}");
    assert_eq!(request.engine, Some(Engine::Typst));

    h.checker.send(
        0,
        FactCheckEvent::Status {
            message: "Searching: attention 1998".into(),
        },
    );
    h.frame(cx);
    let status = h
        .pane
        .read_with(cx, |pane, _| pane.buffers[0].assist.jobs[0].status.clone());
    assert_eq!(status.as_deref(), Some("Searching: attention 1998"));

    h.checker
        .send(0, finding("1998", FindingLevel::Incorrect, Some("2014")));
    // Whitespace-insensitive: the checker doubled a space.
    h.checker.send(
        0,
        finding(
            "spread  quickly",
            FindingLevel::Questionable,
            Some("caught on"),
        ),
    );
    h.checker
        .send(0, finding("not in the text", FindingLevel::Incorrect, None));
    h.checker.send(0, FactCheckEvent::Done { claims: 3 });
    h.frame(cx);

    assert_eq!(h.pane.read_with(cx, |pane, _| pane.findings_count()), 2);
    assert_eq!(
        h.findings(cx),
        vec![
            ("1998".to_string(), FindingLevel::Incorrect),
            ("spread quickly".to_string(), FindingLevel::Questionable),
        ]
    );
    assert_eq!(
        h.notes(cx),
        vec!["2 problems found · 3 claims checked · 1 not located".to_string()]
    );
    assert!(h.events.borrow().contains(&EditorEvent::FactCheckFinished {
        path: h.path.clone(),
        findings: 2,
        claims: 3,
    }));
    let year = range_of(text, "1998");
    let marks = h
        .pane
        .read_with(cx, |pane, cx| pane.buffers[0].assist.marks.get_ranges(cx));
    assert_eq!(marks, vec![year.clone()], "incorrect: a wavy underline");

    // The cursor in the year opens its card; Apply replaces it.
    h.select(year.start + 1..year.start + 1, cx);
    assert!(h.card_open(cx));
    assert!(h.on_screen("oyl-fact-card", cx));
    h.click("oyl-card-apply", cx);
    assert_eq!(
        h.text(cx),
        "Attention was first proposed in 2014, and the idea spread quickly.\n\nUnrelated.\n"
    );
    assert_eq!(h.pane.read_with(cx, |pane, _| pane.findings_count()), 1);
    assert!(!h.card_open(cx));

    // Dismiss removes the other one, leaving the text alone.
    let spread = range_of(&h.text(cx), "spread quickly");
    h.select(spread.start + 3..spread.start + 3, cx);
    assert!(h.card_open(cx));
    h.click("oyl-card-dismiss", cx);
    assert_eq!(h.pane.read_with(cx, |pane, _| pane.findings_count()), 0);
    assert!(h.text(cx).contains("spread quickly"));
    assert!(!h.card_open(cx));
}

#[gpui::test]
fn rewrite_with_a_remark_streams_a_suggestion_that_apply_uses(cx: &mut TestAppContext) {
    let text = "The method was first proposed in 1998.\n";
    let h = Harness::open(cx, "main.typ", text);
    h.select(0..38, cx);
    h.keys("cmd-i", cx);
    h.checker.send(
        0,
        finding("in 1998", FindingLevel::Incorrect, Some("in 2014")),
    );
    h.checker.send(0, FactCheckEvent::Done { claims: 1 });
    h.frame(cx);

    let quote = range_of(text, "in 1998");
    h.select(quote.start + 1..quote.start + 1, cx);
    assert!(h.card_open(cx));
    // Rewrite waits for a remark.
    h.click("oyl-card-rewrite", cx);
    assert_eq!(h.rewriter.count(), 0);

    h.click("oyl-card-remark", cx);
    h.type_text("use the 2017 figure", cx);
    assert!(
        h.card_open(cx),
        "the card stays open while its field has focus"
    );
    h.keys("enter", cx);

    assert_eq!(h.rewriter.count(), 1);
    let request = h.rewriter.request(0);
    assert_eq!(request.kind, RewriteKind::FactFix);
    assert_eq!(request.text, "in 1998");
    assert_eq!(
        request.guidance,
        "Problem: “in 1998” is not right.\n\
         Sources: https://arxiv.org/abs/1409.0473; bahdanau2014\n\
         Remarks: use the 2017 figure"
    );

    h.rewriter.send(
        0,
        AgentEvent::TextDelta {
            text: "in 20".into(),
        },
    );
    h.frame(cx);
    let streamed = h.pane.read_with(cx, |pane, _| {
        pane.buffers[0].assist.findings[0]
            .rewrite
            .as_ref()
            .map(|rewrite| rewrite.streamed.clone())
    });
    assert_eq!(streamed.as_deref(), Some("in 20"));

    h.rewriter.complete(0, "in 2017");
    h.frame(cx);
    let suggestion = h.pane.read_with(cx, |pane, _| {
        pane.buffers[0].assist.findings[0].suggestion.clone()
    });
    assert_eq!(suggestion.as_deref(), Some("in 2017"));

    h.click("oyl-card-apply", cx);
    assert_eq!(h.text(cx), "The method was first proposed in 2017.\n");
    assert_eq!(h.pane.read_with(cx, |pane, _| pane.findings_count()), 0);
}

#[gpui::test]
fn hovering_a_finding_opens_its_card_and_leaving_closes_it(cx: &mut TestAppContext) {
    let text = "It was shown in 1998 by many groups of people.\n";
    let h = Harness::open(cx, "main.typ", text);
    h.select(0..46, cx);
    h.keys("cmd-i", cx);
    h.checker
        .send(0, finding("1998", FindingLevel::Incorrect, Some("2014")));
    h.checker.send(0, FactCheckEvent::Done { claims: 1 });
    h.select(0..0, cx);
    assert!(!h.card_open(cx));

    let year = h.bounds_of(range_of(text, "1998"), cx);
    h.visual(cx)
        .simulate_mouse_move(year.center(), None, Modifiers::default());
    cx.executor().advance_clock(Duration::from_millis(200));
    cx.run_until_parked();
    h.frame(cx);
    assert!(h.card_open(cx));
    assert!(h.on_screen("oyl-fact-card", cx));

    // Over other words, the card closes after a moment.
    let other = h.bounds_of(range_of(text, "people"), cx);
    h.visual(cx)
        .simulate_mouse_move(other.center(), None, Modifiers::default());
    cx.executor().advance_clock(Duration::from_millis(200));
    cx.run_until_parked();
    assert!(h.card_open(cx), "not at once");
    cx.executor().advance_clock(Duration::from_millis(400));
    cx.run_until_parked();
    assert!(!h.card_open(cx));
}

#[gpui::test]
fn findings_follow_edits_and_vanish_when_their_text_is_edited(cx: &mut TestAppContext) {
    let text = "Shown in 1998 and again in 2001.\n";
    let h = Harness::open(cx, "main.typ", text);
    h.select(0..32, cx);
    h.keys("cmd-i", cx);
    h.checker
        .send(0, finding("1998", FindingLevel::Incorrect, Some("2014")));
    h.checker
        .send(0, finding("2001", FindingLevel::Questionable, None));
    h.checker.send(0, FactCheckEvent::Done { claims: 2 });
    h.frame(cx);
    assert_eq!(h.findings(cx).len(), 2);

    // Typing before both moves them.
    h.select(0..0, cx);
    h.type_text("Once ", cx);
    let ranges = h.pane.read_with(cx, |pane, _| {
        pane.buffers[0]
            .assist
            .findings
            .iter()
            .map(|finding| finding.range.clone())
            .collect::<Vec<_>>()
    });
    let moved = h.text(cx);
    assert_eq!(&moved[ranges[0].clone()], "1998");
    assert_eq!(&moved[ranges[1].clone()], "2001");

    // Editing one's text removes just that one.
    h.select(ranges[1].start + 2..ranges[1].start + 2, cx);
    h.type_text("9", cx);
    assert_eq!(
        h.findings(cx),
        vec![("1998".to_string(), FindingLevel::Incorrect)]
    );
}

#[gpui::test]
fn no_findings_and_no_checker(cx: &mut TestAppContext) {
    let text = "Water boils at 100 °C at sea level.\n";
    let h = Harness::open(cx, "main.typ", text);
    h.select(0..5, cx);
    h.keys("cmd-i", cx);
    h.checker.send(0, FactCheckEvent::Done { claims: 1 });
    h.frame(cx);
    assert_eq!(
        h.notes(cx),
        vec!["No factual problems found · 1 claim checked".to_string()]
    );
    assert!(h.events.borrow().contains(&EditorEvent::FactCheckFinished {
        path: h.path.clone(),
        findings: 0,
        claims: 1,
    }));

    h.pane
        .update(cx, |pane, cx| pane.set_fact_checker(None, cx));
    h.keys("cmd-i", cx);
    assert_eq!(h.notes(cx), vec![NO_AI_FACTS.to_string()]);
}
