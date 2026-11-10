//! Try the quick tools from a terminal, with timings.
//!
//! ```text
//! cargo run -p oyl-agent --example quick -- polish [--model M] [--engine latex|typst] TEXT
//! cargo run -p oyl-agent --example quick -- factcheck [--model M] [--engine latex|typst] [--bib FILE] TEXT
//! ```

use std::{process::ExitCode, time::Instant};

use oyl_agent::{AgentEvent, FactCheckEvent, FactCheckRequest, Registry, RewriteRequest};
use oyl_model::Engine;

fn main() -> ExitCode {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let model = take(&mut args, "--model");
    let engine = match take(&mut args, "--engine").as_deref() {
        Some("latex") => Some(Engine::Latex),
        Some("typst") => Some(Engine::Typst),
        _ => None,
    };
    let bibliography = take(&mut args, "--bib")
        .map(|path| std::fs::read_to_string(path).unwrap_or_default())
        .unwrap_or_default();
    let [command, text] = args.as_slice() else {
        eprintln!(
            "usage: quick polish|factcheck [--model M] [--engine latex|typst] [--bib FILE] TEXT"
        );
        return ExitCode::FAILURE;
    };
    let registry = Registry::detect(std::env::temp_dir().join("oyl-quick-example"));
    let started = Instant::now();
    match command.as_str() {
        "polish" => {
            let Some(rewriter) = registry.rewriter() else {
                eprintln!("{}", registry.rewriter_status());
                return ExitCode::FAILURE;
            };
            let request = RewriteRequest {
                text: text.clone(),
                engine,
                model,
                ..RewriteRequest::default()
            };
            let handle = match rewriter.rewrite(request) {
                Ok(handle) => handle,
                Err(err) => {
                    eprintln!("{err:#}");
                    return ExitCode::FAILURE;
                }
            };
            let mut first = None;
            while let Ok(event) = handle.events.recv_blocking() {
                match event {
                    AgentEvent::TextDelta { .. } => {
                        first.get_or_insert(started.elapsed());
                    }
                    AgentEvent::Completed { summary, .. } => {
                        println!(
                            "first text after {:.2?}, done after {:.2?}",
                            first.unwrap_or_default(),
                            started.elapsed()
                        );
                        println!("IN : {text}\nOUT: {summary}");
                        return ExitCode::SUCCESS;
                    }
                    AgentEvent::Failed { message } => {
                        println!("FAILED after {:.2?}: {message}", started.elapsed());
                        return ExitCode::FAILURE;
                    }
                    _ => {}
                }
            }
            ExitCode::FAILURE
        }
        "factcheck" => {
            let Some(checker) = registry.fact_checker() else {
                eprintln!("{}", registry.fact_checker_status());
                return ExitCode::FAILURE;
            };
            let request = FactCheckRequest {
                text: text.clone(),
                engine,
                bibliography,
                model,
                ..FactCheckRequest::default()
            };
            let handle = match checker.check(request) {
                Ok(handle) => handle,
                Err(err) => {
                    eprintln!("{err:#}");
                    return ExitCode::FAILURE;
                }
            };
            while let Ok(event) = handle.events.recv_blocking() {
                println!("[{:>6.1}s] {event:?}", started.elapsed().as_secs_f64());
                if matches!(
                    event,
                    FactCheckEvent::Done { .. } | FactCheckEvent::Failed { .. }
                ) {
                    return ExitCode::SUCCESS;
                }
            }
            ExitCode::FAILURE
        }
        _ => ExitCode::FAILURE,
    }
}

fn take(args: &mut Vec<String>, flag: &str) -> Option<String> {
    let index = args.iter().position(|arg| arg == flag)?;
    if index + 1 >= args.len() {
        return None;
    }
    let value = args.remove(index + 1);
    args.remove(index);
    Some(value)
}
