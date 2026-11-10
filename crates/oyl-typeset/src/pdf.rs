//! PDF files shown through [`RenderedDocument`], rasterized by hayro: the
//! output of a LaTeX compile, or any PDF the app wants to show.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::PathBuf;
use std::sync::{Arc, OnceLock};

use anyhow::{Result, anyhow, bail};
use hayro::hayro_interpret::InterpreterSettings;
use hayro::hayro_interpret::TransformExt;
use hayro::hayro_syntax::object::{Dict, Object};
use hayro::hayro_syntax::page::{Page, Rotation};
use hayro::hayro_syntax::{LoadPdfError, Pdf};
use hayro::kurbo::Affine;
use hayro::vello_cpu::color::palette::css::WHITE;
use hayro::vello_cpu::{Pixmap, RasterizerSettings, RenderContext, Resources, TargetInit};
use hayro::{RenderCache, RenderSettings, render_into};
use oyl_model::SourceLocation;
use oyl_model::typeset::{PageImage, PagePosition, RenderedDocument};

use crate::latex::synctex::SyncTex;
use crate::raster::{panic_message, pixel_size, premultiplied_onto_white};

/// SyncTeX data of a LaTeX compile: the compressed file, the directory TeX
/// ran in and the project root.
pub(crate) type SyncTexSource = (Vec<u8>, PathBuf, PathBuf);

/// A PDF, loaded once and shared by every render.
pub(crate) struct PdfDocument {
    bytes: Arc<Vec<u8>>,
    pdf: Pdf,
    sizes: Vec<(f32, f32)>,
    fingerprints: Vec<OnceLock<u64>>,
    settings: InterpreterSettings,
    synctex_source: Option<SyncTexSource>,
    synctex: OnceLock<Option<SyncTex>>,
}

impl PdfDocument {
    /// Parses `bytes`; `synctex` enables click-to-source.
    pub(crate) fn open(bytes: Vec<u8>, synctex: Option<SyncTexSource>) -> Result<Self> {
        let bytes = Arc::new(bytes);
        let pdf = Pdf::new(bytes.clone()).map_err(|err| match err {
            LoadPdfError::Decryption(_) => anyhow!("the PDF is encrypted"),
            LoadPdfError::Invalid => anyhow!("the file is not a readable PDF"),
        })?;
        let sizes: Vec<(f32, f32)> = pdf.pages().iter().map(Page::render_dimensions).collect();
        if sizes.is_empty() {
            bail!("the PDF has no pages");
        }
        Ok(Self {
            bytes,
            fingerprints: sizes.iter().map(|_| OnceLock::new()).collect(),
            sizes,
            pdf,
            settings: InterpreterSettings::default(),
            synctex_source: synctex,
            synctex: OnceLock::new(),
        })
    }

    fn synctex(&self) -> Option<&SyncTex> {
        self.synctex
            .get_or_init(|| {
                let (bytes, run_dir, root) = self.synctex_source.as_ref()?;
                SyncTex::from_gz(bytes, run_dir, root)
            })
            .as_ref()
    }

    fn render(&self, index: usize, pixels_per_pt: f32) -> Result<PageImage> {
        let page = self.pdf.pages().get(index).ok_or_else(|| {
            anyhow!(
                "page {} does not exist (the PDF has {} pages)",
                index + 1,
                self.sizes.len()
            )
        })?;
        let (width_pt, height_pt) = self.sizes[index];
        let (width, height) = pixel_size(width_pt, height_pt, pixels_per_pt)?;
        let (Ok(width_px), Ok(height_px)) = (u16::try_from(width), u16::try_from(height)) else {
            bail!("a {width}×{height} px bitmap is too large");
        };

        // The cache borrows the document and is not thread-safe, so every
        // render (possibly on its own thread) has its own.
        let cache = RenderCache::new();
        let mut context = RenderContext::new(width_px, height_px);
        let transform =
            Affine::scale(f64::from(pixels_per_pt)) * page.initial_transform(true).to_kurbo();
        render_into(
            page,
            &cache,
            &self.settings,
            &RenderSettings::default(),
            &mut context,
            transform,
        );
        context.flush();
        let mut pixmap = Pixmap::new(width_px, height_px);
        context.render_with(
            &mut pixmap,
            &mut Resources::default(),
            RasterizerSettings {
                target_init: TargetInit::Clear(WHITE),
                ..Default::default()
            },
        );
        Ok(PageImage {
            width,
            height,
            rgba: premultiplied_onto_white(pixmap.data_as_u8_slice().to_vec()),
        })
    }
}

/// Hashes what a page draws: its content, size, rotation, the resources it
/// uses (fonts, images, forms) and its annotations. Object numbers are not
/// part of the hash, so a page that only moved within the file keeps its
/// fingerprint.
fn page_hash(page: &Page) -> u64 {
    let mut hasher = DefaultHasher::new();
    page.page_stream().hash(&mut hasher);
    let (width, height) = page.render_dimensions();
    width.to_bits().hash(&mut hasher);
    height.to_bits().hash(&mut hasher);
    let rotation: u8 = match page.rotation() {
        Rotation::None => 0,
        Rotation::Horizontal => 1,
        Rotation::Flipped => 2,
        Rotation::FlippedHorizontal => 3,
    };
    rotation.hash(&mut hasher);
    let mut budget = 20_000_usize;
    let resources = page.resources();
    for dict in [
        &resources.fonts,
        &resources.x_objects,
        &resources.ext_g_states,
        &resources.color_spaces,
        &resources.patterns,
        &resources.shadings,
        &resources.properties,
    ] {
        hash_dict(dict, &mut hasher, 6, &mut budget);
    }
    if let Some(annotations) = page.raw().get::<Object>(b"Annots") {
        hash_object(&annotations, &mut hasher, 4, &mut budget);
    }
    hasher.finish()
}

fn hash_dict(dict: &Dict, hasher: &mut DefaultHasher, depth: u8, budget: &mut usize) {
    if depth == 0 || *budget == 0 {
        return;
    }
    for (key, _) in dict.entries() {
        let key: &[u8] = key.as_ref();
        // Back-references to the page tree would pull in the whole file.
        if key == b"Parent" || key == b"P" {
            continue;
        }
        key.hash(hasher);
        if let Some(value) = dict.get::<Object>(key) {
            hash_object(&value, hasher, depth - 1, budget);
        }
    }
}

fn hash_object(object: &Object, hasher: &mut DefaultHasher, depth: u8, budget: &mut usize) {
    if *budget == 0 {
        return;
    }
    *budget -= 1;
    match object {
        Object::Null(_) => 0_u8.hash(hasher),
        Object::Boolean(value) => value.hash(hasher),
        Object::Number(number) => number.as_f64().to_bits().hash(hasher),
        Object::String(string) => string.as_bytes().hash(hasher),
        Object::Name(name) => AsRef::<[u8]>::as_ref(name).hash(hasher),
        Object::Dict(dict) => hash_dict(dict, hasher, depth, budget),
        Object::Array(array) => {
            if depth > 0 {
                for item in array.iter::<Object>() {
                    hash_object(&item, hasher, depth - 1, budget);
                }
            }
        }
        Object::Stream(stream) => {
            hash_dict(stream.dict(), hasher, depth, budget);
            stream.raw_data().hash(hasher);
        }
    }
}

impl RenderedDocument for PdfDocument {
    fn page_count(&self) -> usize {
        self.sizes.len()
    }

    fn page_size_pt(&self, index: usize) -> (f32, f32) {
        self.sizes.get(index).copied().unwrap_or((0.0, 0.0))
    }

    fn render_page(&self, index: usize, pixels_per_pt: f32) -> Result<PageImage> {
        catch_unwind(AssertUnwindSafe(|| self.render(index, pixels_per_pt))).map_err(|payload| {
            anyhow!(
                "rendering page {} failed: {}",
                index + 1,
                panic_message(&*payload)
            )
        })?
    }

    fn pdf(&self) -> Result<Arc<Vec<u8>>> {
        Ok(self.bytes.clone())
    }

    fn page_fingerprint(&self, index: usize) -> u64 {
        let Some(slot) = self.fingerprints.get(index) else {
            return u64::MAX - index as u64;
        };
        *slot.get_or_init(|| {
            let page = self.pdf.pages().get(index);
            catch_unwind(AssertUnwindSafe(|| page.map(page_hash)))
                .ok()
                .flatten()
                // Unhashable: a value that changes with every document.
                .unwrap_or(Arc::as_ptr(&self.bytes) as u64 ^ index as u64)
        })
    }

    fn source_at(&self, position: PagePosition) -> Option<SourceLocation> {
        self.synctex()?
            .source_at(position.page, position.x_pt, position.y_pt)
    }

    fn position_of(&self, location: &SourceLocation) -> Option<PagePosition> {
        self.synctex()?.position_of(&location.file, location.line)
    }
}

/// Opens a PDF for display: page sizes, lazy per-page rasterization at any
/// scale onto white, and the bytes for export. Used for the LaTeX output and
/// for source PDFs.
pub fn rasterize_pdf(bytes: Vec<u8>) -> Result<Arc<dyn RenderedDocument>> {
    Ok(Arc::new(PdfDocument::open(bytes, None)?))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_send_sync<T: Send + Sync>() {}

    #[test]
    fn document_is_shareable() {
        assert_send_sync::<PdfDocument>();
    }

    #[test]
    fn rejects_non_pdfs() {
        assert!(rasterize_pdf(b"hello".to_vec()).is_err());
        assert!(rasterize_pdf(Vec::new()).is_err());
    }
}
