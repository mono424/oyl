//! Typst, in-process: compile, diagnostics, and the laid-out document.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::Path;
use std::sync::{Arc, OnceLock};
use std::time::Instant;

use anyhow::{Context, Result, anyhow, bail};
use oyl_model::typeset::{
    CompileOutput, CompileRequest, PageImage, PagePosition, RenderedDocument,
};
use oyl_model::{Diagnostic, Severity, SourceLocation};
use typst::diag::{SourceDiagnostic, Warned};
use typst::layout::{Abs, Frame, FrameItem, Point};
use typst::syntax::{
    DiagSpan, FileId, LinkedNode, RootedPath, Side, Source, Span, SyntaxKind, VirtualPath,
    VirtualRoot,
};
use typst_ide::{Jump, jump_from_click_in_frame, jump_from_cursor};
use typst_kit::packages::{FsPackages, SystemPackages, UniversePackages};
use typst_layout::PagedDocument;
use typst_pdf::PdfOptions;
use typst_render::RenderOptions;

use crate::download::Http;
use crate::paths::absolute;
use crate::raster::{panic_message, pixel_size, premultiplied_onto_white};
use crate::world::{Shared, SnapshotWorld, TypstWorld};

/// How many compiles a memoized result may go unused before comemo drops it.
const CACHE_MAX_AGE: usize = 10;

/// The Typst half of the [`Compiler`](crate::Compiler).
pub(crate) struct TypstEngine {
    packages: Arc<SystemPackages>,
    shared: Option<Arc<Shared>>,
    world: Option<TypstWorld>,
}

impl TypstEngine {
    /// An engine whose downloaded packages live in `<data_dir>/packages`.
    /// Local packages (`@local/...`) are served from Typst's standard data
    /// directory, like the Typst CLI does.
    pub(crate) fn new(data_dir: &Path) -> Self {
        let packages = SystemPackages::from_parts(
            FsPackages::system_data(),
            Some(FsPackages::new(data_dir.join("packages"))),
            UniversePackages::new(Http::new()),
        );
        Self {
            packages: Arc::new(packages),
            shared: None,
            world: None,
        }
    }

    /// Compiles a Typst project. Never panics: failures are diagnostics.
    pub(crate) fn compile(&mut self, request: &CompileRequest) -> CompileOutput {
        let start = Instant::now();
        let root = absolute(&request.root);
        let main = absolute(&request.main);

        if !root.is_dir() {
            let message = format!("The project folder {} does not exist.", root.display());
            return CompileOutput::failed(vec![Diagnostic::error(message)], start.elapsed());
        }
        let main_id = match VirtualPath::virtualize(&root, &main) {
            Ok(vpath) => RootedPath::new(VirtualRoot::Project, vpath).intern(),
            Err(err) => {
                let mut diagnostic = Diagnostic::error(format!(
                    "The main file {} cannot be compiled: {err}.",
                    main.display()
                ));
                diagnostic
                    .hints
                    .push("The main file must be inside the project folder.".into());
                return CompileOutput::failed(vec![diagnostic], start.elapsed());
            }
        };

        let shared = self
            .shared
            .get_or_insert_with(|| Arc::new(Shared::load()))
            .clone();
        let world = match &mut self.world {
            Some(world) if world.root() == root => world,
            slot => slot.insert(TypstWorld::new(
                shared,
                self.packages.clone(),
                root,
                main_id,
            )),
        };
        world.prepare(main_id, &request.overlays);

        let compiled = catch_unwind(AssertUnwindSafe(|| {
            let Warned { output, warnings } = typst::compile::<PagedDocument>(&*world);
            let mut diagnostics: Vec<Diagnostic> = Vec::new();
            let document = match output {
                Ok(document) => {
                    let snapshot = world.snapshot();
                    Some(Arc::new(TypstDocument::new(document, snapshot))
                        as Arc<dyn RenderedDocument>)
                }
                Err(errors) => {
                    diagnostics.extend(errors.iter().map(|error| convert(world, error)));
                    None
                }
            };
            diagnostics.extend(warnings.iter().map(|warning| convert(world, warning)));
            (document, diagnostics)
        }));

        let output = match compiled {
            Ok((document, diagnostics)) => CompileOutput {
                document,
                diagnostics,
                elapsed: start.elapsed(),
            },
            Err(payload) => {
                // The memoization caches and the world may hold state from the
                // aborted compile; start over cleanly next time.
                self.world = None;
                comemo::evict(0);
                let message = format!("Typst stopped unexpectedly: {}", panic_message(&*payload));
                log::error!("{message}");
                return CompileOutput::failed(vec![Diagnostic::error(message)], start.elapsed());
            }
        };
        comemo::evict(CACHE_MAX_AGE);
        output
    }
}

/// Maps a Typst diagnostic onto the contract's [`Diagnostic`].
///
/// Errors raised inside a package are reported at the call site in the
/// project (the innermost trace point there), with the package location as a
/// hint, because that is the code the writer can change.
fn convert(world: &TypstWorld, diagnostic: &SourceDiagnostic) -> Diagnostic {
    let severity = match diagnostic.severity {
        typst::diag::Severity::Error => Severity::Error,
        typst::diag::Severity::Warning => Severity::Warning,
    };
    let mut out = Diagnostic {
        severity,
        message: diagnostic.message.to_string(),
        file: None,
        line: None,
        column: None,
        hints: diagnostic
            .hints
            .iter()
            .map(|hint| hint.v.to_string())
            .collect(),
    };

    let primary = world.locate(diagnostic.span);
    let in_project =
        |located: &crate::world::Located| matches!(located.id.root(), VirtualRoot::Project);
    let location = match primary {
        Some(located) if in_project(&located) => Some(located),
        other => {
            let call_site = diagnostic
                .trace
                .iter()
                .filter_map(|point| world.locate(DiagSpan::from(point.span)))
                .find(|located| in_project(located));
            match (call_site, other) {
                (Some(site), Some(inner)) => {
                    out.hints.push(format!(
                        "raised in {}:{}:{}",
                        package_label(inner.id, &inner.path),
                        inner.line,
                        inner.column
                    ));
                    Some(site)
                }
                (site, inner) => site.or(inner),
            }
        }
    };
    if let Some(located) = location {
        out.file = Some(located.path);
        out.line = Some(located.line);
        out.column = Some(located.column);
    }
    out
}

/// `@preview/name:1.0.0/src/lib.typ` for package files, the path otherwise.
fn package_label(id: FileId, path: &Path) -> String {
    match id.root() {
        VirtualRoot::Package(spec) => format!("{spec}{}", id.vpath().get_with_slash()),
        VirtualRoot::Project => path.display().to_string(),
    }
}

/// A laid-out Typst paper.
pub(crate) struct TypstDocument {
    document: PagedDocument,
    world: SnapshotWorld,
    fingerprints: Vec<u64>,
    pdf: OnceLock<Result<Arc<Vec<u8>>, String>>,
}

impl TypstDocument {
    fn new(document: PagedDocument, world: SnapshotWorld) -> Self {
        let fingerprints = document
            .pages()
            .iter()
            .map(|page| {
                let hash = typst::utils::hash128(page);
                (hash as u64) ^ ((hash >> 64) as u64)
            })
            .collect();
        Self {
            document,
            world,
            fingerprints,
            pdf: OnceLock::new(),
        }
    }

    fn page(&self, index: usize) -> Result<&typst_layout::Page> {
        self.document.pages().get(index).ok_or_else(|| {
            anyhow!(
                "page {} does not exist (the paper has {} pages)",
                index + 1,
                self.document.pages().len()
            )
        })
    }

    fn export_pdf(&self) -> Result<Arc<Vec<u8>>, String> {
        let exported = catch_unwind(AssertUnwindSafe(|| {
            typst_pdf::pdf(&self.document, &PdfOptions::default())
        }));
        match exported {
            Ok(Ok(bytes)) => Ok(Arc::new(bytes)),
            Ok(Err(errors)) => Err(errors
                .iter()
                .map(|error| error.message.to_string())
                .collect::<Vec<_>>()
                .join("; ")),
            Err(payload) => Err(format!(
                "PDF export stopped unexpectedly: {}",
                panic_message(&*payload)
            )),
        }
    }

    /// The location of a jump target, as a 1-based line and column.
    fn location(&self, id: FileId, offset: usize) -> Option<SourceLocation> {
        let source = self.world.source_of(id)?;
        let (line, column) = source.lines().byte_to_line_column(offset)?;
        Some(SourceLocation::new(
            self.world.path_of(id)?,
            u32::try_from(line + 1).ok()?,
            u32::try_from(column + 1).ok()?,
        ))
    }
}

/// Removes link areas from a frame so that a click lands on the text under
/// it (a citation is a link to the bibliography, but clicking it should open
/// the `@key` in the source).
fn strip_links(frame: &mut Frame) {
    frame.retain(|item| match item {
        FrameItem::Link(..) => false,
        FrameItem::Group(group) => {
            strip_links(&mut group.frame);
            true
        }
        _ => true,
    });
}

/// The glyph produced by the text node `span` whose offset into the node is
/// the largest one not after `offset` (bytes), with its baseline position in
/// the frame.
fn glyph_position(frame: &Frame, span: Span, offset: usize) -> Option<(usize, Point)> {
    let mut best: Option<(usize, Point)> = None;
    for (position, item) in frame.items() {
        match item {
            FrameItem::Group(group) => {
                if let Some((glyph, point)) = glyph_position(&group.frame, span, offset)
                    && best.is_none_or(|(known, _)| glyph > known)
                {
                    best = Some((glyph, *position + point.transform(group.transform)));
                }
            }
            FrameItem::Text(text) => {
                let mut x = position.x;
                for glyph in &text.glyphs {
                    let (glyph_span, glyph_offset) = glyph.span;
                    let glyph_offset = usize::from(glyph_offset);
                    if glyph_span == span
                        && glyph_offset <= offset
                        && best.is_none_or(|(known, _)| glyph_offset > known)
                    {
                        best = Some((glyph_offset, Point::new(x, position.y)));
                    }
                    x += glyph.x_advance.at(text.size);
                }
            }
            _ => {}
        }
    }
    best
}

/// Byte offset of a 1-based line and column, clamped into the line.
fn offset_of(source: &Source, line: u32, column: u32) -> Option<usize> {
    let lines = source.lines();
    let line = usize::try_from(line.max(1) - 1).ok()?;
    let range = lines.line_to_range(line)?;
    let column = usize::try_from(column.max(1) - 1).ok()?;
    Some(
        lines
            .line_column_to_byte(line, column)
            .unwrap_or(range.end)
            .min(range.end),
    )
}

/// Text leaves to try for a cursor position: the one at the cursor, then the
/// following ones, then the preceding ones (the cursor may sit on markup
/// such as `= ` or `#emph[` that produces no glyphs itself).
fn text_offsets_near(source: &Source, cursor: usize) -> Vec<usize> {
    const MAX_LEAVES: usize = 400;
    let is_text =
        |node: &LinkedNode| matches!(node.kind(), SyntaxKind::Text | SyntaxKind::MathText);
    let root = LinkedNode::new(source.root());
    let mut offsets = vec![cursor];
    if let Some(start) = root
        .leaf_at(cursor, Side::After)
        .or_else(|| root.leaf_at(cursor, Side::Before))
    {
        let mut next = Some(start.clone());
        for _ in 0..MAX_LEAVES {
            let Some(node) = next else { break };
            if is_text(&node) {
                offsets.push(node.offset());
            }
            next = node.next_leaf();
        }
        let mut prev = start.prev_leaf();
        for _ in 0..MAX_LEAVES {
            let Some(node) = prev else { break };
            if is_text(&node) {
                offsets.push(node.range().end);
            }
            prev = node.prev_leaf();
        }
    }
    offsets
}

impl RenderedDocument for TypstDocument {
    fn page_count(&self) -> usize {
        self.document.pages().len()
    }

    fn page_size_pt(&self, index: usize) -> (f32, f32) {
        match self.document.pages().get(index) {
            Some(page) => {
                let size = page.frame.size();
                (size.x.to_pt() as f32, size.y.to_pt() as f32)
            }
            None => (0.0, 0.0),
        }
    }

    fn render_page(&self, index: usize, pixels_per_pt: f32) -> Result<PageImage> {
        let page = self.page(index)?;
        let (width_pt, height_pt) = self.page_size_pt(index);
        // Validates the scale and bounds the allocation before typst-render
        // allocates the pixmap.
        let (width, height) = pixel_size(width_pt, height_pt, pixels_per_pt)?;
        let options = RenderOptions {
            pixel_per_pt: typst::utils::Scalar::new(f64::from(pixels_per_pt)),
            render_bleed: false,
        };
        let pixmap = catch_unwind(AssertUnwindSafe(|| typst_render::render(page, &options)))
            .map_err(|payload| {
                anyhow!(
                    "rendering page {} failed: {}",
                    index + 1,
                    panic_message(&*payload)
                )
            })?;
        let (actual_width, actual_height) = (pixmap.width(), pixmap.height());
        if (actual_width, actual_height) != (width, height) {
            bail!(
                "rendering page {} produced {actual_width}×{actual_height} px instead of {width}×{height}",
                index + 1
            );
        }
        Ok(PageImage {
            width,
            height,
            rgba: premultiplied_onto_white(pixmap.take()),
        })
    }

    fn pdf(&self) -> Result<Arc<Vec<u8>>> {
        self.pdf
            .get_or_init(|| self.export_pdf())
            .clone()
            .map_err(|message| anyhow!("PDF export failed: {message}"))
            .context("exporting the paper")
    }

    fn page_fingerprint(&self, index: usize) -> u64 {
        // Out of range: a value no real page hashes to in practice, and one
        // that differs per index.
        self.fingerprints
            .get(index)
            .copied()
            .unwrap_or(u64::MAX - index as u64)
    }

    fn source_at(&self, position: PagePosition) -> Option<SourceLocation> {
        let page = self.document.pages().get(position.page)?;
        let mut frame = page.frame.clone();
        strip_links(&mut frame);
        let click = Point::new(
            Abs::pt(f64::from(position.x_pt)),
            Abs::pt(f64::from(position.y_pt)),
        );
        match jump_from_click_in_frame(&self.world, &self.document, &frame, click)? {
            Jump::File(id, offset) => self.location(id, offset),
            Jump::Url(_) | Jump::Position(_) => None,
        }
    }

    fn position_of(&self, location: &SourceLocation) -> Option<PagePosition> {
        let source = self.world.source_at_path(&location.file)?;
        let cursor = offset_of(source, location.line, location.column)?;
        let is_text =
            |node: &LinkedNode| matches!(node.kind(), SyntaxKind::Text | SyntaxKind::MathText);
        text_offsets_near(source, cursor)
            .into_iter()
            .find_map(|offset| {
                // typst-ide finds the pages showing the text node at the cursor
                // (at the node's first glyph); the glyph of the cursor's own
                // character is then looked up on those pages.
                let found = jump_from_cursor(&self.document, source, offset);
                let first = found.first()?;
                let root = LinkedNode::new(source.root());
                let leaf = root
                    .leaf_at(offset, Side::Before)
                    .filter(is_text)
                    .or_else(|| root.leaf_at(offset, Side::After).filter(is_text))?;
                let within = offset.saturating_sub(leaf.offset());
                let mut best: Option<(usize, usize, Point)> = None;
                for position in &found {
                    let index = position.page.get() - 1;
                    let Some(page) = self.document.pages().get(index) else {
                        continue;
                    };
                    if let Some((glyph, point)) = glyph_position(&page.frame, leaf.span(), within)
                        && best.is_none_or(|(known, ..)| glyph > known)
                    {
                        best = Some((glyph, index, point));
                    }
                }
                let (page, point) = match best {
                    Some((_, page, point)) => (page, point),
                    None => (first.page.get() - 1, first.point),
                };
                Some(PagePosition {
                    page,
                    x_pt: point.x.to_pt() as f32,
                    y_pt: point.y.to_pt() as f32,
                })
            })
    }
}
