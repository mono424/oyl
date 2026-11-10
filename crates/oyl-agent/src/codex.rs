//! The Codex harness: the `codex` CLI driven directly with `codex exec
//! --json`, in the workspace-write sandbox.
//!
//! The event schema follows `codex-rs/exec/src/exec_events.rs`
//! (`thread.started`, `turn.started`, `turn.completed`, `turn.failed`,
//! `item.started` / `item.updated` / `item.completed`, `error`; items
//! `agent_message`, `reasoning`, `command_execution`, `file_change`,
//! `mcp_tool_call`, `web_search`, `todo_list`, `error`). Unknown events and
//! fields are ignored.

use std::{
    collections::{HashMap, HashSet},
    ffi::OsString,
    path::PathBuf,
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

/// The model id that means "whatever Codex is configured to use".
pub const DEFAULT_MODEL: &str = "default";

/// Codex, driven directly.
pub struct CodexHarness {
    setup: Arc<Setup>,
}

impl CodexHarness {
    /// A harness that uses the `codex` found by `setup`.
    pub fn new(setup: Arc<Setup>) -> Self {
        Self { setup }
    }

    /// The models offered in the picker.
    pub fn models() -> Vec<ModelChoice> {
        [
            (DEFAULT_MODEL, "Default (your Codex setting)"),
            ("gpt-6.1-sol", "GPT-6.1 Sol"),
            ("gpt-6.1-terra", "GPT-6.1 Terra"),
            ("gpt-6-luna", "GPT-6 Luna (fastest)"),
        ]
        .into_iter()
        .map(|(id, label)| ModelChoice {
            id: id.into(),
            label: label.into(),
        })
        .collect()
    }
}

fn is_image(path: &std::path::Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| {
            matches!(
                ext.to_ascii_lowercase().as_str(),
                "png" | "jpg" | "jpeg" | "gif" | "webp"
            )
        })
}

/// The `codex` arguments for one turn (the prompt goes to stdin, `-`).
pub(crate) fn command_args(request: &TurnRequest) -> Vec<OsString> {
    let mut args: Vec<OsString> = [
        "exec",
        "--json",
        "--sandbox",
        "workspace-write",
        // Bibliography lookups (Crossref, arXiv) need the network.
        "-c",
        "sandbox_workspace_write.network_access=true",
        "--skip-git-repo-check",
    ]
    .into_iter()
    .map(OsString::from)
    .collect();
    if let Some(model) = request
        .model
        .as_deref()
        .filter(|m| !m.is_empty() && *m != DEFAULT_MODEL)
    {
        args.push("--model".into());
        args.push(model.into());
    }
    let session = request.session.as_deref().filter(|s| !s.is_empty());
    if let Some(session) = session {
        args.push("resume".into());
        push_images(&mut args, request);
        args.push(session.into());
    } else {
        push_images(&mut args, request);
    }
    args.push("-".into());
    args
}

fn push_images(args: &mut Vec<OsString>, request: &TurnRequest) {
    for image in request.attachments.iter().filter(|path| is_image(path)) {
        args.push("--image".into());
        args.push(image.into());
    }
}

impl Harness for CodexHarness {
    fn info(&self) -> HarnessInfo {
        let detection = self.setup.detection();
        let (readiness, status) = match &detection.codex {
            Some(codex) => (Readiness::Ready, codex.describe("codex")),
            None => (
                Readiness::Unavailable,
                "Install Codex (the codex command) to use this harness".into(),
            ),
        };
        HarnessInfo {
            id: HarnessId::codex(),
            name: "Codex".into(),
            tagline: "OpenAI's coding agent, driven directly".into(),
            readiness,
            status,
            models: Self::models(),
            default_model: Some(DEFAULT_MODEL.into()),
        }
    }

    fn start(&self, request: TurnRequest) -> anyhow::Result<TurnHandle> {
        let detection = self.setup.detection();
        process::begin("oyl-codex-turn", move |turn| {
            let Some(codex) = detection.codex.clone() else {
                turn.fail("Codex is not installed: the codex command was not found.");
                return;
            };
            if !request.project_root.is_dir() {
                turn.fail(format!(
                    "The paper folder {} does not exist.",
                    request.project_root.display()
                ));
                return;
            }
            let spawn = Spawn {
                program: codex.path,
                args: command_args(&request),
                cwd: request.project_root.clone(),
                env: detection.child_env(),
                stdin: prompt::full_prompt(&request).into_bytes(),
            };
            let mut parser =
                CodexParser::new(request.project_root.clone(), request.session.clone());
            turn.run_agent(&spawn, &mut parser, "Codex");
        })
    }
}

/// Maps `codex exec --json` lines to agent events.
pub(crate) struct CodexParser {
    root: PathBuf,
    session: Option<String>,
    started: HashSet<String>,
    /// Characters of each message / reasoning item already emitted.
    emitted: HashMap<String, usize>,
    text_emitted: bool,
    last_message: String,
    last_error: Option<String>,
    did_work: bool,
}

impl CodexParser {
    pub(crate) fn new(root: PathBuf, session: Option<String>) -> Self {
        Self {
            root,
            session: session.filter(|s| !s.is_empty()),
            started: HashSet::new(),
            emitted: HashMap::new(),
            text_emitted: false,
            last_message: String::new(),
            last_error: None,
            did_work: false,
        }
    }

    /// Emit the part of a growing text item not sent yet.
    fn text_item(&mut self, id: &str, text: &str, thinking: bool, out: &mut Vec<AgentEvent>) {
        let sent = self.emitted.get(id).copied().unwrap_or(0);
        let new = match text.get(sent..) {
            Some(rest) if sent <= text.len() => rest,
            _ => text,
        };
        if new.is_empty() {
            return;
        }
        self.did_work = true;
        if thinking {
            let text = if sent == 0 && self.emitted.values().any(|n| *n > 0) {
                format!("\n\n{new}")
            } else {
                new.to_owned()
            };
            out.push(AgentEvent::ThinkingDelta { text });
        } else {
            if sent == 0 && self.text_emitted {
                out.push(AgentEvent::TextDelta {
                    text: "\n\n".into(),
                });
            }
            self.text_emitted = true;
            out.push(AgentEvent::TextDelta {
                text: new.to_owned(),
            });
            self.last_message = text.to_owned();
        }
        self.emitted.insert(id.to_owned(), text.len());
    }

    fn tool_started(&mut self, id: &str, name: &str, input: &Value, out: &mut Vec<AgentEvent>) {
        if !self.started.insert(id.to_owned()) {
            return;
        }
        self.did_work = true;
        let view = tools::describe(name, input, &self.root);
        out.push(AgentEvent::ToolStarted {
            id: id.to_owned(),
            name: name.to_owned(),
            title: view.title,
            detail: view.detail,
        });
    }

    fn item(&mut self, phase: &str, item: &Value, out: &mut Vec<AgentEvent>) {
        let id = item
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or("item")
            .to_owned();
        let kind = item
            .get("type")
            .or_else(|| item.get("item_type"))
            .and_then(Value::as_str)
            .unwrap_or_default();
        let completed = phase == "item.completed";
        let status = item
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or_default();
        match kind {
            "agent_message" | "assistant_message" => {
                if let Some(text) = item.get("text").and_then(Value::as_str) {
                    self.text_item(&id, text, false, out);
                }
            }
            "reasoning" => {
                if let Some(text) = item.get("text").and_then(Value::as_str) {
                    self.text_item(&id, text, true, out);
                }
            }
            "command_execution" => {
                let command = item.get("command").cloned().unwrap_or(Value::Null);
                self.tool_started(
                    &id,
                    "command_execution",
                    &serde_json::json!({ "command": command }),
                    out,
                );
                if completed {
                    let exit_ok = item
                        .get("exit_code")
                        .and_then(Value::as_i64)
                        .is_none_or(|code| code == 0);
                    let output = item
                        .get("aggregated_output")
                        .and_then(Value::as_str)
                        .unwrap_or_default();
                    out.push(AgentEvent::ToolFinished {
                        id,
                        ok: status != "failed" && status != "declined" && exit_ok,
                        output: (!output.is_empty())
                            .then(|| tools::clip(output, tools::OUTPUT_CHARS)),
                    });
                }
            }
            "file_change" => {
                let changes = item
                    .get("changes")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default();
                let paths: Vec<PathBuf> = changes
                    .iter()
                    .filter_map(|change| change.get("path").and_then(Value::as_str))
                    .map(|path| tools::absolute(path, &self.root))
                    .collect();
                if !self.started.contains(&id) {
                    self.started.insert(id.clone());
                    self.did_work = true;
                    let title = match paths.as_slice() {
                        [one] => format!(
                            "Edit {}",
                            tools::display_path(&one.to_string_lossy(), &self.root)
                        ),
                        [] => "Edit files".to_owned(),
                        many => format!("Edit {} files", many.len()),
                    };
                    let detail = changes
                        .iter()
                        .map(|change| {
                            let kind = change
                                .get("kind")
                                .and_then(Value::as_str)
                                .unwrap_or("update");
                            let path = change.get("path").and_then(Value::as_str).unwrap_or("?");
                            format!("{kind} {path}")
                        })
                        .collect::<Vec<_>>()
                        .join("\n");
                    out.push(AgentEvent::ToolStarted {
                        id: id.clone(),
                        name: "file_change".into(),
                        title,
                        detail: (!detail.is_empty()).then_some(detail),
                    });
                }
                if completed {
                    let ok = status != "failed";
                    out.push(AgentEvent::ToolFinished {
                        id,
                        ok,
                        output: None,
                    });
                    if ok {
                        out.extend(
                            paths
                                .into_iter()
                                .map(|path| AgentEvent::FileChanged { path }),
                        );
                    }
                }
            }
            "mcp_tool_call" => {
                let server = item.get("server").and_then(Value::as_str).unwrap_or("mcp");
                let tool = item.get("tool").and_then(Value::as_str).unwrap_or("tool");
                if self.started.insert(id.clone()) {
                    self.did_work = true;
                    let arguments = item.get("arguments").map(Value::to_string);
                    out.push(AgentEvent::ToolStarted {
                        id: id.clone(),
                        name: format!("{server}.{tool}"),
                        title: format!("{server}: {tool}"),
                        detail: arguments.map(|a| tools::clip(&a, 4000)),
                    });
                }
                if completed {
                    let error = item.pointer("/error/message").and_then(Value::as_str);
                    let result = item.get("result").filter(|r| !r.is_null()).map(|r| {
                        r.get("content")
                            .map(tools::result_text)
                            .unwrap_or_else(|| r.to_string())
                    });
                    out.push(AgentEvent::ToolFinished {
                        id,
                        ok: status != "failed" && error.is_none(),
                        output: error
                            .map(str::to_owned)
                            .or(result)
                            .map(|o| tools::clip(&o, tools::OUTPUT_CHARS)),
                    });
                }
            }
            "web_search" => {
                let query = item.get("query").cloned().unwrap_or(Value::Null);
                self.tool_started(
                    &id,
                    "web_search",
                    &serde_json::json!({ "query": query }),
                    out,
                );
                if completed {
                    out.push(AgentEvent::ToolFinished {
                        id,
                        ok: true,
                        output: None,
                    });
                }
            }
            "todo_list" => {
                self.tool_started(&id, "todo_list", &Value::Null, out);
                if completed {
                    let items = item
                        .get("items")
                        .and_then(Value::as_array)
                        .cloned()
                        .unwrap_or_default();
                    let plan = items
                        .iter()
                        .map(|todo| {
                            let done = todo
                                .get("completed")
                                .and_then(Value::as_bool)
                                .unwrap_or(false);
                            let text = todo.get("text").and_then(Value::as_str).unwrap_or_default();
                            format!("[{}] {text}", if done { "x" } else { " " })
                        })
                        .collect::<Vec<_>>()
                        .join("\n");
                    out.push(AgentEvent::ToolFinished {
                        id,
                        ok: true,
                        output: (!plan.is_empty()).then_some(plan),
                    });
                }
            }
            "error" => {
                if let Some(message) = item.get("message").and_then(Value::as_str) {
                    out.push(AgentEvent::Status {
                        message: message.to_owned(),
                    });
                }
            }
            _ => {}
        }
    }
}

impl StreamParser for CodexParser {
    fn stdout_line(&mut self, line: &str, out: &mut Vec<AgentEvent>) {
        let Ok(value) = serde_json::from_str::<Value>(line) else {
            return;
        };
        match value
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or_default()
        {
            "thread.started" => {
                if let Some(id) = value
                    .get("thread_id")
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty())
                {
                    self.session = Some(id.to_owned());
                }
            }
            phase @ ("item.started" | "item.updated" | "item.completed") => {
                if let Some(item) = value.get("item") {
                    self.item(phase, item, out);
                }
            }
            "turn.completed" => {
                let usage = value.get("usage");
                let field = |name: &str| usage.and_then(|u| u.get(name)).and_then(Value::as_u64);
                if usage.is_some() {
                    out.push(AgentEvent::Usage {
                        cost_usd: None,
                        input_tokens: field("input_tokens"),
                        output_tokens: field("output_tokens"),
                    });
                }
                out.push(AgentEvent::Completed {
                    success: true,
                    summary: self.last_message.trim().to_owned(),
                    session: self.session.clone(),
                });
            }
            "turn.failed" => {
                let message = value
                    .pointer("/error/message")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
                    .or_else(|| self.last_error.clone())
                    .unwrap_or_else(|| "Codex could not finish the turn.".into());
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
            "error" => {
                // Stream errors may be retried by Codex; report and remember.
                if let Some(message) = value.get("message").and_then(Value::as_str) {
                    self.last_error = Some(message.to_owned());
                    out.push(AgentEvent::Status {
                        message: message.to_owned(),
                    });
                }
            }
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

    fn feed(lines: &[Value]) -> Vec<AgentEvent> {
        let mut parser = CodexParser::new(PathBuf::from("/papers/owls"), None);
        let mut out = Vec::new();
        for line in lines {
            parser.stdout_line(&line.to_string(), &mut out);
        }
        out
    }

    /// The documented sample (`codex exec --json`), extended with the item
    /// kinds of `exec_events.rs`.
    #[test]
    fn documented_events_map_in_order() {
        let events = feed(&[
            json!({"type":"thread.started","thread_id":"0199a213-81c0-7800-8aa1-bbab2a035a53"}),
            json!({"type":"turn.started"}),
            json!({"type":"item.completed","item":{"id":"item_0","type":"reasoning","text":"**Planning** the edit"}}),
            json!({"type":"item.started","item":{"id":"item_1","type":"command_execution","command":"bash -lc ls","aggregated_output":"","exit_code":null,"status":"in_progress"}}),
            json!({"type":"item.completed","item":{"id":"item_1","type":"command_execution","command":"bash -lc ls","aggregated_output":"main.typ\nrefs.bib\n","exit_code":0,"status":"completed"}}),
            json!({"type":"item.started","item":{"id":"item_2","type":"file_change","changes":[{"path":"main.typ","kind":"update"}],"status":"in_progress"}}),
            json!({"type":"item.completed","item":{"id":"item_2","type":"file_change","changes":[{"path":"main.typ","kind":"update"}],"status":"completed"}}),
            json!({"type":"item.completed","item":{"id":"item_3","type":"agent_message","text":"Added a sentence about owls."}}),
            json!({"type":"turn.completed","usage":{"input_tokens":24763,"cached_input_tokens":24448,"output_tokens":122,"reasoning_output_tokens":0}}),
        ]);
        let session = Some("0199a213-81c0-7800-8aa1-bbab2a035a53".to_owned());
        assert_eq!(
            events,
            vec![
                AgentEvent::ThinkingDelta {
                    text: "**Planning** the edit".into()
                },
                AgentEvent::ToolStarted {
                    id: "item_1".into(),
                    name: "command_execution".into(),
                    title: "Run ls".into(),
                    detail: Some("bash -lc ls".into()),
                },
                AgentEvent::ToolFinished {
                    id: "item_1".into(),
                    ok: true,
                    output: Some("main.typ\nrefs.bib\n".into())
                },
                AgentEvent::ToolStarted {
                    id: "item_2".into(),
                    name: "file_change".into(),
                    title: "Edit main.typ".into(),
                    detail: Some("update main.typ".into()),
                },
                AgentEvent::ToolFinished {
                    id: "item_2".into(),
                    ok: true,
                    output: None
                },
                AgentEvent::FileChanged {
                    path: PathBuf::from("/papers/owls/main.typ")
                },
                AgentEvent::TextDelta {
                    text: "Added a sentence about owls.".into()
                },
                AgentEvent::Usage {
                    cost_usd: None,
                    input_tokens: Some(24763),
                    output_tokens: Some(122)
                },
                AgentEvent::Completed {
                    success: true,
                    summary: "Added a sentence about owls.".into(),
                    session
                },
            ]
        );
    }

    #[test]
    fn failed_commands_and_growing_messages() {
        let events = feed(&[
            json!({"type":"thread.started","thread_id":"t"}),
            json!({"type":"item.completed","item":{"id":"c","type":"command_execution","command":["bash","-lc","latexmk main.tex"],"aggregated_output":"! error","exit_code":12,"status":"failed"}}),
            json!({"type":"item.updated","item":{"id":"m","type":"agent_message","text":"Hel"}}),
            json!({"type":"item.completed","item":{"id":"m","type":"agent_message","text":"Hello"}}),
            json!({"type":"item.completed","item":{"id":"m2","type":"agent_message","text":"Done."}}),
            json!({"type":"item.started","item":{"id":"todo","type":"todo_list","items":[{"text":"edit","completed":false}]}}),
            json!({"type":"item.completed","item":{"id":"todo","type":"todo_list","items":[{"text":"edit","completed":true}]}}),
        ]);
        assert!(
            matches!(&events[0], AgentEvent::ToolStarted { title, .. } if title == "Run latexmk")
        );
        assert_eq!(
            events[1],
            AgentEvent::ToolFinished {
                id: "c".into(),
                ok: false,
                output: Some("! error".into())
            }
        );
        assert_eq!(events[2], AgentEvent::TextDelta { text: "Hel".into() });
        assert_eq!(events[3], AgentEvent::TextDelta { text: "lo".into() });
        assert_eq!(
            events[4],
            AgentEvent::TextDelta {
                text: "\n\n".into()
            }
        );
        assert_eq!(
            events[5],
            AgentEvent::TextDelta {
                text: "Done.".into()
            }
        );
        assert!(
            matches!(&events[6], AgentEvent::ToolStarted { title, .. } if title == "Update the plan")
        );
        assert_eq!(
            events[7],
            AgentEvent::ToolFinished {
                id: "todo".into(),
                ok: true,
                output: Some("[x] edit".into())
            }
        );
    }

    #[test]
    fn turn_failed_before_work_is_a_failure() {
        let events = feed(&[
            json!({"type":"thread.started","thread_id":"t"}),
            json!({"type":"error","message":"stream disconnected; retrying"}),
            json!({"type":"turn.failed","error":{"message":"unexpected status 401 Unauthorized"}}),
        ]);
        assert_eq!(
            events[0],
            AgentEvent::Status {
                message: "stream disconnected; retrying".into()
            }
        );
        assert_eq!(
            events[1],
            AgentEvent::Failed {
                message: "unexpected status 401 Unauthorized".into()
            }
        );
    }

    #[test]
    fn mcp_and_web_search_items() {
        let events = feed(&[
            json!({"type":"item.started","item":{"id":"w","type":"web_search","query":"owl neck rotation"}}),
            json!({"type":"item.completed","item":{"id":"w","type":"web_search","query":"owl neck rotation"}}),
            json!({"type":"item.started","item":{"id":"x","type":"mcp_tool_call","server":"zotero","tool":"search","arguments":{"q":"owls"},"status":"in_progress"}}),
            json!({"type":"item.completed","item":{"id":"x","type":"mcp_tool_call","server":"zotero","tool":"search","arguments":{"q":"owls"},"result":{"content":[{"type":"text","text":"3 items"}]},"status":"completed"}}),
            json!({"type":"item.completed","item":{"id":"e","type":"error","message":"command timed out"}}),
        ]);
        assert!(
            matches!(&events[0], AgentEvent::ToolStarted { title, .. } if title == "Search the web for “owl neck rotation”")
        );
        assert_eq!(
            events[1],
            AgentEvent::ToolFinished {
                id: "w".into(),
                ok: true,
                output: None
            }
        );
        assert!(
            matches!(&events[2], AgentEvent::ToolStarted { title, name, .. } if title == "zotero: search" && name == "zotero.search")
        );
        assert_eq!(
            events[3],
            AgentEvent::ToolFinished {
                id: "x".into(),
                ok: true,
                output: Some("3 items".into())
            }
        );
        assert_eq!(
            events[4],
            AgentEvent::Status {
                message: "command timed out".into()
            }
        );
    }

    #[test]
    fn arguments_for_new_and_resumed_turns() {
        let mut request = TurnRequest {
            project_root: PathBuf::from("/papers/owls"),
            model: Some("gpt-6-luna".into()),
            attachments: vec![
                PathBuf::from("/tmp/plot.png"),
                PathBuf::from("/tmp/paper.pdf"),
            ],
            ..TurnRequest::default()
        };
        let text = |args: Vec<OsString>| {
            args.iter()
                .map(|a| a.to_string_lossy().into_owned())
                .collect::<Vec<_>>()
                .join(" ")
        };
        assert_eq!(
            text(command_args(&request)),
            "exec --json --sandbox workspace-write -c sandbox_workspace_write.network_access=true \
--skip-git-repo-check --model gpt-6-luna --image /tmp/plot.png -"
        );
        request.session = Some("0199a213".into());
        request.model = Some(DEFAULT_MODEL.into());
        assert_eq!(
            text(command_args(&request)),
            "exec --json --sandbox workspace-write -c sandbox_workspace_write.network_access=true \
--skip-git-repo-check resume --image /tmp/plot.png 0199a213 -"
        );
    }
}
