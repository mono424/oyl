//! Fact checks of a selection (⇧⌘B): the model lists the checkable claims,
//! verifies them on the web — the only tools it has — and answers with JSON
//! findings, each anchored to an exact quote of the selection.

use std::{
    ffi::OsString,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

use oyl_model::{
    Engine,
    agent::{
        AgentEvent, FactCheckEvent, FactCheckHandle, FactCheckRequest, FactChecker, FactFinding,
        FindingLevel, TurnHandle,
    },
};
use serde_json::Value;

use crate::{
    claude::ClaudeParser,
    codex::CodexParser,
    detect::Setup,
    guard::{cited_keys, missing_tokens},
    process::{self, Spawn},
    rewrite::{claude_model_label, scratch_dir},
    tools,
};

/// The model a Claude fact check uses unless the request names one.
pub const CLAUDE_FACT_CHECK_MODEL: &str = "sonnet";
/// How long a fact check may take before it is stopped.
pub const FACT_CHECK_TIMEOUT: Duration = Duration::from_secs(120);
/// The only tools a Claude fact check gets.
const WEB_TOOLS: [&str; 2] = ["WebSearch", "WebFetch"];
const CONTEXT_CHARS: usize = 800;
const BIB_CHARS: usize = 12_000;

/// The rules for the checker.
pub(crate) fn system_prompt(engine: Option<Engine>) -> String {
    let format = match engine {
        Some(Engine::Latex) => "LaTeX",
        Some(Engine::Typst) => "Typst",
        None => "LaTeX or Typst",
    };
    format!(
        "You are the fact checker of an academic paper written in {format}. You get one passage in <text>, \
the paper around it in <before> and <after> (context only, not to be checked), and in <bibliography> the \
entries of the works the passage cites (\\cite{{key}} or @key refers to the entry with that key).

1. Find the checkable factual claims in <text>: numbers, dates, names, attributions, quotations, causal \
or empirical claims, and what a cited work is said to show.
2. Verify them with web search and by reading reliable sources: primary sources, official data, the cited \
work itself, reviews. Be quick: a few searches, read only what you need.
3. Report only real problems. \"incorrect\": reliable sources contradict the claim. \"questionable\": the \
sources clearly cannot support the claim as written (unsupported, outdated, overstated, or the cited work \
does not say it). Never report style, wording, grammar or opinions. When sources agree, or you cannot \
tell, report nothing for that claim.

Answer with one JSON object and nothing else — no prose, no code fences:
{{\"claims\": <how many claims you checked>, \"findings\": [{{\"quote\": \"<the wrong part of <text>, \
copied character for character, as short as possible but unique>\", \"level\": \"incorrect\" or \
\"questionable\", \"problem\": \"<one or two plain sentences>\", \"suggestion\": \"<a drop-in replacement \
for quote in {format}, keeping its citations and math, or null>\", \"sources\": [\"<URL or cite key>\"]}}]}}
With nothing wrong, answer {{\"claims\": <n>, \"findings\": []}}."
    )
}

/// The message: the passage, its context and the cited bibliography entries.
pub(crate) fn user_message(request: &FactCheckRequest) -> String {
    let keys = cited_keys(&request.text, request.engine);
    let bibliography = bib_entries(&request.bibliography, &keys);
    format!(
        "<before>\n{}\n</before>\n\n<text>\n{}\n</text>\n\n<after>\n{}\n</after>\n\n<bibliography>\n{}\n</bibliography>\n\nCheck the claims in <text> and answer with the JSON object only.",
        tail(&request.before, CONTEXT_CHARS),
        request.text,
        head(&request.after, CONTEXT_CHARS),
        bibliography,
    )
}

fn tail(text: &str, max: usize) -> String {
    let chars: Vec<char> = text.chars().collect();
    chars[chars.len().saturating_sub(max)..].iter().collect()
}

fn head(text: &str, max: usize) -> String {
    text.chars().take(max).collect()
}

/// The BibTeX entries for `keys`, one after the other; a note for a key the
/// bibliography lacks.
pub(crate) fn bib_entries(bibliography: &str, keys: &[String]) -> String {
    if keys.is_empty() {
        return "(the passage cites nothing)".into();
    }
    let mut out = String::new();
    for key in keys {
        match find_entry(bibliography, key) {
            Some(entry) => {
                out.push_str(entry);
                out.push_str("\n\n");
            }
            None => out.push_str(&format!("(no entry for {key})\n\n")),
        }
    }
    tools::clip(out.trim_end(), BIB_CHARS)
}

fn find_entry<'a>(bibliography: &'a str, key: &str) -> Option<&'a str> {
    let mut search = 0;
    while let Some(offset) = bibliography[search..].find('@') {
        let at = search + offset;
        search = at + 1;
        let rest = &bibliography[at..];
        let Some(open) = rest.find(['{', '(']) else {
            break;
        };
        let entry_key = rest[open + 1..]
            .split(',')
            .next()
            .unwrap_or_default()
            .trim();
        if entry_key != key {
            continue;
        }
        let mut depth = 0usize;
        for (i, c) in rest.char_indices().skip(open) {
            match c {
                '{' | '(' => depth += 1,
                '}' | ')' => {
                    depth -= 1;
                    if depth == 0 {
                        return Some(&rest[..=i]);
                    }
                }
                _ => {}
            }
        }
        return Some(rest);
    }
    None
}

/// Read the checker's answer: the findings whose quote is really in `text`,
/// and how many claims were checked.
pub(crate) fn parse_report(
    raw: &str,
    text: &str,
    engine: Option<Engine>,
) -> Result<(Vec<FactFinding>, usize), String> {
    let body = unfence(raw);
    let value = serde_json::from_str::<Value>(body.trim())
        .ok()
        .or_else(|| span(&body, '{', '}').and_then(|s| serde_json::from_str(s).ok()))
        .or_else(|| span(&body, '[', ']').and_then(|s| serde_json::from_str(s).ok()));
    let (items, claims) = match value {
        Some(Value::Object(map)) => (
            map.get("findings")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default(),
            map.get("claims")
                .and_then(Value::as_u64)
                .map(|n| n as usize),
        ),
        Some(Value::Array(items)) => (items, None),
        _ => {
            // A cut-off answer: keep every finding that is complete.
            let items = complete_objects(&body);
            if items.is_empty() && !body.contains("\"findings\"") {
                return Err("The fact check did not answer in the expected form.".into());
            }
            (items, number_after(&body, "\"claims\""))
        }
    };
    let mut findings: Vec<FactFinding> = Vec::new();
    for item in &items {
        if let Some(finding) = finding_from(item, text, engine)
            && !findings.iter().any(|f| f.quote == finding.quote)
        {
            findings.push(finding);
        }
    }
    let claims = claims.unwrap_or(findings.len()).max(findings.len());
    Ok((findings, claims))
}

fn finding_from(item: &Value, text: &str, engine: Option<Engine>) -> Option<FactFinding> {
    let quote = item.get("quote").and_then(Value::as_str)?;
    let quote = [
        quote,
        quote.trim(),
        quote.trim().trim_matches(['"', '“', '”']),
    ]
    .into_iter()
    .find(|candidate| !candidate.is_empty() && text.contains(candidate))?;
    let level = match item
        .get("level")
        .and_then(Value::as_str)?
        .trim()
        .to_ascii_lowercase()
        .as_str()
    {
        "incorrect" | "wrong" | "false" => FindingLevel::Incorrect,
        "questionable" | "doubtful" | "unsupported" | "unclear" => FindingLevel::Questionable,
        _ => return None,
    };
    let problem = item.get("problem").and_then(Value::as_str)?.trim();
    if problem.is_empty() {
        return None;
    }
    // A suggestion that loses a citation or math of the quote is no drop-in.
    let suggestion = item
        .get("suggestion")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty() && *s != quote)
        .filter(|s| missing_tokens(quote, s, engine).is_empty())
        .map(str::to_owned);
    let sources = match item.get("sources") {
        Some(Value::Array(list)) => list
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_owned)
            .collect(),
        Some(Value::String(one)) => vec![one.clone()],
        _ => Vec::new(),
    };
    Some(FactFinding {
        quote: quote.to_owned(),
        level,
        problem: problem.to_owned(),
        suggestion,
        sources,
    })
}

/// The content of a fenced block if the answer has one, else the answer.
fn unfence(raw: &str) -> String {
    let trimmed = raw.trim();
    if let Some(start) = trimmed.find("```") {
        let after = &trimmed[start + 3..];
        let body = after.split_once('\n').map_or(after, |(_, rest)| rest);
        return body.split("```").next().unwrap_or(body).to_owned();
    }
    trimmed.to_owned()
}

fn span(text: &str, open: char, close: char) -> Option<&str> {
    let start = text.find(open)?;
    let end = text.rfind(close)?;
    (end > start).then(|| &text[start..=end])
}

/// Every complete `{…}` object inside the `findings` array of a cut-off
/// answer.
fn complete_objects(body: &str) -> Vec<Value> {
    let Some(start) = body.find("\"findings\"") else {
        return Vec::new();
    };
    let Some(open) = body[start..].find('[') else {
        return Vec::new();
    };
    let chars: Vec<(usize, char)> = body[start + open + 1..].char_indices().collect();
    let base = start + open + 1;
    let mut objects = Vec::new();
    let (mut depth, mut begin, mut in_string, mut escaped) = (0usize, None, false, false);
    for &(i, c) in &chars {
        if in_string {
            match (escaped, c) {
                (true, _) => escaped = false,
                (false, '\\') => escaped = true,
                (false, '"') => in_string = false,
                _ => {}
            }
            continue;
        }
        match c {
            '"' => in_string = true,
            '{' => {
                if depth == 0 {
                    begin = Some(i);
                }
                depth += 1;
            }
            '}' if depth > 0 => {
                depth -= 1;
                if depth == 0
                    && let Some(b) = begin.take()
                    && let Ok(value) = serde_json::from_str::<Value>(&body[base + b..base + i + 1])
                {
                    objects.push(value);
                }
            }
            ']' if depth == 0 => break,
            _ => {}
        }
    }
    objects
}

fn number_after(body: &str, key: &str) -> Option<usize> {
    let rest = &body[body.find(key)? + key.len()..];
    let digits: String = rest
        .trim_start_matches(|c: char| c == ':' || c.is_whitespace())
        .chars()
        .take_while(char::is_ascii_digit)
        .collect();
    digits.parse().ok()
}

/// The status line for a tool the checker uses.
fn status_for(name: &str, detail: Option<&str>) -> Option<String> {
    let input: Value = detail
        .and_then(|d| serde_json::from_str(d).ok())
        .unwrap_or(Value::Null);
    match name {
        "WebSearch" | "web_search" => {
            let query = input
                .get("query")
                .and_then(Value::as_str)
                .unwrap_or("the web");
            Some(format!("Searching: {}…", tools::clip(query, 80)))
        }
        "WebFetch" | "web_fetch" => {
            let url = input.get("url").and_then(Value::as_str).unwrap_or_default();
            let host = url.split_once("://").map_or(url, |(_, rest)| rest);
            let host = host.split(['/', '?', '#']).next().unwrap_or(host);
            Some(format!("Reading: {host}…"))
        }
        _ => None,
    }
}

/// Turn the events of a model run into a fact check's: status lines while
/// it searches and reads, then the findings and `Done`, or `Failed`. Stops
/// the run after `timeout`.
pub(crate) fn fact_check_events(
    inner: TurnHandle,
    request: FactCheckRequest,
    timeout: Duration,
) -> anyhow::Result<FactCheckHandle> {
    let (tx, rx) = async_channel::unbounded();
    let canceller = inner.canceller();
    let finished = Arc::new(AtomicBool::new(false));
    let timed_out = Arc::new(AtomicBool::new(false));
    {
        let (finished, timed_out, canceller) = (
            Arc::clone(&finished),
            Arc::clone(&timed_out),
            inner.canceller(),
        );
        std::thread::Builder::new()
            .name("oyl-fact-check-timer".into())
            .spawn(move || {
                let started = Instant::now();
                while !finished.load(Ordering::SeqCst) {
                    if started.elapsed() > timeout {
                        timed_out.store(true, Ordering::SeqCst);
                        canceller();
                        return;
                    }
                    std::thread::sleep(Duration::from_millis(100));
                }
            })?;
    }
    std::thread::Builder::new()
        .name("oyl-fact-check-events".into())
        .spawn(move || {
            let send = |event: FactCheckEvent| tx.send_blocking(event).is_ok();
            send(FactCheckEvent::Status {
                message: "Finding the claims to check…".into(),
            });
            let mut outcome = None;
            while let Ok(event) = inner.events.recv_blocking() {
                match event {
                    AgentEvent::ToolStarted { name, detail, .. } => {
                        if let Some(message) = status_for(&name, detail.as_deref()) {
                            send(FactCheckEvent::Status { message });
                        }
                    }
                    AgentEvent::Completed {
                        success: true,
                        summary,
                        ..
                    } => {
                        outcome = Some(
                            match parse_report(&summary, &request.text, request.engine) {
                                Ok((findings, claims)) => {
                                    for finding in findings {
                                        send(FactCheckEvent::Finding(finding));
                                    }
                                    FactCheckEvent::Done { claims }
                                }
                                Err(message) => FactCheckEvent::Failed { message },
                            },
                        );
                        break;
                    }
                    AgentEvent::Completed { summary, .. } => {
                        outcome = Some(FactCheckEvent::Failed { message: summary });
                        break;
                    }
                    AgentEvent::Failed { message } => {
                        outcome = Some(FactCheckEvent::Failed { message });
                        break;
                    }
                    _ => {}
                }
            }
            finished.store(true, Ordering::SeqCst);
            let last = if timed_out.load(Ordering::SeqCst) {
                FactCheckEvent::Failed {
                    message: format!(
                        "The fact check took longer than {} seconds and was stopped.",
                        timeout.as_secs()
                    ),
                }
            } else {
                outcome.unwrap_or(FactCheckEvent::Failed {
                    message: "The fact check ended without a result.".into(),
                })
            };
            send(last);
        })?;
    Ok(FactCheckHandle::new(rx, move || canceller()))
}

fn nothing_to_check() -> FactCheckHandle {
    let (tx, rx) = async_channel::unbounded();
    let _ = tx.send_blocking(FactCheckEvent::Done { claims: 0 });
    FactCheckHandle::new(rx, || {})
}

/// Fact checks with the `claude` CLI, allowed to search and read the web
/// and nothing else.
pub struct ClaudeFactChecker {
    setup: Arc<Setup>,
}

impl ClaudeFactChecker {
    /// A checker that uses the `claude` found by `setup`.
    pub fn new(setup: Arc<Setup>) -> Self {
        Self { setup }
    }
}

/// The `claude` arguments of one fact check (the message goes to stdin).
pub(crate) fn claude_args(request: &FactCheckRequest, model: &str) -> Vec<OsString> {
    let mut args: Vec<OsString> = [
        "-p",
        "--output-format",
        "stream-json",
        "--verbose",
        "--tools",
    ]
    .into_iter()
    .map(OsString::from)
    .collect();
    args.extend(WEB_TOOLS.iter().map(OsString::from));
    args.push("--allowedTools".into());
    args.extend(WEB_TOOLS.iter().map(OsString::from));
    args.extend(
        [
            "--permission-prompts",
            "none",
            "--setting-sources",
            "project,local",
            "--strict-mcp-config",
            "--no-session-persistence",
            "--model",
            model,
        ]
        .into_iter()
        .map(OsString::from),
    );
    args.push("--system-prompt".into());
    args.push(system_prompt(request.engine).into());
    args
}

impl FactChecker for ClaudeFactChecker {
    fn name(&self) -> String {
        format!(
            "Claude Code ({}, web search)",
            claude_model_label(CLAUDE_FACT_CHECK_MODEL)
        )
    }

    fn check(&self, request: FactCheckRequest) -> anyhow::Result<FactCheckHandle> {
        if request.text.trim().is_empty() {
            return Ok(nothing_to_check());
        }
        let detection = self.setup.detection();
        let Some(claude) = detection.claude.clone() else {
            anyhow::bail!("Claude Code is not installed: the claude command was not found");
        };
        let model = request
            .model
            .clone()
            .filter(|m| !m.is_empty() && m != crate::claude::DEFAULT_MODEL)
            .unwrap_or_else(|| CLAUDE_FACT_CHECK_MODEL.into());
        let cwd = scratch_dir();
        let spawn = Spawn {
            program: claude.path,
            args: claude_args(&request, &model),
            cwd: cwd.clone(),
            env: detection.child_env(),
            stdin: user_message(&request).into_bytes(),
        };
        let inner = process::begin("oyl-fact-check", move |turn| {
            let mut parser = ClaudeParser::new(cwd, None);
            turn.run_agent(&spawn, &mut parser, "Claude Code");
        })?;
        fact_check_events(inner, request, FACT_CHECK_TIMEOUT)
    }
}

/// Fact checks with `codex exec`, read-only, with its web search on (best
/// effort; untested against a real Codex).
pub struct CodexFactChecker {
    setup: Arc<Setup>,
}

impl CodexFactChecker {
    /// A checker that uses the `codex` found by `setup`.
    pub fn new(setup: Arc<Setup>) -> Self {
        Self { setup }
    }
}

/// The `codex` arguments of one fact check (the prompt goes to stdin).
pub(crate) fn codex_args(model: Option<&str>) -> Vec<OsString> {
    let mut args: Vec<OsString> = [
        "exec",
        "--json",
        "--sandbox",
        "read-only",
        "--skip-git-repo-check",
        "-c",
        "tools.web_search=true",
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

impl FactChecker for CodexFactChecker {
    fn name(&self) -> String {
        "Codex (web search)".into()
    }

    fn check(&self, request: FactCheckRequest) -> anyhow::Result<FactCheckHandle> {
        if request.text.trim().is_empty() {
            return Ok(nothing_to_check());
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
        let prompt = format!(
            "{}\nUse web search only; do not run commands or read files.\n\n{}",
            system_prompt(request.engine),
            user_message(&request)
        );
        let spawn = Spawn {
            program: codex.path,
            args: codex_args(model.as_deref()),
            cwd: cwd.clone(),
            env: detection.child_env(),
            stdin: prompt.into_bytes(),
        };
        let inner = process::begin("oyl-fact-check", move |turn| {
            let mut parser = CodexParser::new(cwd, None);
            turn.run_agent(&spawn, &mut parser, "Codex");
        })?;
        fact_check_events(inner, request, FACT_CHECK_TIMEOUT)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const TEXT: &str = r"The Eiffel Tower was completed in 1887 and is 330 m tall \cite{eiffel}.";

    fn collect(handle: &FactCheckHandle) -> Vec<FactCheckEvent> {
        let mut events = Vec::new();
        while let Ok(event) = handle.events.recv_blocking() {
            events.push(event);
        }
        events
    }

    #[test]
    fn prompt_and_message() {
        let system = system_prompt(Some(Engine::Latex));
        assert!(
            system.contains("\"claims\"")
                && system.contains("character for character")
                && system.contains("Never report style")
        );
        let request = FactCheckRequest {
            text: TEXT.into(),
            engine: Some(Engine::Latex),
            bibliography: "@book{other, title={X}}\n\n@misc{eiffel,\n  title = {Tour {Eiffel}},\n  year = {1889}\n}\n".into(),
            ..FactCheckRequest::default()
        };
        let message = user_message(&request);
        assert!(message.contains(&format!("<text>\n{TEXT}\n</text>")));
        assert!(message.contains("@misc{eiffel,\n  title = {Tour {Eiffel}},\n  year = {1889}\n}"));
        assert!(!message.contains("@book{other"), "only cited entries");
        assert_eq!(bib_entries("", &["x".into()]), "(no entry for x)");
        assert_eq!(bib_entries("@a{b,}", &[]), "(the passage cites nothing)");
    }

    #[test]
    fn reports_are_parsed_and_quotes_checked() {
        let raw = r#"```json
{"claims": 3, "findings": [
  {"quote": "completed in 1887", "level": "incorrect", "problem": "It was completed in 1889.", "suggestion": "completed in 1889", "sources": ["https://www.toureiffel.paris/en"]},
  {"quote": "330 m tall \\cite{eiffel}", "level": "Questionable", "problem": "Height changed with antennas.", "suggestion": "about 330 m tall", "sources": "eiffel"},
  {"quote": "built by aliens", "level": "incorrect", "problem": "Not in the text.", "sources": []},
  {"quote": "completed in 1887", "level": "incorrect", "problem": "Duplicate.", "sources": []},
  {"quote": "is 330", "level": "stylistic", "problem": "Not a level."}
]}
```"#;
        let (findings, claims) = parse_report(raw, TEXT, Some(Engine::Latex)).expect("parse");
        assert_eq!(claims, 3);
        assert_eq!(findings.len(), 2);
        assert_eq!(findings[0].level, FindingLevel::Incorrect);
        assert_eq!(findings[0].suggestion.as_deref(), Some("completed in 1889"));
        assert_eq!(findings[1].level, FindingLevel::Questionable);
        assert_eq!(
            findings[1].suggestion, None,
            "the suggestion dropped the citation"
        );
        assert_eq!(findings[1].sources, vec!["eiffel".to_owned()]);
    }

    #[test]
    fn prose_around_json_and_cut_off_answers() {
        let wrapped = r#"Here is my check: {"claims": 2, "findings": []} Done."#;
        assert_eq!(
            parse_report(wrapped, TEXT, None).expect("parse"),
            (Vec::new(), 2)
        );
        let cut = r#"{"claims": 4, "findings": [{"quote": "1887", "level": "incorrect", "problem": "1889.", "suggestion": "1889", "sources": []}, {"quote": "330 m", "level": "questi"#;
        let (findings, claims) = parse_report(cut, TEXT, None).expect("partial");
        assert_eq!(claims, 4);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].quote, "1887");
        assert!(parse_report("I could not check this.", TEXT, None).is_err());
        let bare = r#"[{"quote": "330 m", "level": "questionable", "problem": "Antennas.", "sources": []}]"#;
        assert_eq!(parse_report(bare, TEXT, None).expect("array").1, 1);
    }

    #[test]
    fn events_map_to_status_findings_and_done() {
        let (tx, rx) = async_channel::unbounded();
        for event in [
            AgentEvent::ToolStarted {
                id: "1".into(),
                name: "WebSearch".into(),
                title: "Search".into(),
                detail: Some(json!({"query": "Eiffel Tower completion year"}).to_string()),
            },
            AgentEvent::ToolStarted {
                id: "2".into(),
                name: "WebFetch".into(),
                title: "Fetch".into(),
                detail: Some(json!({"url": "https://en.wikipedia.org/wiki/Eiffel_Tower", "prompt": "x"}).to_string()),
            },
            AgentEvent::TextDelta { text: "{".into() },
            AgentEvent::Completed {
                success: true,
                summary: r#"{"claims": 2, "findings": [{"quote": "1887", "level": "incorrect", "problem": "It opened in 1889.", "suggestion": "1889", "sources": ["https://en.wikipedia.org/wiki/Eiffel_Tower"]}]}"#.into(),
                session: None,
            },
        ] {
            let _ = tx.send_blocking(event);
        }
        let request = FactCheckRequest {
            text: TEXT.into(),
            ..FactCheckRequest::default()
        };
        let events = collect(
            &fact_check_events(TurnHandle::new(rx, || {}), request, Duration::from_secs(5))
                .expect("events"),
        );
        assert_eq!(
            events[0],
            FactCheckEvent::Status {
                message: "Finding the claims to check…".into()
            }
        );
        assert_eq!(
            events[1],
            FactCheckEvent::Status {
                message: "Searching: Eiffel Tower completion year…".into()
            }
        );
        assert_eq!(
            events[2],
            FactCheckEvent::Status {
                message: "Reading: en.wikipedia.org…".into()
            }
        );
        assert!(
            matches!(&events[3], FactCheckEvent::Finding(f) if f.quote == "1887" && f.level == FindingLevel::Incorrect)
        );
        assert_eq!(events[4], FactCheckEvent::Done { claims: 2 });
        assert_eq!(events.len(), 5);
    }

    #[test]
    fn a_slow_check_is_stopped_and_fails() {
        let (tx, rx) = async_channel::unbounded::<AgentEvent>();
        let cancelled = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&cancelled);
        let tx_stop = tx.clone();
        let inner = TurnHandle::new(rx, move || {
            flag.store(true, Ordering::SeqCst);
            let _ = tx_stop.send_blocking(AgentEvent::Completed {
                success: false,
                summary: "Stopped.".into(),
                session: None,
            });
        });
        let request = FactCheckRequest {
            text: TEXT.into(),
            ..FactCheckRequest::default()
        };
        let events = collect(
            &fact_check_events(inner, request, Duration::from_millis(200)).expect("events"),
        );
        assert!(cancelled.load(Ordering::SeqCst));
        assert!(
            matches!(events.last(), Some(FactCheckEvent::Failed { message }) if message.contains("longer than"))
        );
        drop(tx);
    }

    #[test]
    fn command_lines_allow_only_the_web() {
        let request = FactCheckRequest::default();
        let args: Vec<String> = claude_args(&request, "sonnet")
            .iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        let tools_at = args.iter().position(|a| a == "--tools").expect("--tools");
        assert_eq!(&args[tools_at + 1..tools_at + 3], ["WebSearch", "WebFetch"]);
        let allowed_at = args
            .iter()
            .position(|a| a == "--allowedTools")
            .expect("--allowedTools");
        assert_eq!(
            &args[allowed_at + 1..allowed_at + 3],
            ["WebSearch", "WebFetch"]
        );
        assert!(args.contains(&"--no-session-persistence".to_owned()));
        assert!(
            codex_args(None)
                .iter()
                .any(|a| a == "tools.web_search=true")
        );
    }
}
