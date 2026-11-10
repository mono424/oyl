//! Reading a PDF's metadata and the text of its first pages, in pure Rust
//! (hayro), to find what paper it is.

use std::panic::{AssertUnwindSafe, catch_unwind};

use hayro_interpret::font::GlyphRun;
use hayro_interpret::hayro_cmap::BfString;
use hayro_interpret::hayro_syntax::Pdf;
use hayro_interpret::hayro_syntax::object::{Dict, Stream};
use hayro_interpret::kurbo::{Affine, BezPath, Point, Rect, Vec2};
use hayro_interpret::{
    BlendMode, ClipPath, Context, Device, DrawMode, DrawProps, Image, ImageDrawProps,
    InterpreterCache, InterpreterSettings, SoftMask, interpret_page,
};

/// What a PDF says about itself.
#[derive(Debug, Default)]
pub(crate) struct PdfText {
    pub title: Option<String>,
    pub author: Option<String>,
    pub subject: Option<String>,
    pub keywords: Option<String>,
    /// The XMP metadata packet, as text.
    pub xmp: Option<String>,
    /// The text of the first pages, one string per page.
    pub pages: Vec<String>,
    pub page_count: usize,
}

/// Reads a PDF's metadata and the text of up to `max_pages` first pages.
/// `None` when the bytes are not a PDF hayro can open. Never panics: a
/// malformed file that trips the reader is treated as unreadable.
pub(crate) fn read(bytes: &[u8], max_pages: usize) -> Option<PdfText> {
    let data = bytes.to_vec();
    catch_unwind(AssertUnwindSafe(|| read_inner(data, max_pages)))
        .ok()
        .flatten()
}

fn read_inner(data: Vec<u8>, max_pages: usize) -> Option<PdfText> {
    let pdf = Pdf::new(data).ok()?;
    let meta = pdf.metadata();
    let mut out = PdfText {
        title: meta
            .title
            .as_deref()
            .map(pdf_string)
            .filter(|s| !s.is_empty()),
        author: meta
            .author
            .as_deref()
            .map(pdf_string)
            .filter(|s| !s.is_empty()),
        subject: meta
            .subject
            .as_deref()
            .map(pdf_string)
            .filter(|s| !s.is_empty()),
        keywords: meta
            .keywords
            .as_deref()
            .map(pdf_string)
            .filter(|s| !s.is_empty()),
        ..PdfText::default()
    };
    let xref = pdf.xref();
    out.xmp = xref
        .get::<Dict<'_>>(xref.root_id())
        .and_then(|catalog| catalog.get::<Stream<'_>>(b"Metadata"))
        .and_then(|s| s.decoded().ok())
        .map(|bytes| String::from_utf8_lossy(&bytes).into_owned());
    let pages = pdf.pages();
    out.page_count = pages.len();
    let settings = InterpreterSettings::default();
    let cache = InterpreterCache::new();
    for page in pages.iter().take(max_pages) {
        let mut context = Context::new(
            Affine::IDENTITY,
            Rect::new(0.0, 0.0, 1.0, 1.0),
            &cache,
            xref,
            settings.clone(),
        );
        let mut text = TextDevice::default();
        interpret_page(page, &mut context, &mut text);
        out.pages.push(text.finish());
    }
    Some(out)
}

/// The size of the first page, in points (rotation applied).
pub(crate) fn page_size(bytes: &[u8]) -> Option<(u32, u32)> {
    let data = bytes.to_vec();
    catch_unwind(AssertUnwindSafe(|| {
        let pdf = Pdf::new(data).ok()?;
        let page = pdf.pages().iter().next()?;
        let (w, h) = page.render_dimensions();
        (w > 0.0 && h > 0.0).then(|| (w.round() as u32, h.round() as u32))
    }))
    .ok()
    .flatten()
}

/// A PDF text string: UTF-16 with a byte-order mark, UTF-8 with one, or
/// PDFDocEncoding (read as Latin-1, which it matches for letters).
fn pdf_string(bytes: &[u8]) -> String {
    let s = if let Some(rest) = bytes.strip_prefix(&[0xFE, 0xFF]) {
        let units: Vec<u16> = rest
            .as_chunks::<2>()
            .0
            .iter()
            .map(|c| u16::from_be_bytes([c[0], c[1]]))
            .collect();
        String::from_utf16_lossy(&units)
    } else if let Some(rest) = bytes.strip_prefix(&[0xFF, 0xFE]) {
        let units: Vec<u16> = rest
            .as_chunks::<2>()
            .0
            .iter()
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .collect();
        String::from_utf16_lossy(&units)
    } else if let Some(rest) = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]) {
        String::from_utf8_lossy(rest).into_owned()
    } else if let Ok(s) = std::str::from_utf8(bytes) {
        s.to_string()
    } else {
        bytes.iter().map(|&b| b as char).collect()
    };
    s.trim_matches(char::from(0))
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// Glyph-space units per em in hayro's glyph transforms.
const GLYPH_UNITS: f64 = 1000.0;

/// Rough advance widths, in ems, to tell a word space from the next letter
/// when the PDF positions words instead of drawing spaces.
fn advance(c: char) -> f64 {
    match c {
        'i' | 'j' | 'l' | '.' | ',' | ':' | ';' | '\'' | '!' | '|' | ' ' => 0.28,
        'f' | 't' | 'r' | '(' | ')' | '[' | ']' | '/' | '-' | 'I' | 'J' => 0.36,
        'm' | 'w' | 'M' | 'W' => 0.86,
        c if c.is_ascii_uppercase() => 0.68,
        c if c.is_ascii_digit() => 0.5,
        c if c.is_ascii_lowercase() => 0.5,
        _ => 0.6,
    }
}

/// Letters whose advance tells a proportional font from a monospaced one.
const NARROW: &str = "iljtfr.,:;1'!I";
const WIDE: &str = "mwMWOQDGHNU@%";

/// One drawn glyph, in page space.
struct PlacedGlyph {
    text: String,
    origin: Point,
    /// Unit vector of the advance direction.
    dir: Vec2,
    /// Font size in page units.
    size: f64,
}

impl PlacedGlyph {
    fn last_char(&self) -> char {
        self.text.chars().next_back().unwrap_or(' ')
    }

    fn is_space(&self) -> bool {
        self.text.chars().all(char::is_whitespace)
    }

    /// How far `next` is along this glyph's line, or `None` when it starts
    /// a new line.
    fn along(&self, next: &PlacedGlyph) -> Option<f64> {
        let d = next.origin - self.origin;
        let em = self.size.max(next.size);
        if (next.dir - self.dir).hypot() > 0.1 || d.cross(self.dir).abs() > 0.6 * em {
            None
        } else {
            Some(d.dot(self.dir))
        }
    }
}

fn size_key(size: f64) -> i64 {
    (size * 2.0).round() as i64
}

fn median(values: &mut [f64]) -> Option<f64> {
    if values.is_empty() {
        return None;
    }
    values.sort_by(f64::total_cmp);
    Some(values[values.len() / 2])
}

/// Collects the glyphs of a page, then writes them out as lines: spaces
/// where the gap is wider than the letter before it, line breaks where the
/// baseline moves.
#[derive(Default)]
struct TextDevice {
    glyphs: Vec<PlacedGlyph>,
}

impl TextDevice {
    fn push(&mut self, s: &str, transform: Affine) {
        // Glyph space has 1000 units per em; the vertical em is the font
        // size (the horizontal one is stretched to the font's widths).
        let origin = transform * Point::ZERO;
        let x = transform * Point::new(GLYPH_UNITS, 0.0) - origin;
        let y = transform * Point::new(0.0, GLYPH_UNITS) - origin;
        let size = y.hypot();
        let width = x.hypot();
        if size <= f64::EPSILON || width <= f64::EPSILON || !size.is_finite() || !width.is_finite()
        {
            return;
        }
        self.glyphs.push(PlacedGlyph {
            text: s.to_string(),
            origin,
            dir: x / width,
            size,
        });
    }

    /// Font sizes set in a monospaced font, with their advance in ems: where
    /// narrow and wide letters advance alike.
    fn monospaced(&self) -> std::collections::HashMap<i64, f64> {
        use std::collections::HashMap;
        let mut narrow: HashMap<i64, Vec<f64>> = HashMap::new();
        let mut wide: HashMap<i64, Vec<f64>> = HashMap::new();
        let mut all: HashMap<i64, Vec<f64>> = HashMap::new();
        for pair in self.glyphs.windows(2) {
            let (a, b) = (&pair[0], &pair[1]);
            if a.is_space() || b.is_space() {
                continue;
            }
            let Some(along) = a.along(b) else { continue };
            let em = along / a.size;
            if !(0.05..1.5).contains(&em) {
                continue;
            }
            let key = size_key(a.size);
            all.entry(key).or_default().push(em);
            let c = a.last_char();
            if NARROW.contains(c) {
                narrow.entry(key).or_default().push(em);
            } else if WIDE.contains(c) {
                wide.entry(key).or_default().push(em);
            }
        }
        let mut out = HashMap::new();
        for (key, mut values) in all {
            let n = narrow.get_mut(&key).and_then(|v| median(v));
            let w = wide.get_mut(&key).and_then(|v| median(v));
            if let (Some(n), Some(w)) = (n, w)
                && (n - w).abs() / w < 0.1
                && let Some(m) = median(&mut values)
            {
                out.insert(key, m);
            }
        }
        out
    }

    fn finish(self) -> String {
        let mono = self.monospaced();
        let g = &self.glyphs;
        // How far each glyph is from the one before it on its line.
        let steps: Vec<Option<f64>> = g
            .windows(2)
            .map(|w| {
                if w[0].is_space() || w[1].is_space() {
                    None
                } else {
                    w[0].along(&w[1])
                }
            })
            .collect();
        let mut out = String::new();
        for (i, glyph) in g.iter().enumerate() {
            if i > 0 {
                let p = &g[i - 1];
                match p.along(glyph) {
                    None => out.push('\n'),
                    Some(along) => {
                        let em = p.size.max(glyph.size);
                        let adv = match mono.get(&size_key(p.size)) {
                            Some(m) => m * p.text.chars().count() as f64,
                            None => p.text.chars().map(advance).sum(),
                        } * p.size;
                        // Typewriter text inside proportional text: the step
                        // matches the steps on both sides, so it is no word gap.
                        let like = |j: Option<usize>| {
                            j.and_then(|j| steps.get(j).copied().flatten())
                                .is_some_and(|s| (s - along).abs() <= 0.03 * em)
                        };
                        let even = like(i.checked_sub(2)) && like(Some(i));
                        let gap = along < -0.5 * em || (along - adv > 0.18 * em && !even);
                        if gap && !p.is_space() && !glyph.is_space() {
                            out.push(' ');
                        }
                    }
                }
            }
            out.push_str(&glyph.text);
        }
        // Tidy: single spaces, no trailing ones, no empty lines at the end.
        let lines: Vec<String> = out
            .lines()
            .map(|l| l.split_whitespace().collect::<Vec<_>>().join(" "))
            .collect();
        let end = lines
            .iter()
            .rposition(|l| !l.is_empty())
            .map(|i| i + 1)
            .unwrap_or(0);
        lines[..end].join("\n")
    }
}

impl<'a> Device<'a> for TextDevice {
    fn draw_path(&mut self, _: &BezPath, _: DrawProps<'a>, _: &DrawMode) {}
    fn push_clip_path(&mut self, _: &ClipPath) {}
    fn push_transparency_group(&mut self, _: f32, _: Option<SoftMask<'a>>, _: BlendMode) {}
    fn draw_glyph_run(&mut self, run: &GlyphRun<'_, 'a>, props: DrawProps<'a>, _: &DrawMode) {
        for glyph in run.glyphs() {
            let text = match glyph.as_unicode() {
                Some(BfString::Char(c)) => c.to_string(),
                Some(BfString::String(s)) => s,
                None => continue,
            };
            self.push(&text, props.transform * glyph.transform());
        }
    }
    fn draw_image(&mut self, _: Image<'a, '_>, _: ImageDrawProps<'a>) {}
    fn pop_clip(&mut self) {}
    fn pop_transparency_group(&mut self) {}
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A one-page PDF with Helvetica text lines and an Info dictionary,
    /// built by hand with a correct cross-reference table.
    pub(crate) fn tiny_pdf(lines: &[&str], title: Option<&str>) -> Vec<u8> {
        let mut content = String::from("BT /F1 11 Tf 72 720 Td 14 TL\n");
        for line in lines {
            if line.starts_with('[') {
                content.push_str(&format!("{line} TJ T*\n"));
            } else {
                content.push_str(&format!(
                    "({}) Tj T*\n",
                    line.replace('(', "\\(").replace(')', "\\)")
                ));
            }
        }
        content.push_str("ET\n");
        let info = match title {
            Some(t) => format!("<< /Title ({t}) /Author (Jane Doe; John Roe) >>"),
            None => "<< >>".to_string(),
        };
        let objects = [
            "<< /Type /Catalog /Pages 2 0 R >>".to_string(),
            "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_string(),
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << /Font << /F1 5 0 R >> >> /Contents 4 0 R >>".to_string(),
            format!("<< /Length {} >>\nstream\n{content}endstream", content.len()),
            "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>".to_string(),
            info,
        ];
        let mut out = b"%PDF-1.4\n".to_vec();
        let mut offsets = Vec::new();
        for (i, obj) in objects.iter().enumerate() {
            offsets.push(out.len());
            out.extend_from_slice(format!("{} 0 obj\n{obj}\nendobj\n", i + 1).as_bytes());
        }
        let xref = out.len();
        out.extend_from_slice(
            format!("xref\n0 {}\n0000000000 65535 f \n", objects.len() + 1).as_bytes(),
        );
        for o in offsets {
            out.extend_from_slice(format!("{o:010} 00000 n \n").as_bytes());
        }
        out.extend_from_slice(
            format!(
                "trailer\n<< /Size {} /Root 1 0 R /Info 6 0 R >>\nstartxref\n{xref}\n%%EOF\n",
                objects.len() + 1
            )
            .as_bytes(),
        );
        out
    }

    #[test]
    fn reads_text_and_metadata() {
        let pdf = tiny_pdf(
            &[
                "A Study of Things",
                "[(Jane)-333(Doe)] ",
                "DOI: 10.1109/CVPR.2016.90",
            ],
            Some("A Study of Things"),
        );
        let t = read(&pdf, 2).unwrap();
        assert_eq!(t.page_count, 1);
        assert_eq!(t.title.as_deref(), Some("A Study of Things"));
        assert_eq!(t.author.as_deref(), Some("Jane Doe; John Roe"));
        let page = &t.pages[0];
        assert!(page.contains("A Study of Things"), "{page:?}");
        assert!(page.contains("Jane Doe"), "{page:?}");
        assert!(page.contains("DOI: 10.1109/CVPR.2016.90"), "{page:?}");
        assert_eq!(page.lines().count(), 3, "{page:?}");
    }

    #[test]
    fn reads_a_core_graphics_pdf() {
        // Made by macOS's cgtexttopdf: embedded subset TrueType fonts.
        let t = read(include_bytes!("../tests/fixtures/quartz_doi.pdf"), 2).unwrap();
        let page = &t.pages[0];
        let lines: Vec<&str> = page.lines().collect();
        assert_eq!(
            lines[0], "Deep Residual Learning for Image Recognition",
            "{page:?}"
        );
        assert_eq!(
            lines[1], "Kaiming He, Xiangyu Zhang, Shaoqing Ren, Jian Sun",
            "{page:?}"
        );
        assert!(page.contains("DOI: 10.1109/CVPR.2016.90"), "{page:?}");
    }

    #[test]
    fn reads_a_pdftex_pdf() {
        // Made by pdfLaTeX: Type 1 fonts, words placed by kerning, no space glyphs.
        let t = read(include_bytes!("../tests/fixtures/pdftex_doi.pdf"), 2).unwrap();
        let page = &t.pages[0];
        assert!(
            page.contains("Deep Residual Learning for Image Recognition"),
            "{page:?}"
        );
        assert!(
            page.contains("Deeper neural networks are more difficult to train."),
            "{page:?}"
        );
        assert!(
            page.contains("arXiv:1512.03385v1 [cs.CV] 10 Dec 2015."),
            "{page:?}"
        );
        assert!(page.contains("DOI: 10.1109/CVPR.2016.90"), "{page:?}");
    }

    #[test]
    fn not_a_pdf() {
        assert!(read(b"hello", 1).is_none());
        assert!(read(b"%PDF-1.4\ngarbage", 1).is_none_or(|t| t.pages.is_empty()));
    }

    #[test]
    fn text_strings() {
        assert_eq!(pdf_string(&[0xFE, 0xFF, 0, b'H', 0, b'i']), "Hi");
        assert_eq!(pdf_string(b"Caf\xe9"), "Café");
        assert_eq!(pdf_string(b"  two  words "), "two words");
    }
}
