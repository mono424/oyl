//! The Claude Code harness: the `claude` CLI driven directly, in print mode
//! with the `stream-json` output format.

use std::{
    collections::{HashMap, HashSet},
    ffi::OsString,
    path::{Path, PathBuf},
    sync::Arc,
};

use oyl_model::agent::{
    AgentEvent, Harness, HarnessId, HarnessInfo, ModelChoice, Readiness, TurnHandle, TurnRequest,
};
use serde_json::Value;

use crate::{
    detect::Setup,
    process::{self, Spawn, StreamParser},
    prompt, tools,
};

/// The model id that means "whatever Claude Code is configured to use".
pub const DEFAULT_MODEL: &str = "default";

/// Tools Claude Code may use without asking: everything that reads, searches
/// and edits files in the paper folder (edits are accepted by the permission
/// mode), the typesetting tools, bibliography lookups, and harmless
/// file-system commands. Anything else is denied, never prompted for.
pub const ALLOWED_TOOLS: &[&str] = &[
    "Read",
    "Edit",
    "MultiEdit",
    "Write",
    "NotebookEdit",
    "Glob",
    "Grep",
    "LS",
    "TodoWrite",
    "Bash(typst *)",
    "Bash(latexmk *)",
    "Bash(pdflatex *)",
    "Bash(xelatex *)",
    "Bash(lualatex *)",
    "Bash(tectonic *)",
    "Bash(bibtex *)",
    "Bash(biber *)",
    "Bash(makeindex *)",
    "Bash(kpsewhich *)",
    "Bash(pdfinfo *)",
    "Bash(pdftotext *)",
    "Bash(curl *)",
    "Bash(ls *)",
    "Bash(cat *)",
    "Bash(head *)",
    "Bash(tail *)",
    "Bash(wc *)",
    "Bash(grep *)",
    "Bash(rg *)",
    "Bash(find *)",
    "Bash(file *)",
    "Bash(mkdir *)",
    "Bash(cp *)",
    "Bash(mv *)",
    "Bash(which *)",
    "WebFetch(domain:api.crossref.org)",
    "WebFetch(domain:doi.org)",
    "WebFetch(domain:export.arxiv.org)",
    "WebFetch(domain:arxiv.org)",
    "WebSearch",
];

/// Claude Code, driven directly.
pub struct ClaudeCodeHarness {
    setup: Arc<Setup>,
}

impl ClaudeCodeHarness {
    /// A harness that uses the `claude` found by `setup`.
    pub fn new(setup: Arc<Setup>) -> Self {
        Self { setup }
    }

    /// The models offered in the picker, newest first. Claude Code's own
    /// aliases, so they follow its updates (`fable` is Fable 5.2).
    pub fn models() -> Vec<ModelChoice> {
        [
            ("fable", "Fable 5.2"),
            ("opus", "Opus"),
            ("sonnet", "Sonnet"),
            ("haiku", "Haiku (fastest)"),
            (DEFAULT_MODEL, "Default (Claude Code's own choice)"),
        ]
        .into_iter()
        .map(|(id, label)| ModelChoice {
            id: id.into(),
            label: label.into(),
        })
        .collect()
    }
}

/// The reasoning efforts `claude --effort` takes, lowest first.
pub const CLAUDE_EFFORTS: [&str; 5] = ["low", "medium", "high", "xhigh", "max"];

/// The effort of a request when Claude Code knows it.
pub(crate) fn effort_of(request: &TurnRequest) -> Option<&str> {
    request
        .effort
        .as_deref()
        .filter(|effort| CLAUDE_EFFORTS.contains(effort))
}

/// The `claude` arguments for one turn (the message goes to stdin).
pub(crate) fn command_args(request: &TurnRequest) -> Vec<OsString> {
    let mut args: Vec<OsString> = [
        "-p",
        "--output-format",
        "stream-json",
        "--verbose",
        "--include-partial-messages",
        "--permission-mode",
        "acceptEdits",
        "--permission-prompts",
        "none",
        // The user's own hooks, plugins and permission rules stay out of a
        // Oyl turn; the paper folder's project settings still apply.
        "--setting-sources",
        "project,local",
        "--strict-mcp-config",
    ]
    .into_iter()
    .map(OsString::from)
    .collect();
    args.push("--append-system-prompt".into());
    args.push(
        prompt::writing_rules(prompt::engine_of(request), request.main_file.as_deref()).into(),
    );
    if let Some(model) = request
        .model
        .as_deref()
        .filter(|m| !m.is_empty() && *m != DEFAULT_MODEL)
    {
        args.push("--model".into());
        args.push(model.into());
    }
    if let Some(effort) = effort_of(request) {
        args.push("--effort".into());
        args.push(effort.into());
    }
    if let Some(session) = request.session.as_deref().filter(|s| !s.is_empty()) {
        args.push("--resume".into());
        args.push(session.into());
    }
    let mut extra_dirs: Vec<&Path> = request
        .attachments
        .iter()
        .filter(|path| !path.starts_with(&request.project_root))
        .filter_map(|path| path.parent())
        .collect();
    extra_dirs.sort();
    extra_dirs.dedup();
    for dir in extra_dirs {
        args.push("--add-dir".into());
        args.push(dir.into());
    }
    // Variadic, so last.
    args.push("--allowedTools".into());
    args.extend(ALLOWED_TOOLS.iter().map(OsString::from));
    args
}

impl Harness for ClaudeCodeHarness {
    fn info(&self) -> HarnessInfo {
        let detection = self.setup.detection();
        let (readiness, status) = match &detection.claude {
            Some(claude) => (Readiness::Ready, claude.describe("claude")),
            None => (
                Readiness::Unavailable,
                "Install Claude Code (the claude command) to use this harness".into(),
            ),
        };
        HarnessInfo {
            id: HarnessId::claude_code(),
            name: "Claude Code".into(),
            tagline: "Anthropic's coding agent, driven directly".into(),
            readiness,
            status,
            models: Self::models(),
            default_model: Some(DEFAULT_MODEL.into()),
            efforts: CLAUDE_EFFORTS.iter().map(|e| (*e).to_owned()).collect(),
            // None: the CLI's own default for the model.
            default_effort: None,
            capabilities: Vec::new(),
        }
    }

    fn start(&self, request: TurnRequest) -> anyhow::Result<TurnHandle> {
        let detection = self.setup.detection();
        process::begin("oyl-claude-turn", move |turn| {
            let Some(claude) = detection.claude.clone() else {
                turn.fail("Claude Code is not installed: the claude command was not found.");
                return;
            };
            if !request.project_root.is_dir() {
                turn.fail(format!(
                    "The paper folder {} does not exist.",
                    request.project_root.display()
                ));
                return;
            }
            if let Some(effort) = request.effort.as_deref()
                && effort_of(&request).is_none()
            {
                turn.status(format!(
                    "Claude Code has no \"{effort}\" effort; it runs at its default."
                ));
            }
            let spawn = Spawn {
                program: claude.path,
                args: command_args(&request),
                cwd: request.project_root.clone(),
                env: detection.child_env(),
                stdin: prompt::user_message(&request).into_bytes(),
            };
            let mut parser =
                ClaudeParser::new(request.project_root.clone(), request.session.clone());
            turn.run_agent(&spawn, &mut parser, "Claude Code");
        })
    }
}

/// Maps Claude Code's `stream-json` lines to agent events.
pub(crate) struct ClaudeParser {
    root: PathBuf,
    session: Option<String>,
    /// Tool calls by id: name and the files they write.
    tools: HashMap<String, (String, Vec<PathBuf>)>,
    /// Messages whose text arrived as partial deltas.
    streamed: HashSet<String>,
    current_message: Option<String>,
    text_emitted: bool,
    need_break: bool,
    did_work: bool,
}

impl ClaudeParser {
    pub(crate) fn new(root: PathBuf, session: Option<String>) -> Self {
        Self {
            root,
            session: session.filter(|s| !s.is_empty()),
            tools: HashMap::new(),
            streamed: HashSet::new(),
            current_message: None,
            text_emitted: false,
            need_break: false,
            did_work: false,
        }
    }

    fn text(&mut self, text: &str, out: &mut Vec<AgentEvent>) {
        if text.is_empty() {
            return;
        }
        if self.need_break && self.text_emitted {
            out.push(AgentEvent::TextDelta {
                text: "\n\n".into(),
            });
        }
        self.need_break = false;
        self.text_emitted = true;
        self.did_work = true;
        out.push(AgentEvent::TextDelta {
            text: text.to_owned(),
        });
    }

    fn stream_event(&mut self, event: &Value, out: &mut Vec<AgentEvent>) {
        match event.get("type").and_then(Value::as_str) {
            Some("message_start") => {
                self.current_message = event
                    .pointer("/message/id")
                    .and_then(Value::as_str)
                    .map(str::to_owned);
            }
            Some("content_block_start") => {
                if event.pointer("/content_block/type").and_then(Value::as_str) == Some("text") {
                    self.need_break = true;
                }
            }
            Some("content_block_delta") => {
                let delta = event.get("delta").unwrap_or(&Value::Null);
                match delta.get("type").and_then(Value::as_str) {
                    Some("text_delta") => {
                        if let Some(id) = &self.current_message {
                            self.streamed.insert(id.clone());
                        }
                        if let Some(text) = delta.get("text").and_then(Value::as_str) {
                            self.text(text, out);
                        }
                    }
                    Some("thinking_delta") => {
                        if let Some(id) = &self.current_message {
                            self.streamed.insert(id.clone());
                        }
                        if let Some(text) = delta
                            .get("thinking")
                            .and_then(Value::as_str)
                            .filter(|t| !t.is_empty())
                        {
                            out.push(AgentEvent::ThinkingDelta {
                                text: text.to_owned(),
                            });
                        }
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }

    fn assistant(&mut self, message: &Value, out: &mut Vec<AgentEvent>) {
        let id = message
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let streamed = self.streamed.contains(id);
        let Some(blocks) = message.get("content").and_then(Value::as_array) else {
            return;
        };
        for block in blocks {
            match block.get("type").and_then(Value::as_str) {
                Some("text") if !streamed => {
                    self.need_break = true;
                    if let Some(text) = block.get("text").and_then(Value::as_str) {
                        self.text(text, out);
                    }
                }
                Some("thinking") if !streamed => {
                    if let Some(text) = block
                        .get("thinking")
                        .and_then(Value::as_str)
                        .filter(|t| !t.is_empty())
                    {
                        out.push(AgentEvent::ThinkingDelta {
                            text: text.to_owned(),
                        });
                    }
                }
                Some("tool_use") => {
                    let Some(tool_id) = block.get("id").and_then(Value::as_str) else {
                        continue;
                    };
                    if self.tools.contains_key(tool_id) {
                        continue;
                    }
                    let name = block.get("name").and_then(Value::as_str).unwrap_or("tool");
                    let input = block.get("input").unwrap_or(&Value::Null);
                    let view = tools::describe(name, input, &self.root);
                    self.tools
                        .insert(tool_id.to_owned(), (name.to_owned(), view.edits));
                    self.did_work = true;
                    self.need_break = true;
                    out.push(AgentEvent::ToolStarted {
                        id: tool_id.to_owned(),
                        name: name.to_owned(),
                        title: view.title,
                        detail: view.detail,
                    });
                }
                _ => {}
            }
        }
    }

    fn user(&mut self, message: &Value, out: &mut Vec<AgentEvent>) {
        let Some(blocks) = message.get("content").and_then(Value::as_array) else {
            return;
        };
        for block in blocks {
            if block.get("type").and_then(Value::as_str) != Some("tool_result") {
                continue;
            }
            let Some(tool_id) = block.get("tool_use_id").and_then(Value::as_str) else {
                continue;
            };
            let ok = !block
                .get("is_error")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let output = tools::result_text(block.get("content").unwrap_or(&Value::Null));
            out.push(AgentEvent::ToolFinished {
                id: tool_id.to_owned(),
                ok,
                output: (!output.is_empty()).then(|| tools::clip(&output, tools::OUTPUT_CHARS)),
            });
            if ok && let Some((_, edits)) = self.tools.get(tool_id) {
                out.extend(
                    edits
                        .iter()
                        .map(|path| AgentEvent::FileChanged { path: path.clone() }),
                );
            }
        }
    }

    fn result(&mut self, value: &Value, out: &mut Vec<AgentEvent>) {
        let usage = value.get("usage");
        let field = |name: &str| usage.and_then(|u| u.get(name)).and_then(Value::as_u64);
        let input = [
            field("input_tokens"),
            field("cache_creation_input_tokens"),
            field("cache_read_input_tokens"),
        ]
        .into_iter()
        .flatten()
        .reduce(|a, b| a + b);
        out.push(AgentEvent::Usage {
            cost_usd: value.get("total_cost_usd").and_then(Value::as_f64),
            input_tokens: input,
            output_tokens: field("output_tokens"),
        });
        let is_error = value
            .get("is_error")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let subtype = value
            .get("subtype")
            .and_then(Value::as_str)
            .unwrap_or("success");
        let text = value
            .get("result")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .trim()
            .to_owned();
        let errors = value
            .get("errors")
            .and_then(Value::as_array)
            .map(|errors| {
                errors
                    .iter()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>()
                    .join("; ")
            })
            .unwrap_or_default();
        if subtype == "success" && !is_error {
            out.push(AgentEvent::Completed {
                success: true,
                summary: text,
                session: self.session.clone(),
            });
            return;
        }
        let message = [text.as_str(), errors.as_str()]
            .into_iter()
            .find(|m| !m.is_empty())
            .map(str::to_owned)
            .unwrap_or_else(|| format!("Claude Code stopped: {}", subtype.replace('_', " ")));
        if self.did_work {
            out.push(AgentEvent::Completed {
                success: false,
                summary: message,
                session: self.session.clone(),
            });
        } else {
            out.push(AgentEvent::Failed { message });
        }
    }
}

impl StreamParser for ClaudeParser {
    fn stdout_line(&mut self, line: &str, out: &mut Vec<AgentEvent>) {
        let Ok(value) = serde_json::from_str::<Value>(line) else {
            if !line.trim().is_empty() {
                log::debug!("claude: non-JSON output: {line}");
            }
            return;
        };
        if let Some(session) = value
            .get("session_id")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
        {
            self.session = Some(session.to_owned());
        }
        // Lines of a sub-agent (the Task tool) belong to that tool call.
        let nested = value
            .get("parent_tool_use_id")
            .is_some_and(|parent| !parent.is_null());
        match value.get("type").and_then(Value::as_str) {
            Some("stream_event") if !nested => {
                if let Some(event) = value.get("event") {
                    self.stream_event(event, out);
                }
            }
            Some("assistant") if !nested => {
                if let Some(message) = value.get("message") {
                    self.assistant(message, out);
                }
            }
            Some("user") if !nested => {
                if let Some(message) = value.get("message") {
                    self.user(message, out);
                }
            }
            Some("result") => self.result(&value, out),
            _ => {}
        }
    }

    fn session(&self) -> Option<String> {
        self.session.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn feed(parser: &mut ClaudeParser, lines: &[Value]) -> Vec<AgentEvent> {
        let mut out = Vec::new();
        for line in lines {
            parser.stdout_line(&line.to_string(), &mut out);
        }
        out
    }

    /// The shape of a real `claude -p --output-format stream-json --verbose
    /// --include-partial-messages` run (2.1.321), trimmed.
    fn sample_turn(root: &str) -> Vec<Value> {
        let s = "f91ec444-221d-4a63-be12-355bf0d42b5f";
        vec![
            json!({"type":"system","subtype":"hook_started","hook_name":"SessionStart:startup","session_id":s}),
            json!({"type":"system","subtype":"init","cwd":root,"session_id":s,"model":"claude-haiku-5-5","permissionMode":"acceptEdits"}),
            json!({"type":"stream_event","event":{"type":"message_start","message":{"id":"msg_1","role":"assistant","content":[]}},"session_id":s,"parent_tool_use_id":null}),
            json!({"type":"stream_event","event":{"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":""}},"session_id":s,"parent_tool_use_id":null}),
            json!({"type":"stream_event","event":{"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":""}},"session_id":s,"parent_tool_use_id":null}),
            json!({"type":"stream_event","event":{"type":"content_block_start","index":1,"content_block":{"type":"text","text":""}},"session_id":s,"parent_tool_use_id":null}),
            json!({"type":"stream_event","event":{"type":"content_block_delta","index":1,"delta":{"type":"text_delta","text":"Let me "}},"session_id":s,"parent_tool_use_id":null}),
            json!({"type":"stream_event","event":{"type":"content_block_delta","index":1,"delta":{"type":"text_delta","text":"look."}},"session_id":s,"parent_tool_use_id":null}),
            json!({"type":"assistant","message":{"id":"msg_1","content":[{"type":"text","text":"Let me look."}]},"session_id":s,"parent_tool_use_id":null}),
            json!({"type":"stream_event","event":{"type":"content_block_start","index":2,"content_block":{"type":"tool_use","id":"toolu_1","name":"Read","input":{}}},"session_id":s,"parent_tool_use_id":null}),
            json!({"type":"stream_event","event":{"type":"content_block_delta","index":2,"delta":{"type":"input_json_delta","partial_json":"{\"file_"}},"session_id":s,"parent_tool_use_id":null}),
            json!({"type":"assistant","message":{"id":"msg_1","content":[{"type":"tool_use","id":"toolu_1","name":"Read","input":{"file_path":"main.typ"}}]},"session_id":s,"parent_tool_use_id":null}),
            json!({"type":"rate_limit_event","rate_limit_info":{"status":"allowed"},"session_id":s}),
            json!({"type":"user","message":{"role":"user","content":[{"tool_use_id":"toolu_1","type":"tool_result","content":"1\t= My Paper"}]},"parent_tool_use_id":null,"session_id":s,"tool_use_result":{}}),
            json!({"type":"stream_event","event":{"type":"message_start","message":{"id":"msg_2","content":[]}},"session_id":s,"parent_tool_use_id":null}),
            json!({"type":"assistant","message":{"id":"msg_2","content":[{"type":"tool_use","id":"toolu_2","name":"Edit","input":{"file_path":"main.typ","old_string":"a","new_string":"b"}}]},"session_id":s,"parent_tool_use_id":null}),
            json!({"type":"system","subtype":"permission_denied","tool_name":"Bash","session_id":s}),
            json!({"type":"user","message":{"content":[{"tool_use_id":"toolu_2","type":"tool_result","content":"The file main.typ has been updated successfully."}]},"parent_tool_use_id":null,"session_id":s}),
            json!({"type":"user","message":{"content":[{"tool_use_id":"toolu_9","type":"tool_result","content":"nested"}]},"parent_tool_use_id":"toolu_task","session_id":s}),
            json!({"type":"stream_event","event":{"type":"message_start","message":{"id":"msg_3","content":[]}},"session_id":s,"parent_tool_use_id":null}),
            json!({"type":"stream_event","event":{"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}},"session_id":s,"parent_tool_use_id":null}),
            json!({"type":"stream_event","event":{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Added an owl sentence."}},"session_id":s,"parent_tool_use_id":null}),
            json!({"type":"assistant","message":{"id":"msg_3","content":[{"type":"text","text":"Added an owl sentence."}]},"session_id":s,"parent_tool_use_id":null}),
            json!({"type":"system","subtype":"post_turn_summary","status_detail":"added owl sentence","session_id":s}),
            json!({"type":"result","subtype":"success","is_error":false,"num_turns":3,"result":"Added an owl sentence.","session_id":s,"total_cost_usd":0.0141,"usage":{"input_tokens":20,"cache_creation_input_tokens":100,"cache_read_input_tokens":1000,"output_tokens":300}}),
        ]
    }

    #[test]
    fn a_real_turn_maps_to_events_in_order() {
        let root = "/papers/owls";
        let mut parser = ClaudeParser::new(PathBuf::from(root), None);
        let events = feed(&mut parser, &sample_turn(root));
        let session = Some("f91ec444-221d-4a63-be12-355bf0d42b5f".to_owned());
        assert_eq!(
            events,
            vec![
                AgentEvent::TextDelta {
                    text: "Let me ".into()
                },
                AgentEvent::TextDelta {
                    text: "look.".into()
                },
                AgentEvent::ToolStarted {
                    id: "toolu_1".into(),
                    name: "Read".into(),
                    title: "Read main.typ".into(),
                    detail: Some(r#"{"file_path":"main.typ"}"#.into()),
                },
                AgentEvent::ToolFinished {
                    id: "toolu_1".into(),
                    ok: true,
                    output: Some("1\t= My Paper".into())
                },
                AgentEvent::ToolStarted {
                    id: "toolu_2".into(),
                    name: "Edit".into(),
                    title: "Edit main.typ".into(),
                    detail: Some(
                        json!({"file_path":"main.typ","old_string":"a","new_string":"b"})
                            .to_string()
                    ),
                },
                AgentEvent::ToolFinished {
                    id: "toolu_2".into(),
                    ok: true,
                    output: Some("The file main.typ has been updated successfully.".into()),
                },
                AgentEvent::FileChanged {
                    path: PathBuf::from("/papers/owls/main.typ")
                },
                AgentEvent::TextDelta {
                    text: "\n\n".into()
                },
                AgentEvent::TextDelta {
                    text: "Added an owl sentence.".into()
                },
                AgentEvent::Usage {
                    cost_usd: Some(0.0141),
                    input_tokens: Some(1120),
                    output_tokens: Some(300)
                },
                AgentEvent::Completed {
                    success: true,
                    summary: "Added an owl sentence.".into(),
                    session
                },
            ]
        );
    }

    #[test]
    fn unstreamed_text_comes_from_the_assistant_message() {
        let mut parser = ClaudeParser::new(PathBuf::from("/p"), None);
        let events = feed(
            &mut parser,
            &[
                json!({"type":"assistant","message":{"id":"m1","content":[{"type":"text","text":"Hello"}]},"session_id":"s"}),
                json!({"type":"assistant","message":{"id":"m2","content":[{"type":"text","text":"Again"}]},"session_id":"s"}),
            ],
        );
        assert_eq!(
            events,
            vec![
                AgentEvent::TextDelta {
                    text: "Hello".into()
                },
                AgentEvent::TextDelta {
                    text: "\n\n".into()
                },
                AgentEvent::TextDelta {
                    text: "Again".into()
                },
            ]
        );
        assert_eq!(parser.session(), Some("s".into()));
    }

    #[test]
    fn errors_before_any_work_fail_the_turn() {
        let mut parser = ClaudeParser::new(PathBuf::from("/p"), Some("old".into()));
        let events = feed(
            &mut parser,
            &[
                json!({"type":"result","subtype":"success","is_error":true,"result":"Not logged in · Please run /login","session_id":"s2"}),
            ],
        );
        assert_eq!(
            events.last(),
            Some(&AgentEvent::Failed {
                message: "Not logged in · Please run /login".into()
            })
        );
    }

    #[test]
    fn errors_after_work_complete_unsuccessfully() {
        let mut parser = ClaudeParser::new(PathBuf::from("/p"), None);
        let events = feed(
            &mut parser,
            &[
                json!({"type":"assistant","message":{"id":"m","content":[{"type":"tool_use","id":"t","name":"Bash","input":{"command":"latexmk -pdf main.tex"}}]},"session_id":"s"}),
                json!({"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"t","is_error":true,"content":[{"type":"text","text":"denied"}]}]},"session_id":"s"}),
                json!({"type":"result","subtype":"error_max_turns","is_error":true,"session_id":"s"}),
            ],
        );
        assert!(
            matches!(&events[0], AgentEvent::ToolStarted { title, .. } if title == "Run latexmk")
        );
        assert_eq!(
            events[1],
            AgentEvent::ToolFinished {
                id: "t".into(),
                ok: false,
                output: Some("denied".into())
            }
        );
        assert_eq!(
            events.last(),
            Some(&AgentEvent::Completed {
                success: false,
                summary: "Claude Code stopped: error max turns".into(),
                session: Some("s".into())
            })
        );
    }

    #[test]
    fn arguments_carry_model_session_and_rules() {
        let request = TurnRequest {
            project_root: PathBuf::from("/papers/owls"),
            model: Some("haiku".into()),
            session: Some("abc".into()),
            attachments: vec![
                PathBuf::from("/Users/me/Desktop/plot.png"),
                PathBuf::from("/papers/owls/figures/a.png"),
            ],
            ..TurnRequest::default()
        };
        let args: Vec<String> = command_args(&request)
            .into_iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        let after = |flag: &str| {
            args.iter()
                .position(|a| a == flag)
                .map(|i| args[i + 1].clone())
        };
        assert_eq!(after("--model").as_deref(), Some("haiku"));
        assert_eq!(after("--resume").as_deref(), Some("abc"));
        assert_eq!(after("--output-format").as_deref(), Some("stream-json"));
        assert_eq!(after("--permission-mode").as_deref(), Some("acceptEdits"));
        assert_eq!(after("--add-dir").as_deref(), Some("/Users/me/Desktop"));
        assert_eq!(args.iter().filter(|a| *a == "--add-dir").count(), 1);
        assert!(after("--append-system-prompt").is_some_and(|rules| rules.contains("refs.bib")));
        assert_eq!(
            args.last().map(String::as_str),
            ALLOWED_TOOLS.last().copied()
        );
        let default = TurnRequest {
            model: Some(DEFAULT_MODEL.into()),
            ..TurnRequest::default()
        };
        assert!(!command_args(&default).iter().any(|a| a == "--model"));
        assert!(!command_args(&default).iter().any(|a| a == "--effort"));
    }

    #[test]
    fn efforts_map_to_the_effort_flag() {
        let with = |effort: &str| {
            let request = TurnRequest {
                effort: Some(effort.into()),
                ..TurnRequest::default()
            };
            let args: Vec<String> = command_args(&request)
                .into_iter()
                .map(|a| a.to_string_lossy().into_owned())
                .collect();
            args.iter()
                .position(|a| a == "--effort")
                .map(|i| args[i + 1].clone())
        };
        for effort in CLAUDE_EFFORTS {
            assert_eq!(with(effort).as_deref(), Some(effort));
        }
        assert_eq!(with("ultra"), None, "unknown efforts are left out");
        let models: Vec<String> = ClaudeCodeHarness::models()
            .into_iter()
            .map(|m| m.id)
            .collect();
        assert_eq!(models, ["fable", "opus", "sonnet", "haiku", DEFAULT_MODEL]);
    }
}
