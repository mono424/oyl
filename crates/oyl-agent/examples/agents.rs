//! Look at the agent side of Oyl from a terminal.
//!
//! ```text
//! cargo run -p oyl-agent --example agents -- [--data-dir DIR]
//!     list what is installed and the harnesses
//! cargo run -p oyl-agent --example agents -- [--data-dir DIR] run HARNESS FOLDER PROMPT [--model M] [--session S]
//!     run one turn and print its events
//! ```

use std::{path::PathBuf, process::ExitCode};

use oyl_agent::{AgentEvent, HarnessId, Registry, TurnRequest};

fn main() -> ExitCode {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let data_dir = take_flag(&mut args, "--data-dir")
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join("oyl-agent-example"));
    let model = take_flag(&mut args, "--model");
    let session = take_flag(&mut args, "--session");
    let registry = Registry::detect(&data_dir);

    if args.first().map(String::as_str) != Some("run") {
        let detection = registry.detection();
        println!("claude: {:?}", detection.claude);
        println!("codex:  {:?}", detection.codex);
        println!("uv:     {:?}", detection.uv);
        println!("keys:   {:?}", detection.api_keys);
        for info in registry.infos() {
            println!(
                "\n{} [{:?}] {}\n  {}\n  default model: {:?}\n  models: {:?}",
                info.name,
                info.readiness,
                info.id,
                info.status,
                info.default_model,
                info.models
                    .iter()
                    .map(|m| m.id.as_str())
                    .collect::<Vec<_>>()
            );
        }
        println!("\ndefault harness: {}", registry.default_id());
        return ExitCode::SUCCESS;
    }

    let [_, harness, folder, prompt] = args.as_slice() else {
        eprintln!(
            "usage: agents [--data-dir DIR] run HARNESS FOLDER PROMPT [--model M] [--session S]"
        );
        return ExitCode::FAILURE;
    };
    let Some(harness) = registry.get(&HarnessId::new(harness.as_str())) else {
        eprintln!("no harness {harness}");
        return ExitCode::FAILURE;
    };
    let root = PathBuf::from(folder);
    let main_file = ["main.typ", "main.tex"]
        .into_iter()
        .map(|name| root.join(name))
        .find(|p| p.is_file());
    let request = TurnRequest {
        project_root: root.clone(),
        main_file: main_file.clone(),
        active_file: main_file,
        prompt: prompt.clone(),
        model,
        session,
        ..TurnRequest::default()
    };
    let handle = match harness.start(request) {
        Ok(handle) => handle,
        Err(err) => {
            eprintln!("could not start: {err}");
            return ExitCode::FAILURE;
        }
    };
    let mut ok = false;
    while let Ok(event) = handle.events.recv_blocking() {
        match &event {
            AgentEvent::TextDelta { text } => print!("{text}"),
            AgentEvent::Completed { success, .. } => {
                ok = *success;
                println!("\n{event:?}");
            }
            other => println!("\n{other:?}"),
        }
    }
    if ok {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

fn take_flag(args: &mut Vec<String>, flag: &str) -> Option<String> {
    let index = args.iter().position(|arg| arg == flag)?;
    if index + 1 >= args.len() {
        return None;
    }
    let value = args.remove(index + 1);
    args.remove(index);
    Some(value)
}
