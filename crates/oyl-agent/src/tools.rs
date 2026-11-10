//! Turning the tool calls of every harness into the short human lines the
//! chat shows ("Edit main.tex", "Run latexmk") and into file-change events.

use std::path::{Component, Path, PathBuf};

use serde_json::Value;

/// Characters of a tool's raw input kept in `ToolStarted::detail`.
const DETAIL_CHARS: usize = 4000;
/// Characters of a tool's output kept in `ToolFinished::output`.
pub(crate) const OUTPUT_CHARS: usize = 8000;

/// What the chat needs to know about one tool call.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ToolView {
    pub title: String,
    pub detail: Option<String>,
    /// Files the call writes when it succeeds (absolute).
    pub edits: Vec<PathBuf>,
}

/// Describe a tool call. `name` is the harness's tool name, `input` its
/// arguments, `root` the paper folder (relative paths are resolved there
/// and paths inside it are shown relative to it).
pub(crate) fn describe(name: &str, input: &Value, root: &Path) -> ToolView {
    let key = name.to_ascii_lowercase().replace(['-', ' '], "_");
    let path = string_field(input, &["file_path", "path", "notebook_path", "filename"]);
    let shown = path.map(|p| display_path(p, root));
    let edits_path = || path.map(|p| vec![absolute(p, root)]).unwrap_or_default();
    let mut edits = Vec::new();
    let title = match key.as_str() {
        "read" | "read_file" | "view" => titled("Read", shown.as_deref()),
        "edit" | "multiedit" | "multi_edit" | "str_replace" | "str_replace_editor"
        | "notebookedit" | "notebook_edit" => {
            edits = edits_path();
            titled("Edit", shown.as_deref())
        }
        "write" | "create" | "write_file" => {
            edits = edits_path();
            titled("Write", shown.as_deref())
        }
        "bash" | "shell" | "exec_command" | "command_execution" | "local_shell" => {
            match command_text(input) {
                Some(command) => command_title(&command),
                None => "Run a command".into(),
            }
        }
        "bash_job" => "Check a background job".into(),
        "run_commands_parallel" => "Run commands in parallel".into(),
        "glob" => titled("Find", string_field(input, &["pattern"])),
        "grep" => match string_field(input, &["pattern"]) {
            Some(pattern) => format!("Search for “{}”", clip(pattern, 60)),
            None => "Search the files".into(),
        },
        "ls" | "list_dir" => titled("List", shown.as_deref().or(Some("the folder"))),
        "webfetch" | "web_fetch" | "fetch" => match string_field(input, &["url"]) {
            Some(url) => format!("Fetch {}", host_of(url)),
            None => "Fetch a web page".into(),
        },
        "websearch" | "web_search" => match string_field(input, &["query"]) {
            Some(query) => format!("Search the web for “{}”", clip(query, 60)),
            None => "Search the web".into(),
        },
        "todowrite" | "todo_list" | "update_plan" => "Update the plan".into(),
        "task" | "agent" => match string_field(input, &["description"]) {
            Some(description) => format!("Sub-agent: {}", clip(description, 60)),
            None => "Run a sub-agent".into(),
        },
        "ask_user_question" | "askuserquestion" => "Ask a question".into(),
        "skill" => titled("Use skill", string_field(input, &["name", "skill"])),
        "summary" => "Summarize progress".into(),
        _ => name.to_owned(),
    };
    ToolView {
        title,
        detail: detail_of(input),
        edits,
    }
}

fn titled(verb: &str, object: Option<&str>) -> String {
    match object {
        Some(object) if !object.is_empty() => format!("{verb} {object}"),
        _ => verb.to_owned(),
    }
}

fn string_field<'a>(input: &'a Value, keys: &[&str]) -> Option<&'a str> {
    keys.iter()
        .find_map(|key| input.get(key).and_then(Value::as_str))
        .filter(|value| !value.trim().is_empty())
}

/// The command of a shell tool call, as one string.
pub(crate) fn command_text(input: &Value) -> Option<String> {
    match input.get("command").or_else(|| input.get("cmd"))? {
        Value::String(command) => Some(command.clone()),
        Value::Array(parts) => {
            let parts: Vec<&str> = parts.iter().filter_map(Value::as_str).collect();
            (!parts.is_empty()).then(|| parts.join(" "))
        }
        _ => None,
    }
}

fn detail_of(input: &Value) -> Option<String> {
    if let Some(command) = command_text(input) {
        return Some(clip(&command, DETAIL_CHARS));
    }
    match input {
        Value::Null => None,
        Value::Object(map) if map.is_empty() => None,
        other => serde_json::to_string(other)
            .ok()
            .map(|text| clip(&text, DETAIL_CHARS)),
    }
}

/// "Run latexmk" for `cd paper && latexmk -pdf main.tex`.
pub(crate) fn command_title(command: &str) -> String {
    match program_of(command) {
        Some(program) => format!("Run {program}"),
        None => "Run a command".into(),
    }
}

/// The program a shell command line is about: shell wrappers
/// (`bash -lc '…'`), `cd … &&` prefixes, variable assignments and `env`,
/// `sudo`, `time`, … are looked through; the path is reduced to its name.
pub(crate) fn program_of(command: &str) -> Option<String> {
    let mut script = command.trim().to_owned();
    // Unwrap `bash -lc '<script>'` (Codex runs every command like that).
    for _ in 0..3 {
        let words = split_words(&script);
        let first = words.first()?;
        let shell = basename(first);
        if matches!(shell.as_str(), "bash" | "sh" | "zsh" | "dash")
            && let Some(position) = words
                .iter()
                .position(|w| w.starts_with('-') && w.contains('c'))
            && let Some(inner) = words.get(position + 1)
        {
            script = inner.clone();
            continue;
        }
        break;
    }
    for segment in split_segments(&script) {
        let words = split_words(segment);
        let mut iter = words.iter().peekable();
        while let Some(word) = iter.peek() {
            let is_assignment =
                word.contains('=') && !word.starts_with('=') && !word.starts_with('-');
            if is_assignment
                || matches!(
                    word.as_str(),
                    "env" | "sudo" | "time" | "nohup" | "command" | "exec" | "nice"
                )
            {
                iter.next();
            } else {
                break;
            }
        }
        let Some(program) = iter.next() else { continue };
        let program = basename(program);
        if matches!(
            program.as_str(),
            "cd" | "pushd" | "popd" | "set" | "export" | "source" | "." | "true" | ":"
        ) {
            continue;
        }
        // Empty, or a flag left after a wrapper (`command -v typst` is a
        // lookup, not the program the line is about).
        if program.is_empty() || program.starts_with('-') {
            continue;
        }
        return Some(program);
    }
    None
}

/// Split a command line at `&&`, `||`, `;`, `|` and newlines (outside quotes).
fn split_segments(script: &str) -> Vec<&str> {
    let bytes = script.as_bytes();
    let mut segments = Vec::new();
    let (mut start, mut i) = (0, 0);
    let mut quote: Option<u8> = None;
    while i < bytes.len() {
        let byte = bytes[i];
        match quote {
            Some(q) if byte == q => quote = None,
            Some(_) => {}
            None if byte == b'\'' || byte == b'"' => quote = Some(byte),
            None if byte == b';' || byte == b'\n' || byte == b'|' || byte == b'&' => {
                segments.push(&script[start..i]);
                // Swallow the second character of `&&` / `||`.
                if i + 1 < bytes.len() && (bytes[i + 1] == b'&' || bytes[i + 1] == b'|') {
                    i += 1;
                }
                start = i + 1;
            }
            None => {}
        }
        i += 1;
    }
    segments.push(&script[start..]);
    segments
        .into_iter()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect()
}

/// Whitespace-separated words, with simple single/double quoting.
fn split_words(text: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut current = String::new();
    let mut quote: Option<char> = None;
    let mut in_word = false;
    for ch in text.chars() {
        match quote {
            Some(q) if ch == q => quote = None,
            Some(_) => current.push(ch),
            None if ch == '\'' || ch == '"' => {
                quote = Some(ch);
                in_word = true;
            }
            None if ch.is_whitespace() => {
                if in_word {
                    words.push(std::mem::take(&mut current));
                    in_word = false;
                }
            }
            None => {
                current.push(ch);
                in_word = true;
            }
        }
    }
    if in_word {
        words.push(current);
    }
    words
}

fn basename(word: &str) -> String {
    word.rsplit('/').next().unwrap_or(word).to_owned()
}

fn host_of(url: &str) -> String {
    let rest = url.split_once("://").map_or(url, |(_, rest)| rest);
    rest.split(['/', '?', '#'])
        .next()
        .unwrap_or(rest)
        .to_owned()
}

/// A path as the user reads it: relative to the paper folder when inside it.
pub(crate) fn display_path(path: &str, root: &Path) -> String {
    let absolute = absolute(path, root);
    match absolute.strip_prefix(root) {
        Ok(relative) if !relative.as_os_str().is_empty() => relative.display().to_string(),
        _ => crate::detect::display_home(&absolute),
    }
}

/// `path` made absolute against `root`, with `.` and `..` resolved
/// lexically (the file may not exist yet). A path the agent spelled through
/// the folder's canonical location (`/private/var/…` for `/var/…`) is
/// spelled through `root` again, so events name files the way Oyl does.
pub(crate) fn absolute(path: &str, root: &Path) -> PathBuf {
    let path = Path::new(path);
    let joined = if path.is_absolute() {
        path.to_path_buf()
    } else {
        root.join(path)
    };
    let mut normal = PathBuf::new();
    for component in joined.components() {
        match component {
            Component::ParentDir => {
                normal.pop();
            }
            Component::CurDir => {}
            other => normal.push(other.as_os_str()),
        }
    }
    if normal.starts_with(root) {
        return normal;
    }
    if let Ok(canonical) = std::fs::canonicalize(root)
        && canonical != root
        && let Ok(rest) = normal.strip_prefix(&canonical)
    {
        return root.join(rest);
    }
    normal
}

/// `text` cut to about `max` characters, keeping its start and end.
pub(crate) fn clip(text: &str, max: usize) -> String {
    let count = text.chars().count();
    if count <= max {
        return text.to_owned();
    }
    let half = max.saturating_sub(5) / 2;
    let head: String = text.chars().take(half).collect();
    let tail: String = text.chars().skip(count - half).collect();
    format!("{head} … {tail}")
}

/// The text of a tool result: a string, or the text parts of a content list.
pub(crate) fn result_text(content: &Value) -> String {
    match content {
        Value::String(text) => text.clone(),
        Value::Array(parts) => parts
            .iter()
            .filter_map(|part| match part {
                Value::String(text) => Some(text.as_str()),
                other => other.get("text").and_then(Value::as_str),
            })
            .collect::<Vec<_>>()
            .join("\n"),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn titles_for_file_tools() {
        let root = Path::new("/papers/owls");
        let view = describe(
            "Edit",
            &json!({"file_path": "/papers/owls/main.tex", "old_string": "a"}),
            root,
        );
        assert_eq!(view.title, "Edit main.tex");
        assert_eq!(view.edits, vec![PathBuf::from("/papers/owls/main.tex")]);
        let view = describe(
            "Write",
            &json!({"file_path": "sections/intro.typ", "content": "x"}),
            root,
        );
        assert_eq!(view.title, "Write sections/intro.typ");
        assert_eq!(
            view.edits,
            vec![PathBuf::from("/papers/owls/sections/intro.typ")]
        );
        let view = describe("Read", &json!({"file_path": "./refs.bib"}), root);
        assert_eq!(view.title, "Read refs.bib");
        assert!(view.edits.is_empty());
        assert_eq!(
            describe("Grep", &json!({"pattern": "cite"}), root).title,
            "Search for “cite”"
        );
        assert_eq!(
            describe(
                "WebFetch",
                &json!({"url": "https://api.crossref.org/works?q=x"}),
                root
            )
            .title,
            "Fetch api.crossref.org"
        );
        assert_eq!(describe("mystery", &json!({}), root).title, "mystery");
        assert_eq!(describe("mystery", &json!({}), root).detail, None);
    }

    #[test]
    fn titles_for_commands() {
        assert_eq!(command_title("latexmk -pdf main.tex"), "Run latexmk");
        assert_eq!(
            command_title("cd paper && /Library/TeX/texbin/latexmk -pdf main.tex"),
            "Run latexmk"
        );
        assert_eq!(
            command_title("TEXINPUTS=. pdflatex main.tex"),
            "Run pdflatex"
        );
        assert_eq!(
            command_title("bash -lc 'cd /x && typst compile main.typ'"),
            "Run typst"
        );
        assert_eq!(
            command_title("/bin/zsh -lc \"curl -s https://api.crossref.org/works\""),
            "Run curl"
        );
        assert_eq!(command_title("cat refs.bib | grep doi"), "Run cat");
        assert_eq!(
            command_title(
                "cd /p && command -v typst >/dev/null && typst compile main.typ; echo \"exit=$?\""
            ),
            "Run typst"
        );
        assert_eq!(command_title("cd x"), "Run a command");
        assert_eq!(command_title(""), "Run a command");
        let view = describe(
            "Bash",
            &json!({"command": "typst compile main.typ", "description": "Compile"}),
            Path::new("/p"),
        );
        assert_eq!(view.title, "Run typst");
        assert_eq!(view.detail.as_deref(), Some("typst compile main.typ"));
        let view = describe(
            "command_execution",
            &json!({"command": ["bash", "-lc", "ls figures"]}),
            Path::new("/p"),
        );
        assert_eq!(view.title, "Run ls");
    }

    #[test]
    fn paths_are_resolved_and_shown_relative() {
        let root = Path::new("/papers/owls");
        assert_eq!(
            absolute("../owls/./main.tex", root),
            PathBuf::from("/papers/owls/main.tex")
        );
        assert_eq!(
            display_path("/papers/owls/figures/a.png", root),
            "figures/a.png"
        );
        assert_eq!(display_path("/etc/hosts", root), "/etc/hosts");
    }

    #[test]
    fn canonical_spellings_map_back_to_the_folder() {
        let temp = tempfile::tempdir().expect("tempdir");
        let real = temp.path().join("real");
        std::fs::create_dir_all(&real).expect("real");
        let link = temp.path().join("link");
        std::os::unix::fs::symlink(&real, &link).expect("symlink");
        let canonical = std::fs::canonicalize(&real).expect("canonical");
        let reported = canonical.join("main.typ");
        assert_eq!(
            absolute(&reported.to_string_lossy(), &link),
            link.join("main.typ")
        );
        assert_eq!(display_path(&reported.to_string_lossy(), &link), "main.typ");
    }

    #[test]
    fn clipping_keeps_both_ends() {
        let text = "a".repeat(50) + &"b".repeat(50);
        let clipped = clip(&text, 21);
        assert!(clipped.starts_with("aaaa") && clipped.ends_with("bbbb"));
        assert!(clipped.chars().count() <= 21);
        assert_eq!(clip("short", 10), "short");
    }

    #[test]
    fn result_text_handles_content_lists() {
        assert_eq!(result_text(&json!("plain")), "plain");
        assert_eq!(
            result_text(
                &json!([{"type": "text", "text": "a"}, {"type": "image"}, {"type": "text", "text": "b"}])
            ),
            "a\nb"
        );
        assert_eq!(result_text(&Value::Null), "");
    }
}
