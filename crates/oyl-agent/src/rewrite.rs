//! Quick rewrites of a selection (⌘B polish, fact-check fixes): one
//! tool-less model call, text in and text out, streamed — not an agent turn.
//!
//! The model's answer is cleaned (code fences, quotes, wrapper tags, a
//! "Here is…" preamble) and given the selection's own leading and trailing
//! whitespace; a rewrite that lost or changed a citation, reference, label or
//! math is refused rather than returned.

use std::{ffi::OsString, path::PathBuf, sync::Arc};

use oyl_model::{
    Engine,
    agent::{AgentEvent, RewriteKind, RewriteRequest, Rewriter, TurnHandle},
};

use crate::{
    claude::ClaudeParser,
    codex::CodexParser,
    detect::Setup,
    guard::missing_tokens,
    process::{self, Spawn},
};

/// The model a Claude rewrite uses unless the request names one: Sonnet,
/// for the quality of the prose (see the crate's measurements: Haiku is a
/// little faster but smooths less and slips more often).
pub const CLAUDE_REWRITE_MODEL: &str = "sonnet";
/// Characters of context kept on each side of the selection.
const CONTEXT_CHARS: usize = 1200;

/// The rules for the model, per kind of rewrite.
pub(crate) fn system_prompt(kind: RewriteKind, engine: Option<Engine>) -> String {
    let format = match engine {
        Some(Engine::Latex) => "LaTeX",
        Some(Engine::Typst) => "Typst",
        None => "LaTeX or Typst",
    };
    let task = match kind {
        RewriteKind::Polish => {
            "You are the copy editor of an academic paper. You get one passage of the paper in <text>, \
with a little of the surrounding paper in <before> and <after> for context only. Return the passage polished:

- Fix spelling, grammar, punctuation, agreement and article use.
- Improve the flow subtly where a sentence is clumsy. Keep the author's meaning, claims, terminology, \
voice, tense and register, and roughly the same length. Do not add content, examples, hedges or filler, \
and do not drop information."
        }
        RewriteKind::FactFix => {
            "You are the editor of an academic paper. You get one passage of the paper in <text>, with a \
little of the surrounding paper in <before> and <after> for context only, and in <guidance> a factual \
problem a fact check found in the passage, with its sources and the author's remarks. Return the passage \
rewritten so that it fixes that problem:

- Change only what the fix needs; keep everything else word for word.
- Follow the author's remarks in <guidance>. Use only facts the guidance supports; never invent numbers, \
dates, names or sources.
- Keep the author's voice, tense, register and terminology, and roughly the same length."
        }
    };
    format!(
        "{task}
- Keep the language of the passage. Never translate it.
- The paper is written in {format}. Keep every command, environment, citation (\\cite{{…}}, @key), \
reference (\\ref{{…}}, @label), label, math ($…$), comment and URL exactly as written, and keep the \
line breaks where they are.
- If nothing needs to change, return the passage exactly as it is.

Answer with the passage only: no quotes, no code fences, no tags, no preamble, no explanation."
    )
}

/// The user message: context, the passage and, for a fix, the guidance.
pub(crate) fn user_message(request: &RewriteRequest) -> String {
    let before = tail_chars(&request.before, CONTEXT_CHARS);
    let after = head_chars(&request.after, CONTEXT_CHARS);
    let mut message = format!(
        "<before>\n{before}\n</before>\n\n<text>\n{}\n</text>\n\n<after>\n{after}\n</after>\n",
        request.text
    );
    if request.kind == RewriteKind::FactFix {
        message.push_str(&format!(
            "\n<guidance>\n{}\n</guidance>\n",
            request.guidance.trim()
        ));
    }
    message.push_str("\nReturn the rewritten <text> only.");
    message
}

fn tail_chars(text: &str, max: usize) -> &str {
    let count = text.chars().count();
    if count <= max {
        return text;
    }
    let skip = text.char_indices().nth(count - max).map_or(0, |(i, _)| i);
    &text[skip..]
}

fn head_chars(text: &str, max: usize) -> &str {
    match text.char_indices().nth(max) {
        Some((i, _)) => &text[..i],
        None => text,
    }
}

/// Clean the model's answer and check it against the selection: the
/// rewritten text with the selection's own surrounding whitespace, or why
/// it cannot be used.
pub(crate) fn finalize(request: &RewriteRequest, raw: &str) -> Result<String, String> {
    let core = strip_wrapping(raw, &request.text);
    if core.is_empty() {
        return Err("The rewrite came back empty; your text is unchanged.".into());
    }
    let original = request.text.as_str();
    let lead = &original[..original.len() - original.trim_start().len()];
    let trail = &original[original.trim_end().len()..];
    let result = format!("{lead}{core}{trail}");
    let missing = missing_tokens(original, &result, request.engine);
    if let Some(first) = missing.first() {
        let more = match missing.len() {
            1 => String::new(),
            n => format!(" and {} more", n - 1),
        };
        return Err(format!(
            "The rewrite changed or dropped {first}{more}, so your text is unchanged."
        ));
    }
    Ok(result)
}

/// The answer without what models wrap text in: code fences, `<text>`
/// tags, a "Here is the revised text:" line, quotes the selection did not
/// have. Trimmed.
pub(crate) fn strip_wrapping(raw: &str, original: &str) -> String {
    let mut text = raw.trim().to_owned();
    for _ in 0..3 {
        let before = text.clone();
        if text.starts_with("```") {
            text = match text.split_once('\n') {
                Some((_, rest)) => rest.to_owned(),
                None => String::new(),
            };
            if let Some(stripped) = text.trim_end().strip_suffix("```") {
                text = stripped.to_owned();
            }
            text = text.trim().to_owned();
        }
        if let Some(rest) = text.strip_prefix("<text>") {
            text = rest
                .trim_end()
                .strip_suffix("</text>")
                .unwrap_or(rest)
                .trim()
                .to_owned();
        }
        if let Some((first, rest)) = text.split_once('\n') {
            let lower = first.trim().to_ascii_lowercase();
            let preamble = lower.ends_with(':')
                && [
                    "here",
                    "polished",
                    "revised",
                    "corrected",
                    "rewritten",
                    "the polished",
                    "the revised",
                ]
                .iter()
                .any(|start| lower.starts_with(start));
            if preamble {
                text = rest.trim().to_owned();
            }
        }
        let original = original.trim();
        for (open, close) in [('"', '"'), ('“', '”'), ('\'', '\''), ('«', '»'), ('„', '“')]
        {
            if text.chars().count() >= 2
                && text.starts_with(open)
                && text.ends_with(close)
                && !(original.starts_with(open) && original.ends_with(close))
            {
                let inner: String = {
                    let mut chars = text.chars();
                    chars.next();
                    chars.next_back();
                    chars.collect()
                };
                text = inner.trim().to_owned();
            }
        }
        if text == before {
            break;
        }
    }
    text
}

/// Forwards streamed text without what [`strip_wrapping`] removes at its
/// start (an opening fence, a `<text>` tag, a quote) and keeps the last few
/// characters back, so a closing fence or quote never flashes on screen.
#[derive(Debug)]
pub(crate) struct StreamCleaner {
    lead: String,
    original_quoted: bool,
    pending: String,
    decided: bool,
    sent: String,
}

/// Characters held back at the end of the stream.
const HOLD_BACK: usize = 8;

impl StreamCleaner {
    pub(crate) fn new(original: &str) -> Self {
        let trimmed = original.trim_start();
        Self {
            lead: original[..original.len() - trimmed.len()].to_owned(),
            original_quoted: trimmed.starts_with(['"', '“', '\'', '«', '„']),
            pending: String::new(),
            decided: false,
            sent: String::new(),
        }
    }

    /// The part of the stream that can be shown now.
    pub(crate) fn push(&mut self, text: &str) -> Option<String> {
        self.pending.push_str(text);
        if !self.decided {
            let start = self.pending.trim_start();
            let fence = start.starts_with("```") || "```".starts_with(start);
            if fence {
                match start.split_once('\n') {
                    Some((_, rest)) => self.pending = rest.to_owned(),
                    None => return None,
                }
            } else if start.chars().count() < 7 && !start.contains('\n') {
                return None;
            }
            let start = self.pending.trim_start();
            let mut start = start
                .strip_prefix("<text>")
                .unwrap_or(start)
                .trim_start()
                .to_owned();
            if !self.original_quoted && start.starts_with(['"', '“', '«', '„']) {
                start = start.chars().skip(1).collect();
            }
            self.pending = start;
            self.decided = true;
            self.pending.insert_str(0, &self.lead);
        }
        let count = self.pending.chars().count();
        if count <= HOLD_BACK {
            return None;
        }
        let split = self
            .pending
            .char_indices()
            .nth(count - HOLD_BACK)
            .map_or(self.pending.len(), |(i, _)| i);
        let ready: String = self.pending[..split].to_owned();
        self.pending.drain(..split);
        self.sent.push_str(&ready);
        Some(ready)
    }

    /// What is still to be shown once the cleaned final text is known, when
    /// what was shown so far is its beginning.
    pub(crate) fn rest(&self, final_text: &str) -> Option<String> {
        final_text
            .strip_prefix(self.sent.as_str())
            .filter(|rest| !rest.is_empty())
            .map(str::to_owned)
    }
}

/// Turn the events of a model run into those of a rewrite: cleaned
/// `TextDelta`s, then `Completed` with the checked text, or `Failed` (also
/// for a stopped run, so nothing replaces the selection).
pub(crate) fn rewrite_events(
    inner: TurnHandle,
    request: RewriteRequest,
) -> anyhow::Result<TurnHandle> {
    let (tx, rx) = async_channel::unbounded();
    let canceller = inner.canceller();
    std::thread::Builder::new()
        .name("oyl-rewrite-events".into())
        .spawn(move || {
            let send = |event: AgentEvent| tx.send_blocking(event).is_ok();
            let mut cleaner = StreamCleaner::new(&request.text);
            while let Ok(event) = inner.events.recv_blocking() {
                match event {
                    AgentEvent::TextDelta { text } => {
                        if let Some(text) = cleaner.push(&text) {
                            send(AgentEvent::TextDelta { text });
                        }
                    }
                    AgentEvent::Completed {
                        success: true,
                        summary,
                        ..
                    } => {
                        match finalize(&request, &summary) {
                            Ok(text) => {
                                if let Some(rest) = cleaner.rest(&text) {
                                    send(AgentEvent::TextDelta { text: rest });
                                }
                                send(AgentEvent::Completed {
                                    success: true,
                                    summary: text,
                                    session: None,
                                });
                            }
                            Err(message) => {
                                send(AgentEvent::Failed { message });
                            }
                        }
                        return;
                    }
                    AgentEvent::Completed { summary, .. } => {
                        send(AgentEvent::Failed { message: summary });
                        return;
                    }
                    AgentEvent::Failed { message } => {
                        send(AgentEvent::Failed { message });
                        return;
                    }
                    _ => {}
                }
            }
            send(AgentEvent::Failed {
                message: "The rewrite ended without a result.".into(),
            });
        })?;
    Ok(TurnHandle::new(rx, move || canceller()))
}

/// An answer that needs no model: the selection is blank.
fn unchanged(text: String) -> TurnHandle {
    let (tx, rx) = async_channel::unbounded();
    let _ = tx.send_blocking(AgentEvent::Completed {
        success: true,
        summary: text,
        session: None,
    });
    TurnHandle::new(rx, || {})
}

/// An empty folder to run one-shot CLIs in, so they pick up no project.
pub(crate) fn scratch_dir() -> PathBuf {
    let dir = std::env::temp_dir().join("oyl-quick");
    if let Err(err) = std::fs::create_dir_all(&dir) {
        log::warn!("could not create {}: {err}", dir.display());
        return std::env::temp_dir();
    }
    dir
}

/// A friendly name for a Claude model id.
pub(crate) fn claude_model_label(model: &str) -> String {
    match model {
        "sonnet" => "Sonnet".into(),
        "haiku" => "Haiku".into(),
        "opus" => "Opus".into(),
        "fable" => "Fable".into(),
        other => other.into(),
    }
}

/// Rewrites with the `claude` CLI: one turn, no tools, streamed.
pub struct ClaudeRewriter {
    setup: Arc<Setup>,
}

impl ClaudeRewriter {
    /// A rewriter that uses the `claude` found by `setup`.
    pub fn new(setup: Arc<Setup>) -> Self {
        Self { setup }
    }
}

/// The `claude` arguments of one rewrite (the message goes to stdin).
pub(crate) fn claude_args(request: &RewriteRequest, model: &str) -> Vec<OsString> {
    let mut args: Vec<OsString> = [
        "-p",
        "--output-format",
        "stream-json",
        "--verbose",
        "--include-partial-messages",
        // No tools at all: the answer is the one reply.
        "--tools",
        "",
        "--setting-sources",
        "project,local",
        "--strict-mcp-config",
        "--no-session-persistence",
        "--effort",
        "low",
        "--model",
        model,
    ]
    .into_iter()
    .map(OsString::from)
    .collect();
    args.push("--system-prompt".into());
    args.push(system_prompt(request.kind, request.engine).into());
    args
}

impl Rewriter for ClaudeRewriter {
    fn name(&self) -> String {
        format!("Claude Code ({})", claude_model_label(CLAUDE_REWRITE_MODEL))
    }

    fn rewrite(&self, request: RewriteRequest) -> anyhow::Result<TurnHandle> {
        if request.text.trim().is_empty() {
            return Ok(unchanged(request.text));
        }
        let detection = self.setup.detection();
        let Some(claude) = detection.claude.clone() else {
            anyhow::bail!("Claude Code is not installed: the claude command was not found");
        };
        let model = request
            .model
            .clone()
            .filter(|m| !m.is_empty() && m != crate::claude::DEFAULT_MODEL)
            .unwrap_or_else(|| CLAUDE_REWRITE_MODEL.into());
        let cwd = scratch_dir();
        let spawn = Spawn {
            program: claude.path,
            args: claude_args(&request, &model),
            cwd: cwd.clone(),
            env: detection.child_env(),
            stdin: user_message(&request).into_bytes(),
        };
        let inner = process::begin("oyl-rewrite", move |turn| {
            let mut parser = ClaudeParser::new(cwd, None);
            turn.run_agent(&spawn, &mut parser, "Claude Code");
        })?;
        rewrite_events(inner, request)
    }
}

/// Rewrites with `codex exec` in a read-only sandbox (best effort: Codex
/// cannot be run without tools, so it is asked not to use any).
pub struct CodexRewriter {
    setup: Arc<Setup>,
}

impl CodexRewriter {
    /// A rewriter that uses the `codex` found by `setup`.
    pub fn new(setup: Arc<Setup>) -> Self {
        Self { setup }
    }
}

/// The `codex` arguments of one rewrite (the prompt goes to stdin).
pub(crate) fn codex_args(model: Option<&str>) -> Vec<OsString> {
    let mut args: Vec<OsString> = [
        "exec",
        "--json",
        "--sandbox",
        "read-only",
        "--skip-git-repo-check",
        "-c",
        "model_reasoning_effort=\"low\"",
    ]
    .into_iter()
    .map(OsString::from)
    .collect();
    if let Some(model) = model {
        args.push("--model".into());
        args.push(model.into());
    }
    args.push("-".into());
    args
}

/// The single prompt Codex gets: the rules, then the message.
pub(crate) fn codex_prompt(request: &RewriteRequest) -> String {
    format!(
        "{}\n- Do not run any command or read any file: everything you need is below.\n\n{}",
        system_prompt(request.kind, request.engine),
        user_message(request)
    )
}

impl Rewriter for CodexRewriter {
    fn name(&self) -> String {
        "Codex".into()
    }

    fn rewrite(&self, request: RewriteRequest) -> anyhow::Result<TurnHandle> {
        if request.text.trim().is_empty() {
            return Ok(unchanged(request.text));
        }
        let detection = self.setup.detection();
        let Some(codex) = detection.codex.clone() else {
            anyhow::bail!("Codex is not installed: the codex command was not found");
        };
        let model = request
            .model
            .clone()
            .filter(|m| !m.is_empty() && m != crate::codex::DEFAULT_MODEL);
        let cwd = scratch_dir();
        let spawn = Spawn {
            program: codex.path,
            args: codex_args(model.as_deref()),
            cwd: cwd.clone(),
            env: detection.child_env(),
            stdin: codex_prompt(&request).into_bytes(),
        };
        let inner = process::begin("oyl-rewrite", move |turn| {
            let mut parser = CodexParser::new(cwd, None);
            turn.run_agent(&spawn, &mut parser, "Codex");
        })?;
        rewrite_events(inner, request)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::process::StreamParser;
    use serde_json::json;

    fn polish(text: &str, engine: Engine) -> RewriteRequest {
        RewriteRequest {
            kind: RewriteKind::Polish,
            text: text.into(),
            before: "Previous sentence.".into(),
            after: "Next sentence.".into(),
            engine: Some(engine),
            ..RewriteRequest::default()
        }
    }

    fn collect(handle: &TurnHandle) -> Vec<AgentEvent> {
        let mut events = Vec::new();
        while let Ok(event) = handle.events.recv_blocking() {
            events.push(event);
        }
        events
    }

    #[test]
    fn prompts_carry_the_rules_and_the_passage() {
        let system = system_prompt(RewriteKind::Polish, Some(Engine::Latex));
        for rule in [
            "spelling, grammar, punctuation, agreement and article use",
            "Never translate",
            "written in LaTeX",
            "\\cite{…}",
            "exactly as it is",
            "no code fences",
            "roughly the same length",
        ] {
            assert!(system.contains(rule), "missing {rule:?} in {system}");
        }
        let fix = system_prompt(RewriteKind::FactFix, Some(Engine::Typst));
        assert!(
            fix.contains("<guidance>")
                && fix.contains("written in Typst")
                && fix.contains("never invent")
        );

        let mut request = polish("Teh owl hunt at night.", Engine::Latex);
        request.before = "x".repeat(5000);
        let message = user_message(&request);
        assert!(message.contains("<text>\nTeh owl hunt at night.\n</text>"));
        assert!(message.contains("<after>\nNext sentence.\n</after>"));
        assert!(!message.contains("<guidance>"));
        assert!(message.len() < 2000, "context is clipped");
        request.kind = RewriteKind::FactFix;
        request.guidance = "The year is 1973, not 1937. Source: doi.org/x".into();
        assert!(
            user_message(&request)
                .contains("<guidance>\nThe year is 1973, not 1937. Source: doi.org/x\n</guidance>")
        );
    }

    #[test]
    fn answers_are_unwrapped_and_get_the_original_whitespace() {
        let request = polish("  Teh owl hunt at night.\n", Engine::Latex);
        for raw in [
            "The owl hunts at night.",
            "```\nThe owl hunts at night.\n```",
            "```latex\nThe owl hunts at night.\n```",
            "\"The owl hunts at night.\"",
            "“The owl hunts at night.”",
            "<text>\nThe owl hunts at night.\n</text>",
            "Here is the polished text:\n\nThe owl hunts at night.",
        ] {
            assert_eq!(
                finalize(&request, raw).as_deref(),
                Ok("  The owl hunts at night.\n"),
                "{raw:?}"
            );
        }
        let quoted = polish("\"Owls is great,\" she said.", Engine::Typst);
        assert_eq!(
            finalize(&quoted, "\"Owls are great,\" she said.").as_deref(),
            Ok("\"Owls are great,\" she said.")
        );
        assert!(finalize(&request, "  ``` ").is_err());
    }

    #[test]
    fn rewrites_that_touch_citations_or_math_are_refused() {
        let latex = polish(
            r"Owl hears at $f = 6\,$kHz \cite{konishi1973} (see \ref{fig:ear}).",
            Engine::Latex,
        );
        assert!(
            finalize(
                &latex,
                r"Owls hear at $f = 6\,$kHz \cite{konishi1973} (see \ref{fig:ear})."
            )
            .is_ok()
        );
        let dropped = finalize(&latex, r"Owls hear at $f = 6\,$kHz (see \ref{fig:ear}).")
            .expect_err("dropped cite");
        assert!(dropped.contains(r"\cite{konishi1973}"), "{dropped}");
        let changed = finalize(
            &latex,
            r"Owls hear at $f = 6$ kHz \cite{konishi1973} (see \ref{fig:ear}).",
        )
        .expect_err("math");
        assert!(changed.contains(r"$f = 6\,$"), "{changed}");
        let typst = polish("Owl hunt by sound @konishi1973 <sec:owl>.", Engine::Typst);
        assert!(finalize(&typst, "Owls hunt by sound @konishi1973 <sec:owl>.").is_ok());
        assert!(finalize(&typst, "Owls hunt by sound <sec:owl>.").is_err());
    }

    #[test]
    fn streaming_hides_wrapping() {
        let mut cleaner = StreamCleaner::new("  Teh owl hunt at night.");
        let mut shown = String::new();
        for piece in ["``", "`\nThe ow", "l hunts at", " night.", "\n```"] {
            if let Some(text) = cleaner.push(piece) {
                shown.push_str(&text);
            }
        }
        assert!(!shown.contains('`'), "{shown:?}");
        assert!(shown.starts_with("  The owl"), "{shown:?}");
        let request = polish("  Teh owl hunt at night.", Engine::Latex);
        let final_text = finalize(&request, "```\nThe owl hunts at night.\n```").expect("clean");
        let rest = cleaner.rest(&final_text).expect("rest");
        assert_eq!(format!("{shown}{rest}"), final_text);
    }

    /// A recorded `claude -p --output-format stream-json
    /// --include-partial-messages --tools ""` reply, trimmed.
    fn recorded(text_parts: &[&str], result: &str) -> Vec<String> {
        let s = "8f0c";
        let mut lines = vec![
            json!({"type":"system","subtype":"init","session_id":s,"tools":[],"model":"claude-sonnet-5-5"}),
            json!({"type":"stream_event","event":{"type":"message_start","message":{"id":"m1"}},"session_id":s,"parent_tool_use_id":null}),
            json!({"type":"stream_event","event":{"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}},"session_id":s,"parent_tool_use_id":null}),
        ];
        for part in text_parts {
            lines.push(json!({"type":"stream_event","event":{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":part}},"session_id":s,"parent_tool_use_id":null}));
        }
        lines.push(json!({"type":"assistant","message":{"id":"m1","content":[{"type":"text","text":text_parts.concat()}]},"session_id":s,"parent_tool_use_id":null}));
        lines.push(json!({"type":"result","subtype":"success","is_error":false,"result":result,"session_id":s,"total_cost_usd":0.004,"usage":{"input_tokens":900,"output_tokens":40}}));
        lines.into_iter().map(|line| line.to_string()).collect()
    }

    fn replay(lines: Vec<String>) -> TurnHandle {
        let mut parser = ClaudeParser::new(PathBuf::from("/tmp"), None);
        let (tx, rx) = async_channel::unbounded();
        let mut events = Vec::new();
        for line in lines {
            parser.stdout_line(&line, &mut events);
        }
        for event in events {
            let _ = tx.send_blocking(event);
        }
        TurnHandle::new(rx, || {})
    }

    #[test]
    fn a_recorded_reply_becomes_deltas_then_the_checked_text() {
        let request = polish("Owl hunt at night \\cite{a}.", Engine::Latex);
        let inner = replay(recorded(
            &["Owls hunt", " at night", " \\cite{a}."],
            "Owls hunt at night \\cite{a}.",
        ));
        let events = collect(&rewrite_events(inner, request).expect("events"));
        let streamed: String = events
            .iter()
            .filter_map(|e| match e {
                AgentEvent::TextDelta { text } => Some(text.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(streamed, "Owls hunt at night \\cite{a}.");
        assert_eq!(
            events.last(),
            Some(&AgentEvent::Completed {
                success: true,
                summary: "Owls hunt at night \\cite{a}.".into(),
                session: None
            })
        );
        assert!(events.iter().all(|e| matches!(
            e,
            AgentEvent::TextDelta { .. } | AgentEvent::Completed { .. }
        )));
    }

    #[test]
    fn a_reply_that_drops_a_citation_fails() {
        let request = polish("Owl hunt at night \\cite{a}.", Engine::Latex);
        let inner = replay(recorded(&["Owls hunt at night."], "Owls hunt at night."));
        let events = collect(&rewrite_events(inner, request).expect("events"));
        assert!(
            matches!(events.last(), Some(AgentEvent::Failed { message }) if message.contains("\\cite{a}"))
        );
    }

    #[test]
    fn a_stopped_run_fails_instead_of_completing() {
        let (tx, rx) = async_channel::unbounded();
        let _ = tx.send_blocking(AgentEvent::Completed {
            success: false,
            summary: "Stopped.".into(),
            session: None,
        });
        let events = collect(
            &rewrite_events(TurnHandle::new(rx, || {}), polish("x y z", Engine::Typst))
                .expect("events"),
        );
        assert_eq!(
            events,
            vec![AgentEvent::Failed {
                message: "Stopped.".into()
            }]
        );
    }

    #[test]
    fn blank_selections_need_no_model() {
        let temp = tempfile::tempdir().expect("tempdir");
        let rewriter = ClaudeRewriter::new(Setup::with_detection(
            temp.path(),
            crate::Detection::default(),
        ));
        let events = collect(
            &rewriter
                .rewrite(polish("  \n", Engine::Latex))
                .expect("rewrite"),
        );
        assert_eq!(
            events,
            vec![AgentEvent::Completed {
                success: true,
                summary: "  \n".into(),
                session: None
            }]
        );
        assert!(
            rewriter.rewrite(polish("text", Engine::Latex)).is_err(),
            "no claude installed"
        );
    }

    #[test]
    fn command_lines() {
        let request = polish("x", Engine::Latex);
        let args: Vec<String> = claude_args(&request, "haiku")
            .iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        let after = |flag: &str| {
            args.iter()
                .position(|a| a == flag)
                .map(|i| args[i + 1].clone())
        };
        assert_eq!(after("--tools").as_deref(), Some(""));
        assert_eq!(after("--model").as_deref(), Some("haiku"));
        assert_eq!(after("--output-format").as_deref(), Some("stream-json"));
        assert!(after("--system-prompt").is_some_and(|p| p.contains("copy editor")));
        assert!(args.contains(&"--no-session-persistence".to_owned()));
        let codex: Vec<String> = codex_args(Some("gpt-6-luna"))
            .iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            codex.join(" "),
            "exec --json --sandbox read-only --skip-git-repo-check -c model_reasoning_effort=\"low\" --model gpt-6-luna -"
        );
        assert!(codex_prompt(&request).contains("Do not run any command"));
    }
}
