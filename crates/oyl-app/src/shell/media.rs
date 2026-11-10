//! Images and files given to the editor: pasted images and dropped images
//! become figures, a dropped PDF becomes a source, a dropped `.bib` is
//! imported.

use std::fs;
use std::path::{Path, PathBuf};

use gpui_kit::prelude::*;
use gpui_kit::{Context, Window};
use oyl_model::Engine;
use oyl_sources::figures::{self, Figure};
use oyl_sources::pdfs;

use super::{Shell, ToastKind};

/// Image files that become figures when dropped.
const IMAGE_EXTENSIONS: [&str; 13] = [
    "png", "jpg", "jpeg", "gif", "webp", "tif", "tiff", "bmp", "heic", "heif", "avif", "svg", "eps",
];

/// What a dropped file is to the paper.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Dropped {
    /// An image: a figure.
    Image,
    /// A PDF: the paper of a source.
    Pdf,
    /// A bibliography to import.
    Bib,
    /// Something Oyl does not take.
    Other,
}

/// Sorts a dropped file by its extension.
pub(crate) fn classify(path: &Path) -> Dropped {
    let ext = path
        .extension()
        .and_then(|ext| ext.to_str())
        .map(str::to_ascii_lowercase)
        .unwrap_or_default();
    if ext == "pdf" {
        Dropped::Pdf
    } else if ext == "bib" {
        Dropped::Bib
    } else if IMAGE_EXTENSIONS.contains(&ext.as_str()) {
        Dropped::Image
    } else {
        Dropped::Other
    }
}

/// The path a source file writes to include a figure: relative to the paper
/// for LaTeX (it compiles in the paper folder); for Typst, relative to the
/// file when it sits in the paper folder, else rooted at the paper (`/…`).
pub(crate) fn include_path(
    engine: Engine,
    root: &Path,
    file: Option<&Path>,
    figure: &Figure,
) -> String {
    match engine {
        Engine::Latex => figure.rel_path.clone(),
        Engine::Typst => {
            let at_root = file
                .and_then(Path::parent)
                .is_none_or(|dir| super::paper::same_folder(dir, root));
            if at_root {
                figure.rel_path.clone()
            } else {
                format!("/{}", figure.rel_path)
            }
        }
    }
}

/// Adds `\usepackage{graphicx}` to the main LaTeX file when it lacks it.
/// Returns whether the file changed.
pub(crate) fn ensure_graphics_preamble(main: &Path) -> anyhow::Result<bool> {
    let text = fs::read_to_string(main)?;
    match figures::ensure_preamble(Engine::Latex, &text) {
        Some(updated) => {
            oyl_core::write_file_atomic(main, updated)?;
            Ok(true)
        }
        None => Ok(false),
    }
}

/// Where an import comes from.
enum ImageSource {
    Bytes { bytes: Vec<u8>, extension: String },
    File(PathBuf),
}

impl Shell {
    /// A pasted image: stored in `figures/`, then a figure block at the
    /// cursor.
    pub(crate) fn pasted_image(
        &mut self,
        bytes: Vec<u8>,
        extension: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.import_images(vec![ImageSource::Bytes { bytes, extension }], window, cx);
    }

    /// Files dropped on (or pasted into) the editor.
    pub(crate) fn dropped_files(
        &mut self,
        paths: Vec<PathBuf>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let mut images = Vec::new();
        let mut refused = Vec::new();
        for path in paths {
            match classify(&path) {
                Dropped::Image => images.push(ImageSource::File(path)),
                Dropped::Pdf => self.add_pdf_source(path, window, cx),
                Dropped::Bib => self.import_bib(path, window, cx),
                Dropped::Other => refused.push(
                    path.file_name()
                        .map(|name| name.to_string_lossy().into_owned())
                        .unwrap_or_else(|| path.display().to_string()),
                ),
            }
        }
        if !images.is_empty() {
            self.import_images(images, window, cx);
        }
        if !refused.is_empty() {
            self.toast(
                ToastKind::Info,
                format!(
                    "Oyl takes images, PDFs and .bib files here; {} left out.",
                    refused.join(", ")
                ),
                window,
                cx,
            );
        }
    }

    fn import_images(
        &mut self,
        sources: Vec<ImageSource>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(paper) = self.paper.as_ref() else {
            self.toast(
                ToastKind::Info,
                "Open a paper to add figures to it.",
                window,
                cx,
            );
            return;
        };
        let root = paper.root().to_path_buf();
        let engine = paper.engine();
        let main = paper.project.main_path();
        let epoch = paper.epoch;
        if engine == Engine::Latex {
            // The preamble is edited on disk; the buffers must be there first.
            let saved = self.editor.update(cx, |editor, cx| editor.save_all(cx));
            if let Err(err) = saved {
                self.toast(
                    ToastKind::Error,
                    format!("The figure was not added because a file could not be saved: {err:#}"),
                    window,
                    cx,
                );
                return;
            }
        }
        cx.spawn_in(window, async move |this, cx| {
            let imported = cx
                .background_spawn(async move {
                    let preamble = match engine {
                        Engine::Latex => {
                            ensure_graphics_preamble(&main).map_err(|err| format!("{err:#}"))?
                        }
                        Engine::Typst => false,
                    };
                    let mut figures = Vec::new();
                    for source in sources {
                        let figure = match source {
                            ImageSource::Bytes { bytes, extension } => {
                                figures::import_bytes(&root, &bytes, None, &extension)
                            }
                            ImageSource::File(path) => figures::import_file(&root, &path),
                        };
                        figures.push(figure.map_err(|err| err.to_string())?);
                    }
                    Ok::<_, String>((preamble, main, root, figures))
                })
                .await;
            let _ = this.update_in(cx, |this, window, cx| {
                if this.paper.as_ref().map(|p| p.epoch) != Some(epoch) {
                    return;
                }
                match imported {
                    Ok((preamble, main, root, figures)) => {
                        if preamble {
                            this.editor.update(cx, |editor, cx| {
                                editor.reload_from_disk(&main, window, cx)
                            });
                        }
                        let active = this.editor.read(cx).active_path();
                        let blocks: Vec<String> = figures
                            .iter()
                            .map(|figure| {
                                let path = include_path(engine, &root, active.as_deref(), figure);
                                figures::snippet(engine, &path, "", None)
                            })
                            .collect();
                        let text = format!("\n{}\n", blocks.join("\n\n"));
                        this.editor
                            .update(cx, |editor, cx| editor.insert_at_cursor(&text, window, cx));
                        this.refresh_index(cx);
                    }
                    Err(message) => this.toast(
                        ToastKind::Error,
                        format!("The image could not be added: {message}"),
                        window,
                        cx,
                    ),
                }
            });
        })
        .detach();
    }

    /// A dropped PDF: identified (looking its DOI or arXiv id up), added to
    /// the bibliography, and kept as that source's PDF.
    fn add_pdf_source(&mut self, pdf: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        let Some(paper) = self.paper.as_ref() else {
            self.toast(
                ToastKind::Info,
                "Open a paper to add sources to it.",
                window,
                cx,
            );
            return;
        };
        let Some(bibliography) = paper.bibliography.clone() else {
            self.toast(
                ToastKind::Error,
                "The bibliography could not be read, so no source can be added.",
                window,
                cx,
            );
            return;
        };
        let root = paper.root().to_path_buf();
        let epoch = paper.epoch;
        let name = pdf
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        self.toast(ToastKind::Info, format!("Reading {name}…"), window, cx);
        cx.spawn_in(window, async move |this, cx| {
            let added = cx
                .background_spawn(async move {
                    let source = pdfs::from_pdf(&pdf).map_err(|err| err.to_string())?;
                    let (key, candidates) = {
                        let mut bib = bibliography.lock();
                        let key = match bib.find_duplicate(&source) {
                            Some(existing) => existing.to_string(),
                            None => bib.add(source),
                        };
                        bib.save().map_err(|err| err.to_string())?;
                        (key, bib.candidates())
                    };
                    pdfs::attach(&root, &key, &pdf).map_err(|err| err.to_string())?;
                    Ok::<_, String>((key, candidates))
                })
                .await;
            let _ = this.update_in(cx, |this, window, cx| {
                if this.paper.as_ref().map(|p| p.epoch) != Some(epoch) {
                    return;
                }
                match added {
                    Ok((key, candidates)) => {
                        this.set_citations(candidates, cx);
                        this.toast(ToastKind::Success, format!("Added {key}"), window, cx);
                    }
                    Err(message) => this.toast(
                        ToastKind::Error,
                        format!("{name} could not be added as a source: {message}"),
                        window,
                        cx,
                    ),
                }
            });
        })
        .detach();
    }

    /// A dropped `.bib`: its entries join the paper's bibliography (an
    /// entry already there is not added twice).
    fn import_bib(&mut self, file: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        let Some(paper) = self.paper.as_ref() else {
            self.toast(
                ToastKind::Info,
                "Open a paper to add sources to it.",
                window,
                cx,
            );
            return;
        };
        let Some(bibliography) = paper.bibliography.clone() else {
            self.toast(
                ToastKind::Error,
                "The bibliography could not be read, so nothing can be imported.",
                window,
                cx,
            );
            return;
        };
        let epoch = paper.epoch;
        cx.spawn_in(window, async move |this, cx| {
            let imported = cx
                .background_spawn(async move {
                    let text = fs::read_to_string(&file)
                        .map_err(|err| format!("{} could not be read: {err}", file.display()))?;
                    let mut bib = bibliography.lock();
                    let before = bib.len();
                    let keys = bib.import_bibtex(&text).map_err(|err| err.to_string())?;
                    bib.save().map_err(|err| err.to_string())?;
                    Ok::<_, String>((bib.len() - before, keys.len(), bib.candidates()))
                })
                .await;
            let _ = this.update_in(cx, |this, window, cx| {
                if this.paper.as_ref().map(|p| p.epoch) != Some(epoch) {
                    return;
                }
                match imported {
                    Ok((added, total, candidates)) => {
                        this.set_citations(candidates, cx);
                        let message = match (added, total) {
                            (0, _) => "Every source in that file is already in the bibliography."
                                .to_string(),
                            (1, _) => "Added 1 source".to_string(),
                            (n, _) => format!("Added {n} sources"),
                        };
                        this.toast(ToastKind::Success, message, window, cx);
                    }
                    Err(message) => this.toast(
                        ToastKind::Error,
                        format!("The bibliography could not be imported: {message}"),
                        window,
                        cx,
                    ),
                }
            });
        })
        .detach();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn figure(rel: &str) -> Figure {
        Figure {
            path: PathBuf::from("/p").join(rel),
            rel_path: rel.into(),
            width: None,
            height: None,
            bytes: 0,
            used: false,
        }
    }

    #[test]
    fn dropped_files_are_sorted_by_extension() {
        assert_eq!(classify(Path::new("/x/plot.PNG")), Dropped::Image);
        assert_eq!(classify(Path::new("/x/photo.heic")), Dropped::Image);
        assert_eq!(classify(Path::new("/x/paper.pdf")), Dropped::Pdf);
        assert_eq!(classify(Path::new("/x/refs.bib")), Dropped::Bib);
        assert_eq!(classify(Path::new("/x/notes.docx")), Dropped::Other);
        assert_eq!(classify(Path::new("/x/README")), Dropped::Other);
    }

    #[test]
    fn typst_figures_in_subfolders_are_rooted_at_the_paper() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::create_dir_all(root.join("chapters")).unwrap();
        let fig = figure("figures/plot.png");
        assert_eq!(
            include_path(Engine::Typst, root, Some(&root.join("main.typ")), &fig),
            "figures/plot.png"
        );
        assert_eq!(
            include_path(
                Engine::Typst,
                root,
                Some(&root.join("chapters/one.typ")),
                &fig
            ),
            "/figures/plot.png"
        );
        assert_eq!(
            include_path(
                Engine::Latex,
                root,
                Some(&root.join("chapters/one.tex")),
                &fig
            ),
            "figures/plot.png"
        );
        assert_eq!(
            include_path(Engine::Typst, root, None, &fig),
            "figures/plot.png"
        );
    }
}
