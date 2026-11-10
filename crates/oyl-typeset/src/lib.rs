//! Oyl typesetting: turns a paper project into pages.
//!
//! [`Compiler`] is the one entry point. It compiles
//!
//! - **Typst** in-process, incrementally: one long-lived world per open
//!   project, unsaved editor buffers on top of the files on disk, packages
//!   from Typst Universe downloaded on demand into Oyl's data folder;
//! - **LaTeX** out of process with tectonic, latexmk or pdflatex, into
//!   `<root>/.oyl/build/`, with the log turned into diagnostics.
//!
//! Both produce a [`RenderedDocument`] whose pages are rasterized lazily, at
//! any scale, from any thread. [`rasterize_pdf`] shows an arbitrary PDF the
//! same way.

pub use oyl_model::typeset::*;

mod download;
mod latex;
mod paths;
mod pdf;
mod raster;
mod typst_engine;
mod world;

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::time::Instant;

use oyl_model::{Diagnostic, Engine};

pub use crate::pdf::rasterize_pdf;

/// Compiles paper projects. Owns the caches that make recompiles fast (the
/// Typst world, loaded fonts, memoized layout), so keep one for the life of
/// the app and call it from one background thread at a time.
pub struct Compiler {
    data_dir: PathBuf,
    typst: typst_engine::TypstEngine,
}

impl Compiler {
    /// A compiler that keeps downloaded Typst packages and fetched
    /// toolchains in `data_dir` (Oyl's application-support folder).
    /// Cheap: fonts are discovered on the first Typst compile.
    pub fn new(data_dir: PathBuf) -> Self {
        Self {
            typst: typst_engine::TypstEngine::new(&data_dir),
            data_dir,
        }
    }

    /// The folder holding the package cache (`packages/`) and fetched
    /// toolchains (`bin/`).
    pub fn data_dir(&self) -> &Path {
        &self.data_dir
    }

    /// Compiles one project. Never panics: every failure, including a
    /// missing toolchain or a crash inside a compiler, is reported as a
    /// diagnostic, and `document` is `None` when there is nothing to show.
    ///
    /// Typst compiles `request.overlays` over the files on disk. LaTeX
    /// compiles from disk (the app saves before a LaTeX compile).
    pub fn compile(&mut self, request: &CompileRequest) -> CompileOutput {
        let start = Instant::now();
        let data_dir = &self.data_dir;
        let typst = &mut self.typst;
        let result = catch_unwind(AssertUnwindSafe(|| match request.engine {
            Engine::Typst => typst.compile(request),
            Engine::Latex => match latex::detect(data_dir) {
                Some(tool) => latex::compile(request, &tool, data_dir),
                None => {
                    let mut diagnostic = Diagnostic::error("No LaTeX installation was found.");
                    diagnostic.hints.push(if latex::target_triple().is_some() {
                        "Oyl can download tectonic, a self-contained LaTeX, from the setup screen.".into()
                    } else {
                        "Install TeX Live or MacTeX and reopen the paper.".into()
                    });
                    CompileOutput::failed(vec![diagnostic], start.elapsed())
                }
            },
        }));
        result.unwrap_or_else(|payload| {
            let message = format!(
                "Compiling stopped unexpectedly: {}",
                raster::panic_message(&*payload)
            );
            log::error!("{message}");
            CompileOutput::failed(vec![Diagnostic::error(message)], start.elapsed())
        })
    }

    /// Whether `engine` can run here, and with what.
    pub fn toolchain(&self, engine: Engine) -> ToolchainStatus {
        match engine {
            Engine::Typst => {
                let version = typst::utils::version();
                ToolchainStatus::Ready {
                    detail: format!(
                        "Typst {}.{}.{} (built in)",
                        version.major(),
                        version.minor(),
                        version.patch()
                    ),
                }
            }
            Engine::Latex => match latex::detect(&self.data_dir) {
                Some(tool) => ToolchainStatus::Ready {
                    detail: tool.describe(),
                },
                None if latex::target_triple().is_some() => ToolchainStatus::Installable {
                    detail: "No LaTeX found. Oyl can download tectonic (about 60 MB with the LaTeX basics).".into(),
                },
                None => ToolchainStatus::Unavailable {
                    detail: "No LaTeX found. Install TeX Live or MacTeX.".into(),
                },
            },
        }
    }

    /// Downloads tectonic from its official GitHub releases into
    /// `<data_dir>/bin/`, checks that it runs, and makes it the LaTeX
    /// toolchain. `progress` receives short human-readable status lines.
    pub fn install_latex(&self, progress: &dyn Fn(&str)) -> anyhow::Result<()> {
        latex::install_tectonic(&self.data_dir, progress).map(|_| ())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_send<T: Send>() {}

    #[test]
    fn compiler_moves_to_a_background_thread() {
        assert_send::<Compiler>();
    }

    #[test]
    fn typst_is_always_ready() {
        let data = tempfile::tempdir().unwrap();
        let compiler = Compiler::new(data.path().to_path_buf());
        let status = compiler.toolchain(Engine::Typst);
        assert!(status.is_ready());
        assert!(
            status.detail().starts_with("Typst 0.15"),
            "{}",
            status.detail()
        );
    }
}
