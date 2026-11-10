//! LaTeX, out of process: tectonic, latexmk or a bare engine, building into
//! `<root>/.oyl/build/` so the paper folder stays clean.

mod install;
mod log;
mod run;
pub(crate) mod synctex;
mod toolchain;

use std::collections::hash_map::DefaultHasher;
use std::fs;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use oyl_model::Diagnostic;
use oyl_model::typeset::{CompileOutput, CompileRequest, RenderedDocument};

pub(crate) use install::{install_tectonic, target_triple};
pub(crate) use toolchain::{LatexTool, detect};

use self::log::{LogContext, bib_files, finish, parse_blg, parse_log};
use self::run::{Finished, RunError};
use self::toolchain::{child_path, find_in, search_dirs};
use crate::paths::{absolute, relative_inside};
use crate::pdf::PdfDocument;

/// The whole compile, all passes included, must finish within this.
const TIMEOUT: Duration = Duration::from_secs(120);
/// Most engine passes the bare-engine runner makes.
const MAX_PASSES: usize = 4;

/// Which TeX engine the paper asks for (`% !TEX program = xelatex`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Program {
    Pdf,
    Xe,
    Lua,
}

impl Program {
    /// Reads the magic comment at the top of the main file.
    fn of(main_text: &str) -> Self {
        for line in main_text.lines().take(30) {
            let Some(comment) = line.trim_start().strip_prefix('%') else {
                continue;
            };
            let comment = comment.trim_start();
            let Some(rest) = comment
                .strip_prefix("!TEX")
                .or_else(|| comment.strip_prefix("!tex"))
            else {
                continue;
            };
            let Some((key, value)) = rest.split_once('=') else {
                continue;
            };
            let key = key.trim().to_ascii_lowercase();
            if key == "program" || key == "ts-program" {
                return match value.trim().to_ascii_lowercase().as_str() {
                    "xelatex" | "xetex" => Program::Xe,
                    "lualatex" | "luatex" | "lualatex-dev" => Program::Lua,
                    _ => Program::Pdf,
                };
            }
        }
        Program::Pdf
    }

    fn engine(self) -> &'static str {
        match self {
            Program::Pdf => "pdflatex",
            Program::Xe => "xelatex",
            Program::Lua => "lualatex",
        }
    }

    fn latexmk_flag(self) -> &'static str {
        match self {
            Program::Pdf => "-pdf",
            Program::Xe => "-pdfxe",
            Program::Lua => "-pdflua",
        }
    }
}

/// Everything one compile needs to know about the project.
struct Job<'a> {
    tool: &'a LatexTool,
    data_dir: &'a Path,
    root: PathBuf,
    main: PathBuf,
    /// The directory TeX runs in: the main file's folder.
    run_dir: PathBuf,
    /// `<root>/.oyl/build`, absolute.
    build: PathBuf,
    /// The build directory as passed to TeX (relative to `run_dir` when
    /// possible: TeX Live refuses to write to absolute paths by default).
    build_arg: PathBuf,
    /// File name of the main file without extension.
    name: String,
    program: Program,
    deadline: Instant,
}

impl Job<'_> {
    fn output(&self, extension: &str) -> PathBuf {
        self.build.join(format!("{}.{extension}", self.name))
    }

    /// A command for `program` with the environment every TeX tool gets.
    fn command(&self, program: &Path) -> Command {
        let mut command = Command::new(program);
        command
            .current_dir(&self.run_dir)
            .env("PATH", child_path(program, self.data_dir))
            // One message per log line: no wrapping at 79 columns.
            .env("max_print_line", "10000")
            .env("error_line", "254")
            .env("half_error_line", "238");
        if self.build_arg.is_absolute() {
            command.env("TEXMFOUTPUT", &self.build);
        }
        command
    }

    fn run(&self, command: Command) -> Result<Finished, Diagnostic> {
        run::run(command, self.deadline).map_err(|err| match err {
            RunError::TimedOut => {
                let mut diagnostic = Diagnostic::error(format!(
                    "LaTeX did not finish within {} seconds and was stopped.",
                    TIMEOUT.as_secs()
                ));
                diagnostic.file = Some(self.main.clone());
                diagnostic
                    .hints
                    .push("Look for an endless loop, or a package waiting for input.".into());
                diagnostic
            }
            RunError::Spawn(err) => Diagnostic::error(format!(
                "{} could not be started: {err}",
                self.tool.describe()
            )),
        })
    }
}

/// The outcome of the tool runs, before the outputs are read.
struct Runs {
    /// The last run's exit status was success.
    succeeded: bool,
    /// Messages that only exist on the console (tectonic, latexmk).
    console: String,
    /// The compile was cut short (timeout, missing tool).
    aborted: Option<Diagnostic>,
}

/// Compiles a LaTeX project from disk with the given tool.
pub(crate) fn compile(
    request: &CompileRequest,
    tool: &LatexTool,
    data_dir: &Path,
) -> CompileOutput {
    let start = Instant::now();
    let started_at = SystemTime::now();
    let root = absolute(&request.root);
    let main = absolute(&request.main);
    if !main.is_file() {
        let message = format!("The main file {} does not exist.", main.display());
        return CompileOutput::failed(vec![Diagnostic::error(message)], start.elapsed());
    }
    let run_dir = main
        .parent()
        .map_or_else(|| root.clone(), Path::to_path_buf);
    let build = root.join(".oyl").join("build");
    if let Err(err) = fs::create_dir_all(&build) {
        let message = format!(
            "The build folder {} cannot be created: {err}",
            build.display()
        );
        return CompileOutput::failed(vec![Diagnostic::error(message)], start.elapsed());
    }
    let build_arg = relative_inside(&run_dir, &build).unwrap_or_else(|| build.clone());
    let name = main
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or("main")
        .to_owned();
    let program = fs::read(&main)
        .map(|bytes| Program::of(&String::from_utf8_lossy(&bytes)))
        .unwrap_or(Program::Pdf);
    let job = Job {
        tool,
        data_dir,
        root,
        main,
        run_dir,
        build,
        build_arg,
        name,
        program,
        deadline: start + TIMEOUT,
    };

    let runs = match tool {
        LatexTool::Tectonic(path) => run_tectonic(&job, path),
        LatexTool::Latexmk(path) => run_latexmk(&job, path),
        LatexTool::Engine(path) => run_engine(&job, path),
    };

    let mut diagnostics = Vec::new();
    let context = LogContext {
        run_dir: &job.run_dir,
        main: &job.main,
    };
    if let Ok(log) = fs::read(job.output("log")) {
        diagnostics.extend(parse_log(&String::from_utf8_lossy(&log), &context));
    }
    let aux = fs::read(job.output("aux"))
        .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
        .unwrap_or_default();
    let uses_bibliography = aux.contains("\\bibdata") || job.output("bcf").is_file();
    if uses_bibliography && let Ok(blg) = fs::read(job.output("blg")) {
        let bibs = bib_files(&aux, &job.run_dir);
        diagnostics.extend(parse_blg(&String::from_utf8_lossy(&blg), &context, &bibs));
    }
    if let Some(aborted) = runs.aborted.clone() {
        diagnostics.insert(0, aborted);
    }
    if !runs.succeeded && !diagnostics.iter().any(Diagnostic::is_error) {
        diagnostics.push(console_error(&job, &runs.console));
    }
    let mut diagnostics = finish(diagnostics);

    // A PDF written by this compile is shown even when there were errors;
    // an older one only when the tool says it is up to date.
    let pdf = job.output("pdf");
    let fresh = fs::metadata(&pdf)
        .and_then(|metadata| metadata.modified())
        .is_ok_and(|modified| {
            modified
                >= started_at
                    .checked_sub(Duration::from_secs(1))
                    .unwrap_or(started_at)
        });
    let document = if runs.aborted.is_none() && (fresh || runs.succeeded) {
        load_document(&job, &pdf)
    } else {
        None
    };

    if document.is_none() && pdf.is_file() && runs.aborted.is_none() && fresh {
        diagnostics.push(Diagnostic::error("The PDF LaTeX produced cannot be read."));
    }
    CompileOutput {
        document,
        diagnostics,
        elapsed: start.elapsed(),
    }
}

/// Opens the produced PDF (and its SyncTeX data) for the preview.
fn load_document(job: &Job, pdf: &Path) -> Option<Arc<dyn RenderedDocument>> {
    let bytes = fs::read(pdf).ok()?;
    let synctex = fs::read(job.output("synctex.gz")).ok();
    match PdfDocument::open(
        bytes,
        synctex.map(|bytes| (bytes, job.run_dir.clone(), job.root.clone())),
    ) {
        Ok(document) => Some(Arc::new(document)),
        Err(err) => {
            ::log::warn!("{} cannot be read: {err:#}", pdf.display());
            None
        }
    }
}

/// An error made from the console output, when the log explains nothing.
fn console_error(job: &Job, console: &str) -> Diagnostic {
    let lines: Vec<&str> = console
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect();
    let tectonic_error = lines
        .iter()
        .find_map(|line| line.strip_prefix("error: "))
        .map(str::to_owned);
    let tail = lines
        .iter()
        .rev()
        .take(6)
        .rev()
        .copied()
        .collect::<Vec<_>>()
        .join("\n");
    let mut diagnostic = Diagnostic::error(
        tectonic_error.unwrap_or_else(|| format!("{} failed without a log.", job.tool.describe())),
    );
    diagnostic.file = Some(job.main.clone());
    if !tail.is_empty() {
        diagnostic.hints.push(tail);
    }
    diagnostic
}

fn finished(result: Result<Finished, Diagnostic>) -> Runs {
    match result {
        Ok(finished) => Runs {
            succeeded: finished.status.success(),
            console: format!("{}\n{}", finished.stdout, finished.stderr),
            aborted: None,
        },
        Err(diagnostic) => Runs {
            succeeded: false,
            console: String::new(),
            aborted: Some(diagnostic),
        },
    }
}

fn run_tectonic(job: &Job, tectonic: &Path) -> Runs {
    let mut command = job.command(tectonic);
    tectonic_args(&mut command, job.data_dir, tectonic, &job.build_arg);
    command.arg(file_name(&job.main));
    finished(job.run(command))
}

/// Flags for a tectonic compile into `outdir`: keep the log and aux files
/// (for diagnostics and BibTeX messages), write SyncTeX, and keep going after
/// errors so a PDF is produced whenever possible. Oyl's own tectonic keeps
/// its download cache in the data folder.
pub(crate) fn tectonic_args(
    command: &mut Command,
    data_dir: &Path,
    tectonic: &Path,
    outdir: &Path,
) {
    command
        .args(["--keep-logs", "--keep-intermediates", "--synctex"])
        .args(["--chatter", "minimal", "--color", "never"])
        .args(["-Z", "continue-on-errors"])
        .arg("--outdir")
        .arg(outdir);
    if tectonic.starts_with(data_dir.join("bin")) {
        command.env("TECTONIC_CACHE_DIR", data_dir.join("tectonic-cache"));
    }
}

fn run_latexmk(job: &Job, latexmk: &Path) -> Runs {
    let mut command = job.command(latexmk);
    command
        .arg(job.program.latexmk_flag())
        // Keep going after errors, so BibTeX and reruns still happen and a
        // PDF is produced whenever TeX can produce one.
        .arg("-f")
        // Never start a viewer or a watch loop, whatever latexmkrc says.
        .args(["-pv-", "-pvc-"])
        .arg("-interaction=nonstopmode")
        .arg("-file-line-error")
        .arg("-synctex=1")
        .arg(format!("-outdir={}", job.build_arg.display()))
        .arg(file_name(&job.main));
    finished(job.run(command))
}

/// What decides whether BibTeX must run again: the citations, database and
/// style names in the `.aux`, and the databases' contents.
fn bibliography_digest(aux: &str, bibs: &[PathBuf]) -> u64 {
    let mut hasher = DefaultHasher::new();
    for line in aux.lines() {
        if line.starts_with("\\citation")
            || line.starts_with("\\bibdata")
            || line.starts_with("\\bibstyle")
        {
            line.hash(&mut hasher);
        }
    }
    for bib in bibs {
        fs::read(bib).ok().hash(&mut hasher);
    }
    hasher.finish()
}

/// Whether the log asks for another pass.
fn wants_rerun(log: &str) -> bool {
    log.contains("Rerun to get")
        || log.contains("Label(s) may have changed")
        || log.contains("Please (re)run")
        || log.contains("Rerun LaTeX")
        || (log.contains("has changed") && log.contains("Rerun"))
}

/// pdflatex (or xelatex/lualatex) by hand: a pass, BibTeX or Biber when the
/// citations changed, and passes until the cross-references settle.
fn run_engine(job: &Job, pdflatex: &Path) -> Runs {
    let dirs = search_dirs(job.data_dir);
    let engine = match job.program {
        Program::Pdf => pdflatex.to_path_buf(),
        other => pdflatex
            .parent()
            .map(|dir| dir.join(other.engine()))
            .filter(|path| path.is_file())
            .or_else(|| find_in(&dirs, other.engine()))
            .unwrap_or_else(|| pdflatex.to_path_buf()),
    };
    let pass = || {
        let mut command = job.command(&engine);
        command
            .arg("-interaction=nonstopmode")
            .arg("-file-line-error")
            .arg("-synctex=1")
            .arg(format!("-output-directory={}", job.build_arg.display()))
            .arg(file_name(&job.main));
        finished(job.run(command))
    };

    let mut runs = pass();
    let mut passes = 1;
    if runs.aborted.is_some() {
        return runs;
    }

    let aux = fs::read_to_string(job.output("aux")).unwrap_or_default();
    let digest_file = job.output("oyl-bib");
    let mut bibliography_ran = false;
    if aux.contains("\\bibdata") {
        let bibs = bib_files(&aux, &job.run_dir);
        let digest = bibliography_digest(&aux, &bibs).to_string();
        let current = fs::read_to_string(&digest_file).unwrap_or_default();
        if (current != digest || !job.output("bbl").is_file())
            && let Some(bibtex) = find_in(&dirs, "bibtex")
        {
            let mut command = job.command(&bibtex);
            let mut inputs = job.run_dir.clone().into_os_string();
            inputs.push(":");
            command
                .current_dir(&job.build)
                .env("BIBINPUTS", &inputs)
                .env("BSTINPUTS", &inputs)
                .arg(&job.name);
            if let Err(aborted) = job.run(command) {
                return Runs {
                    aborted: Some(aborted),
                    ..runs
                };
            }
            bibliography_ran = true;
            // Written only after a run, so a failed one is retried.
            let _ = fs::write(&digest_file, digest);
        }
    } else if job.output("bcf").is_file() {
        let log = fs::read_to_string(job.output("log")).unwrap_or_default();
        if (log.contains("Please (re)run Biber") || !job.output("bbl").is_file())
            && let Some(biber) = find_in(&dirs, "biber")
        {
            let mut command = job.command(&biber);
            command
                .arg(format!("--input-directory={}", job.build.display()))
                .arg(format!("--output-directory={}", job.build.display()))
                .arg(&job.name);
            if let Err(aborted) = job.run(command) {
                return Runs {
                    aborted: Some(aborted),
                    ..runs
                };
            }
            bibliography_ran = true;
        }
    }

    // A new bibliography needs two more passes (entries, then labels).
    let mut owed: usize = if bibliography_ran { 2 } else { 0 };
    while passes < MAX_PASSES {
        let log = fs::read_to_string(job.output("log")).unwrap_or_default();
        if owed == 0 && !wants_rerun(&log) {
            break;
        }
        runs = pass();
        passes += 1;
        owed = owed.saturating_sub(1);
        if runs.aborted.is_some() {
            break;
        }
    }
    runs
}

fn file_name(path: &Path) -> &std::ffi::OsStr {
    path.file_name().unwrap_or(path.as_os_str())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn reads_the_magic_program_comment() {
        assert_eq!(
            Program::of("% !TEX program = xelatex\n\\documentclass{article}"),
            Program::Xe
        );
        assert_eq!(Program::of("%!TEX TS-program = LuaLaTeX\n"), Program::Lua);
        assert_eq!(Program::of("\\documentclass{article}\n"), Program::Pdf);
    }

    #[test]
    fn rerun_detection() {
        assert!(wants_rerun(
            "LaTeX Warning: Label(s) may have changed. Rerun to get cross-references right."
        ));
        assert!(!wants_rerun(
            "LaTeX Warning: There were undefined references."
        ));
    }

    /// The bare-engine path (pdflatex + bibtex + reruns), which the tool
    /// detection never picks on a machine that also has latexmk.
    #[test]
    fn bare_pdflatex_runs_bibtex_and_reruns() {
        let data = tempfile::tempdir().unwrap();
        let dirs = search_dirs(data.path());
        let Some(pdflatex) = find_in(&dirs, "pdflatex") else {
            eprintln!("skipped: pdflatex not found");
            return;
        };
        if find_in(&dirs, "bibtex").is_none() {
            eprintln!("skipped: bibtex not found");
            return;
        }
        let paper = tempfile::tempdir().unwrap();
        let root = paper.path().to_path_buf();
        fs::write(
            root.join("main.tex"),
            "\\documentclass{article}\n\\begin{document}\nAs \\cite{knuth84} shows.\n\
             \\bibliographystyle{plain}\n\\bibliography{refs}\n\\end{document}\n",
        )
        .unwrap();
        fs::write(
            root.join("refs.bib"),
            "@book{knuth84, author = {Donald E. Knuth}, title = {The TeXbook}, publisher = {Addison-Wesley}, year = {1984}}\n",
        )
        .unwrap();
        let request = CompileRequest {
            root: root.clone(),
            main: root.join("main.tex"),
            engine: oyl_model::Engine::Latex,
            overlays: HashMap::new(),
        };
        let tool = LatexTool::Engine(pdflatex);
        let output = compile(&request, &tool, data.path());
        assert!(
            !output.diagnostics.iter().any(Diagnostic::is_error),
            "{:#?}",
            output.diagnostics
        );
        assert!(
            !output
                .diagnostics
                .iter()
                .any(|d| d.message.contains("undefined")),
            "citation resolved: {:#?}",
            output.diagnostics
        );
        let document = output.document.expect("a PDF");
        assert_eq!(document.page_count(), 1);
        let bbl = fs::read_to_string(root.join(".oyl/build/main.bbl")).unwrap();
        assert!(bbl.contains("knuth84"));

        // A second compile with unchanged citations skips BibTeX.
        let before = fs::metadata(root.join(".oyl/build/main.bbl"))
            .unwrap()
            .modified()
            .unwrap();
        let output = compile(&request, &tool, data.path());
        assert!(output.document.is_some());
        let after = fs::metadata(root.join(".oyl/build/main.bbl"))
            .unwrap()
            .modified()
            .unwrap();
        assert_eq!(before, after);
    }
}
