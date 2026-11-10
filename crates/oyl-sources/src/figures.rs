//! The paper's figures, kept in `<root>/figures/`.
//!
//! Imports (a file picked, dropped or pasted) get readable unique names,
//! photos larger than [`MAX_SIDE`] pixels are scaled down, rotated photos
//! are turned upright, and formats the engines cannot all read (HEIC, HEIF,
//! AVIF, TIFF, GIF, WebP, BMP) are converted with macOS's `sips` to JPEG
//! (photos) or PNG (everything else). PDF, SVG and EPS are copied as they are.

use std::borrow::Cow;
use std::fs;
use std::io::{Cursor, ErrorKind};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::LazyLock;
use std::sync::atomic::{AtomicU64, Ordering};

use image::codecs::jpeg::JpegEncoder;
use image::imageops::FilterType;
use image::metadata::Orientation;
use image::{DynamicImage, ImageDecoder, ImageFormat, ImageReader};
use oyl_model::Engine;
use regex::Regex;
use serde::{Deserialize, Serialize};
use walkdir::WalkDir;

use crate::citations::escape_typst;
use crate::error::{Error, Result};
use crate::fsutil::{rel_path, slug, write_atomic};
use crate::latex::escape;

/// The folder figures are kept in, inside the paper folder.
pub const FIGURES_DIR: &str = "figures";

/// The longest side, in pixels, an imported raster image keeps.
pub const MAX_SIDE: u32 = 3200;

/// File extensions [`list`] shows.
pub const EXTENSIONS: [&str; 14] = [
    "png", "jpg", "jpeg", "gif", "webp", "tif", "tiff", "bmp", "heic", "heif", "avif", "pdf",
    "svg", "eps",
];

/// An image in the figures folder.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Figure {
    /// Absolute path.
    pub path: PathBuf,
    /// Path relative to the paper folder, with `/` (`figures/model.png`).
    pub rel_path: String,
    /// Pixels for raster images, points for PDF, EPS and SVG; `None` when
    /// unknown.
    pub width: Option<u32>,
    /// Height, in the same unit as `width`.
    pub height: Option<u32>,
    /// Size on disk.
    pub bytes: u64,
    /// Whether any of the paper's source files includes it.
    pub used: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Png,
    Jpeg,
    Gif,
    Webp,
    Tiff,
    Bmp,
    Heic,
    Avif,
    Pdf,
    Svg,
    Eps,
}

/// What the bytes are, from their first bytes.
fn sniff(bytes: &[u8]) -> Option<Kind> {
    let b = bytes;
    if b.starts_with(b"\x89PNG\r\n\x1a\n") {
        return Some(Kind::Png);
    }
    if b.starts_with(&[0xFF, 0xD8, 0xFF]) {
        return Some(Kind::Jpeg);
    }
    if b.starts_with(b"GIF87a") || b.starts_with(b"GIF89a") {
        return Some(Kind::Gif);
    }
    if b.len() >= 12 && &b[..4] == b"RIFF" && &b[8..12] == b"WEBP" {
        return Some(Kind::Webp);
    }
    if b.starts_with(b"II*\0") || b.starts_with(b"MM\0*") {
        return Some(Kind::Tiff);
    }
    if b.starts_with(b"BM") && b.len() > 14 {
        return Some(Kind::Bmp);
    }
    if b.len() >= 12 && &b[4..8] == b"ftyp" {
        return match &b[8..12] {
            b"avif" | b"avis" => Some(Kind::Avif),
            b"heic" | b"heix" | b"hevc" | b"hevx" | b"heim" | b"heis" | b"hevm" | b"hevs"
            | b"mif1" | b"msf1" => Some(Kind::Heic),
            _ => None,
        };
    }
    if b.windows(5).take(1024).any(|w| w == b"%PDF-") {
        return Some(Kind::Pdf);
    }
    if b.starts_with(b"%!PS") || b.starts_with(&[0xC5, 0xD0, 0xD3, 0xC6]) {
        return Some(Kind::Eps);
    }
    let head = String::from_utf8_lossy(&b[..b.len().min(4096)]);
    let t = head.trim_start_matches('\u{feff}').trim_start();
    if (t.starts_with('<')) && head.contains("<svg") {
        return Some(Kind::Svg);
    }
    None
}

fn from_ext(ext: &str) -> Option<Kind> {
    Some(
        match ext.trim_start_matches('.').to_ascii_lowercase().as_str() {
            "png" => Kind::Png,
            "jpg" | "jpeg" | "jpe" => Kind::Jpeg,
            "gif" => Kind::Gif,
            "webp" => Kind::Webp,
            "tif" | "tiff" => Kind::Tiff,
            "bmp" => Kind::Bmp,
            "heic" | "heif" | "hif" => Kind::Heic,
            "avif" => Kind::Avif,
            "pdf" => Kind::Pdf,
            "svg" => Kind::Svg,
            "eps" | "ps" => Kind::Eps,
            _ => return None,
        },
    )
}

fn kind_ext(kind: Kind) -> &'static str {
    match kind {
        Kind::Png => "png",
        Kind::Jpeg => "jpg",
        Kind::Gif => "gif",
        Kind::Webp => "webp",
        Kind::Tiff => "tiff",
        Kind::Bmp => "bmp",
        Kind::Heic => "heic",
        Kind::Avif => "avif",
        Kind::Pdf => "pdf",
        Kind::Svg => "svg",
        Kind::Eps => "eps",
    }
}

/// Imports an image file into the figures folder. Returns the new figure
/// (or the existing one, when the same image is already there).
pub fn import_file(root: &Path, src: &Path) -> Result<Figure> {
    let bytes = fs::read(src).map_err(|e| Error::io("read", src, e))?;
    let stem = src.file_stem().map(|s| s.to_string_lossy().into_owned());
    let ext = src
        .extension()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    import_bytes(root, &bytes, stem.as_deref(), &ext)
}

/// Imports image bytes (a paste, a drop) into the figures folder, named
/// after `name_hint` ("figure" without one). `ext` is what the bytes claim
/// to be; the bytes themselves decide when they tell.
pub fn import_bytes(
    root: &Path,
    bytes: &[u8],
    name_hint: Option<&str>,
    ext: &str,
) -> Result<Figure> {
    let kind = sniff(bytes).or_else(|| from_ext(ext)).ok_or_else(|| {
        Error::Invalid("This is not an image Oyl can use: add PNG, JPEG, PDF, SVG, EPS, HEIC, TIFF, GIF or WebP.".into())
    })?;
    let stem = name_hint
        .map(slug)
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "figure".to_string());
    let (data, out_ext): (Cow<'_, [u8]>, &str) = match kind {
        Kind::Pdf | Kind::Svg | Kind::Eps => (Cow::Borrowed(bytes), kind_ext(kind)),
        Kind::Png => (prepare_raster(bytes, ImageFormat::Png)?, "png"),
        Kind::Jpeg => (prepare_raster(bytes, ImageFormat::Jpeg)?, "jpg"),
        Kind::Heic | Kind::Avif => {
            let jpeg = sips_convert(root, bytes, kind_ext(kind), "jpeg")?;
            (
                Cow::Owned(prepare_raster(&jpeg, ImageFormat::Jpeg)?.into_owned()),
                "jpg",
            )
        }
        Kind::Tiff | Kind::Gif | Kind::Webp | Kind::Bmp => {
            let png = sips_convert(root, bytes, kind_ext(kind), "png")?;
            (
                Cow::Owned(prepare_raster(&png, ImageFormat::Png)?.into_owned()),
                "png",
            )
        }
    };
    let path = place(root, &stem, out_ext, &data)?;
    figure_at(root, &path, false)
}

/// The image as it should be stored: unchanged when it is upright and small
/// enough, else turned upright, scaled to [`MAX_SIDE`] and re-encoded in
/// the same format.
fn prepare_raster(bytes: &[u8], format: ImageFormat) -> Result<Cow<'_, [u8]>> {
    let unreadable =
        |e: image::ImageError| Error::Image(format!("This image could not be opened: {e}."));
    let mut decoder = ImageReader::with_format(Cursor::new(bytes), format)
        .into_decoder()
        .map_err(unreadable)?;
    let (w, h) = decoder.dimensions();
    let orientation = decoder.orientation().unwrap_or(Orientation::NoTransforms);
    if w.max(h) <= MAX_SIDE && orientation == Orientation::NoTransforms {
        return Ok(Cow::Borrowed(bytes));
    }
    let mut img = DynamicImage::from_decoder(decoder).map_err(unreadable)?;
    img.apply_orientation(orientation);
    if img.width().max(img.height()) > MAX_SIDE {
        img = img.resize(MAX_SIDE, MAX_SIDE, FilterType::CatmullRom);
    }
    let mut out = Vec::new();
    let failed =
        |e: image::ImageError| Error::Image(format!("The image could not be written: {e}."));
    match format {
        ImageFormat::Jpeg => {
            // JPEG has no transparency: what was clear prints as the page, white.
            let rgb = if img.color().has_alpha() {
                let rgba = img.to_rgba8();
                let mut flat = image::RgbImage::new(rgba.width(), rgba.height());
                for (x, y, p) in rgba.enumerate_pixels() {
                    let a = u16::from(p[3]);
                    let mix = |c: u8| ((u16::from(c) * a + 255 * (255 - a)) / 255) as u8;
                    flat.put_pixel(x, y, image::Rgb([mix(p[0]), mix(p[1]), mix(p[2])]));
                }
                flat
            } else {
                img.to_rgb8()
            };
            JpegEncoder::new_with_quality(&mut out, 90)
                .encode_image(&rgb)
                .map_err(failed)?;
        }
        _ => img
            .write_to(&mut Cursor::new(&mut out), ImageFormat::Png)
            .map_err(failed)?,
    }
    Ok(Cow::Owned(out))
}

/// Converts with macOS's `sips` (`format` is `jpeg` or `png`), through
/// work files in `<root>/.oyl/tmp`.
fn sips_convert(root: &Path, bytes: &[u8], in_ext: &str, format: &str) -> Result<Vec<u8>> {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let work = root.join(".oyl").join("tmp");
    fs::create_dir_all(&work).map_err(|e| Error::io("create", &work, e))?;
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let base = format!("convert-{}-{n}", std::process::id());
    let input = work.join(format!("{base}.{in_ext}"));
    let out_ext = if format == "jpeg" { "jpg" } else { "png" };
    let output = work.join(format!("{base}-out.{out_ext}"));
    fs::write(&input, bytes).map_err(|e| Error::io("write", &input, e))?;
    let sips = if Path::new("/usr/bin/sips").exists() {
        "/usr/bin/sips"
    } else {
        "sips"
    };
    let mut cmd = Command::new(sips);
    cmd.args(["-s", "format", format]);
    if format == "jpeg" {
        cmd.args(["-s", "formatOptions", "90"]);
    }
    let result = cmd.arg(&input).arg("--out").arg(&output).output();
    let converted = fs::read(&output);
    let _ = fs::remove_file(&input);
    let _ = fs::remove_file(&output);
    let _ = fs::remove_dir(&work);
    let run = result.map_err(|e| {
        Error::Image(format!(
            "Converting {} images needs macOS's sips, which could not run ({e}).",
            in_ext.to_uppercase()
        ))
    })?;
    match converted {
        Ok(data) if run.status.success() && !data.is_empty() => Ok(data),
        _ => Err(Error::Image(format!(
            "This {} image could not be converted: {}",
            in_ext.to_uppercase(),
            String::from_utf8_lossy(&run.stderr).trim()
        ))),
    }
}

/// Stores `data` as `figures/<stem>.<ext>`, or `<stem>-2.<ext>`, … when the
/// name is taken by a different file. An identical file already there is
/// reused.
fn place(root: &Path, stem: &str, ext: &str, data: &[u8]) -> Result<PathBuf> {
    let dir = root.join(FIGURES_DIR);
    fs::create_dir_all(&dir).map_err(|e| Error::io("create", &dir, e))?;
    for n in 1u32.. {
        let name = if n == 1 {
            format!("{stem}.{ext}")
        } else {
            format!("{stem}-{n}.{ext}")
        };
        let path = dir.join(name);
        match fs::metadata(&path) {
            Err(e) if e.kind() == ErrorKind::NotFound => {
                write_atomic(&path, data)?;
                return Ok(path);
            }
            Err(e) => return Err(Error::io("read", &path, e)),
            Ok(meta) => {
                if meta.len() == data.len() as u64
                    && fs::read(&path).map_err(|e| Error::io("read", &path, e))? == data
                {
                    return Ok(path);
                }
            }
        }
    }
    Err(Error::Invalid(
        "No free file name was left for the figure.".into(),
    ))
}

/// The figure at `path`, with its size and dimensions.
fn figure_at(root: &Path, path: &Path, used: bool) -> Result<Figure> {
    let meta = fs::metadata(path).map_err(|e| Error::io("read", path, e))?;
    let ext = path
        .extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    let dims = match ext.as_str() {
        "svg" => fs::read_to_string(path).ok().and_then(|t| svg_size(&t)),
        "pdf" => fs::read(path)
            .ok()
            .and_then(|b| crate::pdftext::page_size(&b)),
        "eps" | "ps" => fs::read(path).ok().and_then(|b| eps_size(&b)),
        _ => imagesize::size(path)
            .ok()
            .and_then(|s| Some((u32::try_from(s.width).ok()?, u32::try_from(s.height).ok()?))),
    };
    Ok(Figure {
        path: path.to_path_buf(),
        rel_path: rel_path(root, path).unwrap_or_else(|| path.to_string_lossy().into_owned()),
        width: dims.map(|d| d.0),
        height: dims.map(|d| d.1),
        bytes: meta.len(),
        used,
    })
}

fn svg_size(svg: &str) -> Option<(u32, u32)> {
    static TAG: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"(?is)<svg\b([^>]*)>").expect("valid regex"));
    static ATTR: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r#"(?i)\b(width|height|viewBox)\s*=\s*["']([^"']*)["']"#).expect("valid regex")
    });
    let tag = TAG.captures(svg)?;
    let (mut w, mut h, mut view) = (None, None, None);
    for a in ATTR.captures_iter(&tag[1]) {
        let value = a[2].trim().to_string();
        match a[1].to_ascii_lowercase().as_str() {
            "width" => w = length(&value),
            "height" => h = length(&value),
            _ => view = Some(value),
        }
    }
    if let (Some(w), Some(h)) = (w, h) {
        return Some((w, h));
    }
    let nums: Vec<f64> = view?
        .split([' ', ','])
        .filter(|s| !s.is_empty())
        .filter_map(|s| s.parse().ok())
        .collect();
    (nums.len() == 4 && nums[2] > 0.0 && nums[3] > 0.0)
        .then(|| (nums[2].round() as u32, nums[3].round() as u32))
}

/// An SVG length in CSS pixels; `None` for relative units.
fn length(value: &str) -> Option<u32> {
    let v = value.trim();
    let split = v
        .find(|c: char| !(c.is_ascii_digit() || c == '.'))
        .unwrap_or(v.len());
    let n: f64 = v[..split].parse().ok()?;
    let px = match v[split..].trim() {
        "" | "px" => n,
        "pt" => n * 96.0 / 72.0,
        "in" => n * 96.0,
        "cm" => n * 96.0 / 2.54,
        "mm" => n * 96.0 / 25.4,
        _ => return None,
    };
    (px > 0.0).then(|| px.round() as u32)
}

fn eps_size(bytes: &[u8]) -> Option<(u32, u32)> {
    let head = String::from_utf8_lossy(&bytes[..bytes.len().min(8192)]);
    let line = head
        .lines()
        .find(|l| l.starts_with("%%BoundingBox:") && !l.contains("atend"))?;
    let n: Vec<f64> = line["%%BoundingBox:".len()..]
        .split_whitespace()
        .filter_map(|s| s.parse().ok())
        .collect();
    (n.len() == 4).then(|| {
        (
            (n[2] - n[0]).abs().round() as u32,
            (n[3] - n[1]).abs().round() as u32,
        )
    })
}

/// The figures in `<root>/figures/` (subfolders included, hidden files
/// left out), sorted by path, each marked used when one of `source_texts`
/// includes it (`\includegraphics{…}`, `image("…")`, or its file name).
pub fn list<S: AsRef<str>>(root: &Path, source_texts: &[S]) -> Result<Vec<Figure>> {
    let dir = root.join(FIGURES_DIR);
    if !dir.is_dir() {
        return Ok(Vec::new());
    }
    let refs = references(source_texts);
    let mut out = Vec::new();
    let walker = WalkDir::new(&dir)
        .follow_links(false)
        .into_iter()
        .filter_entry(|e| e.depth() == 0 || !e.file_name().to_string_lossy().starts_with('.'));
    for entry in walker {
        let entry = match entry {
            Ok(e) => e,
            Err(e) => {
                log::warn!("skipping part of the figures folder: {e}");
                continue;
            }
        };
        let path = entry.path();
        let known = path.extension().is_some_and(|e| {
            EXTENSIONS.contains(&e.to_string_lossy().to_ascii_lowercase().as_str())
        });
        if !entry.file_type().is_file() || !known {
            continue;
        }
        let mut fig = figure_at(root, path, false)?;
        fig.used = is_used(&fig, &dir, &refs, source_texts);
        out.push(fig);
    }
    out.sort_by(|a, b| a.rel_path.cmp(&b.rel_path));
    Ok(out)
}

/// The image paths the texts include, normalized: forward slashes, no
/// leading `./` or `../`, no extension, lowercase.
fn references<S: AsRef<str>>(texts: &[S]) -> Vec<String> {
    static INCLUDE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"\\(?:includegraphics|includesvg|includepdf|includestandalone)\*?\s*(?:\[[^\]]*\]\s*)*\{([^}]+)\}")
            .expect("valid regex")
    });
    static TYPST: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r#"\b(?:image|read)\(\s*"([^"]+)""#).expect("valid regex"));
    let mut out = Vec::new();
    for text in texts {
        let text = text.as_ref();
        for c in INCLUDE.captures_iter(text).chain(TYPST.captures_iter(text)) {
            out.push(normalize_ref(&c[1]));
        }
    }
    out
}

fn normalize_ref(r: &str) -> String {
    let mut s = r.trim().replace('\\', "/");
    loop {
        if let Some(rest) = s.strip_prefix("./") {
            s = rest.to_string();
        } else if let Some(rest) = s.strip_prefix("../") {
            s = rest.to_string();
        } else if let Some(rest) = s.strip_prefix('/') {
            s = rest.to_string();
        } else {
            break;
        }
    }
    strip_ext(&s).to_lowercase()
}

fn strip_ext(path: &str) -> &str {
    let name_start = path.rfind('/').map(|i| i + 1).unwrap_or(0);
    match path[name_start..].rfind('.') {
        Some(dot) if dot > 0 => &path[..name_start + dot],
        _ => path,
    }
}

fn is_used<S: AsRef<str>>(fig: &Figure, dir: &Path, refs: &[String], texts: &[S]) -> bool {
    let rel = strip_ext(&fig.rel_path).to_lowercase();
    let in_dir = rel_path(dir, &fig.path)
        .map(|p| strip_ext(&p).to_lowercase())
        .unwrap_or_default();
    let by_ref = refs.iter().any(|r| {
        *r == rel
            || *r == in_dir
            || r.ends_with(&format!("/{rel}"))
            || (!in_dir.is_empty() && r.ends_with(&format!("/{in_dir}")))
    });
    if by_ref {
        return true;
    }
    let name = fig
        .path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    !name.is_empty() && texts.iter().any(|t| t.as_ref().contains(&name))
}

/// A figure block to insert for an image: a LaTeX `figure` environment with
/// `\includegraphics[width=\linewidth]`, caption and label, or a Typst
/// `#figure(image(…), caption: […]) <label>`. `caption` is plain text
/// (escaped for the engine); without a `label`, one is made from the file
/// name (`fig:model`). `rel_path` is the path as the source file includes it.
pub fn snippet(engine: Engine, rel_path: &str, caption: &str, label: Option<&str>) -> String {
    let stem = rel_path
        .rsplit('/')
        .next()
        .map(strip_ext)
        .unwrap_or(rel_path);
    let label = label
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(|l| {
            l.chars()
                .map(|c| {
                    if c.is_alphanumeric() || matches!(c, '_' | '-' | ':' | '.') {
                        c
                    } else {
                        '-'
                    }
                })
                .collect::<String>()
        })
        .unwrap_or_else(|| {
            let s = slug(stem);
            format!(
                "fig:{}",
                if s.is_empty() {
                    "figure".to_string()
                } else {
                    s
                }
            )
        });
    let caption = caption.split_whitespace().collect::<Vec<_>>().join(" ");
    match engine {
        Engine::Latex => {
            let include = if rel_path.to_ascii_lowercase().ends_with(".svg") {
                format!("\\includesvg[width=\\linewidth]{{{}}}", strip_ext(rel_path))
            } else {
                format!("\\includegraphics[width=\\linewidth]{{{rel_path}}}")
            };
            format!(
                "\\begin{{figure}}[htbp]\n  \\centering\n  {include}\n  \\caption{{{}}}\n  \\label{{{label}}}\n\\end{{figure}}",
                escape(&caption)
            )
        }
        Engine::Typst => {
            let path = rel_path.replace('\\', "\\\\").replace('"', "\\\"");
            let caption = if caption.is_empty() {
                String::new()
            } else {
                format!(", caption: [{}]", escape_typst(&caption))
            };
            format!("#figure(image(\"{path}\", width: 80%){caption}) <{label}>")
        }
    }
}

/// Text with LaTeX comments blanked out (same length, so offsets hold).
fn mask_comments(text: &str) -> String {
    let mut out = text.as_bytes().to_vec();
    let b = text.as_bytes();
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'\\' => i += 2,
            b'%' => {
                while i < b.len() && b[i] != b'\n' {
                    out[i] = b' ';
                    i += 1;
                }
            }
            _ => i += 1,
        }
    }
    String::from_utf8(out).unwrap_or_else(|_| text.to_string())
}

/// The main LaTeX file with `\usepackage{graphicx}` added to its preamble
/// when the preamble lacks it (and `\usepackage{svg}` when the text uses
/// `\includesvg` without it). `None` when nothing needs adding, for Typst,
/// and for text without a `\documentclass` preamble.
pub fn ensure_preamble(engine: Engine, main_text: &str) -> Option<String> {
    static CLASS: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"\\documentclass\s*(?:\[[^\]]*\])?\s*\{[^}]*\}").expect("valid regex")
    });
    static PACKAGE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"\\(?:usepackage|RequirePackage)\s*(?:\[[^\]]*\])?\s*\{([^}]*)\}")
            .expect("valid regex")
    });
    if engine != Engine::Latex {
        return None;
    }
    let masked = mask_comments(main_text);
    let class = CLASS.find(&masked)?;
    let begin = masked.find("\\begin{document}").unwrap_or(masked.len());
    if begin < class.end() {
        return None;
    }
    let preamble = &masked[..begin];
    let mut loaded = Vec::new();
    let mut last_end = class.end();
    for c in PACKAGE.captures_iter(preamble) {
        loaded.extend(c[1].split(',').map(|p| p.trim().to_string()));
        last_end = c.get(0).map(|m| m.end()).unwrap_or(last_end);
    }
    let mut add = Vec::new();
    if !loaded.iter().any(|p| p == "graphicx") {
        add.push("\\usepackage{graphicx}");
    }
    if masked.contains("\\includesvg") && !loaded.iter().any(|p| p == "svg") {
        add.push("\\usepackage{svg}");
    }
    if add.is_empty() {
        return None;
    }
    let at = main_text[last_end..]
        .find('\n')
        .map(|i| last_end + i)
        .unwrap_or(main_text.len());
    let mut out = String::with_capacity(main_text.len() + 40);
    out.push_str(&main_text[..at]);
    for a in add {
        out.push('\n');
        out.push_str(a);
    }
    out.push_str(&main_text[at..]);
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{Rgba, RgbaImage};

    fn png(w: u32, h: u32) -> Vec<u8> {
        let img = RgbaImage::from_pixel(w, h, Rgba([10, 20, 30, 255]));
        let mut out = Vec::new();
        DynamicImage::ImageRgba8(img)
            .write_to(&mut Cursor::new(&mut out), ImageFormat::Png)
            .unwrap();
        out
    }

    fn jpeg(w: u32, h: u32) -> Vec<u8> {
        let img = image::RgbImage::from_pixel(w, h, image::Rgb([200, 100, 50]));
        let mut out = Vec::new();
        JpegEncoder::new_with_quality(&mut out, 80)
            .encode_image(&img)
            .unwrap();
        out
    }

    #[test]
    fn imports_get_readable_unique_names() {
        let root = tempfile::tempdir().unwrap();
        let a = import_bytes(
            root.path(),
            &png(10, 8),
            Some("Screenshot 2026-11-09 at 10.22.33"),
            "png",
        )
        .unwrap();
        assert_eq!(a.rel_path, "figures/screenshot-2026-11-09-at-10-22-33.png");
        assert_eq!((a.width, a.height), (Some(10), Some(8)));
        assert!(!a.used);
        // The same image again is the same figure; a different one gets -2.
        let again = import_bytes(
            root.path(),
            &png(10, 8),
            Some("Screenshot 2026-11-09 at 10.22.33"),
            "png",
        )
        .unwrap();
        assert_eq!(again.path, a.path);
        let other = import_bytes(
            root.path(),
            &png(11, 8),
            Some("Screenshot 2026-11-09 at 10.22.33"),
            "png",
        )
        .unwrap();
        assert_eq!(
            other.rel_path,
            "figures/screenshot-2026-11-09-at-10-22-33-2.png"
        );
        let unnamed = import_bytes(root.path(), &png(3, 3), None, "png").unwrap();
        assert_eq!(unnamed.rel_path, "figures/figure.png");
        // The bytes decide over a wrong extension.
        let jpg = import_bytes(root.path(), &jpeg(4, 4), Some("photo"), "png").unwrap();
        assert_eq!(jpg.rel_path, "figures/photo.jpg");
    }

    #[test]
    fn large_images_are_scaled_down() {
        let root = tempfile::tempdir().unwrap();
        let big = import_bytes(root.path(), &png(4000, 1000), Some("wide"), "png").unwrap();
        assert_eq!((big.width, big.height), (Some(3200), Some(800)));
        let photo = import_bytes(root.path(), &jpeg(1500, 3600), Some("tall"), "jpg").unwrap();
        assert_eq!((photo.width, photo.height), (Some(1333), Some(3200)));
        let small = png(100, 50);
        let kept = import_bytes(root.path(), &small, Some("small"), "png").unwrap();
        assert_eq!(
            fs::read(&kept.path).unwrap(),
            small,
            "small images are copied untouched"
        );
    }

    /// A JPEG with an EXIF orientation tag, as phone cameras write them.
    fn jpeg_with_orientation(w: u32, h: u32, orientation: u16) -> Vec<u8> {
        let plain = jpeg(w, h);
        let mut tiff = b"II*\0\x08\0\0\0".to_vec();
        tiff.extend_from_slice(&1u16.to_le_bytes()); // one entry
        tiff.extend_from_slice(&0x0112u16.to_le_bytes()); // Orientation
        tiff.extend_from_slice(&3u16.to_le_bytes()); // SHORT
        tiff.extend_from_slice(&1u32.to_le_bytes());
        tiff.extend_from_slice(&orientation.to_le_bytes());
        tiff.extend_from_slice(&[0, 0]);
        tiff.extend_from_slice(&0u32.to_le_bytes()); // no next IFD
        let mut app1 = b"Exif\0\0".to_vec();
        app1.extend_from_slice(&tiff);
        let mut out = plain[..2].to_vec(); // SOI
        out.extend_from_slice(&[0xFF, 0xE1]);
        out.extend_from_slice(&((app1.len() + 2) as u16).to_be_bytes());
        out.extend_from_slice(&app1);
        out.extend_from_slice(&plain[2..]);
        out
    }

    #[test]
    fn rotated_photos_are_turned_upright() {
        let root = tempfile::tempdir().unwrap();
        // Orientation 6: stored landscape, shown rotated 90° clockwise.
        let f = import_bytes(
            root.path(),
            &jpeg_with_orientation(40, 20, 6),
            Some("phone"),
            "jpg",
        )
        .unwrap();
        assert_eq!((f.width, f.height), (Some(20), Some(40)));
        let upright = jpeg_with_orientation(40, 20, 1);
        let g = import_bytes(root.path(), &upright, Some("upright"), "jpg").unwrap();
        assert_eq!(
            fs::read(&g.path).unwrap(),
            upright,
            "upright photos are copied untouched"
        );
    }

    #[test]
    fn vector_files_are_copied_as_they_are() {
        let root = tempfile::tempdir().unwrap();
        let svg = br#"<?xml version="1.0"?><svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 120 80"><rect/></svg>"#;
        let f = import_bytes(root.path(), svg, Some("Diagram"), "svg").unwrap();
        assert_eq!(f.rel_path, "figures/diagram.svg");
        assert_eq!((f.width, f.height), (Some(120), Some(80)));
        assert_eq!(fs::read(&f.path).unwrap(), svg.to_vec());
        let eps = b"%!PS-Adobe-3.0 EPSF-3.0\n%%BoundingBox: 0 0 200 100\nshowpage\n";
        let e = import_bytes(root.path(), eps, Some("plot"), "eps").unwrap();
        assert_eq!((e.width, e.height), (Some(200), Some(100)));
        let pdf = crate::pdftext::tests::tiny_pdf(&["x"], None);
        let p = import_bytes(root.path(), &pdf, Some("chart"), "pdf").unwrap();
        assert_eq!(p.rel_path, "figures/chart.pdf");
        assert_eq!((p.width, p.height), (Some(612), Some(792)));
        assert!(matches!(
            import_bytes(root.path(), b"hello", Some("x"), "txt"),
            Err(Error::Invalid(_))
        ));
    }

    #[test]
    fn import_file_uses_the_file_name() {
        let root = tempfile::tempdir().unwrap();
        let src = root.path().join("My Plot (final).PNG");
        fs::write(&src, png(5, 5)).unwrap();
        let f = import_file(root.path(), &src).unwrap();
        assert_eq!(f.rel_path, "figures/my-plot-final.png");
    }

    #[test]
    fn converts_with_sips() {
        if !Path::new("/usr/bin/sips").exists() {
            return;
        }
        let root = tempfile::tempdir().unwrap();
        // A TIFF made by sips itself from a PNG.
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("in.png");
        fs::write(&src, png(40, 30)).unwrap();
        let tiff = dir.path().join("scan.tiff");
        let ok = Command::new("/usr/bin/sips")
            .args(["-s", "format", "tiff"])
            .arg(&src)
            .arg("--out")
            .arg(&tiff)
            .output()
            .unwrap();
        assert!(ok.status.success());
        let f = import_file(root.path(), &tiff).unwrap();
        assert_eq!(f.rel_path, "figures/scan.png");
        assert_eq!((f.width, f.height), (Some(40), Some(30)));
        let heic = dir.path().join("photo.heic");
        let made = Command::new("/usr/bin/sips")
            .args(["-s", "format", "heic"])
            .arg(&src)
            .arg("--out")
            .arg(&heic)
            .output()
            .unwrap();
        if made.status.success() && heic.is_file() {
            let f = import_file(root.path(), &heic).unwrap();
            assert_eq!(f.rel_path, "figures/photo.jpg");
            assert_eq!((f.width, f.height), (Some(40), Some(30)));
        }
        assert!(
            !root.path().join(".oyl/tmp").exists(),
            "work files are cleaned up"
        );
    }

    #[test]
    fn listing_marks_what_is_used() {
        let root = tempfile::tempdir().unwrap();
        let fig = root.path().join(FIGURES_DIR);
        fs::create_dir_all(fig.join("sub")).unwrap();
        for name in [
            "arch.png",
            "loss.pdf",
            "unused.jpg",
            "sub/detail.png",
            ".hidden.png",
            "notes.txt",
        ] {
            fs::write(fig.join(name), png(2, 2)).unwrap();
        }
        let tex = "\\graphicspath{{figures/}}\n\\includegraphics[width=\\linewidth]{arch}\n\\includegraphics{../figures/sub/detail.png}";
        let typ = "#figure(image(\"figures/loss.pdf\"))";
        let figs = list(root.path(), &[tex, typ]).unwrap();
        let summary: Vec<(&str, bool)> =
            figs.iter().map(|f| (f.rel_path.as_str(), f.used)).collect();
        assert_eq!(
            summary,
            vec![
                ("figures/arch.png", true),
                ("figures/loss.pdf", true),
                ("figures/sub/detail.png", true),
                ("figures/unused.jpg", false),
            ]
        );
        assert!(
            list(tempfile::tempdir().unwrap().path(), &[""])
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn figure_snippets() {
        assert_eq!(
            snippet(
                Engine::Latex,
                "figures/model.png",
                "The model: 50% smaller & faster",
                None
            ),
            "\\begin{figure}[htbp]\n  \\centering\n  \\includegraphics[width=\\linewidth]{figures/model.png}\n  \\caption{The model: 50\\% smaller \\& faster}\n  \\label{fig:model}\n\\end{figure}"
        );
        assert_eq!(
            snippet(
                Engine::Typst,
                "figures/x.png",
                "A [tiny] #1 plot",
                Some("fig:x")
            ),
            "#figure(image(\"figures/x.png\", width: 80%), caption: [A \\[tiny\\] \\#1 plot]) <fig:x>"
        );
        assert_eq!(
            snippet(Engine::Typst, "figures/My Plot.png", "", None),
            "#figure(image(\"figures/My Plot.png\", width: 80%)) <fig:my-plot>"
        );
        assert!(
            snippet(Engine::Latex, "figures/d.svg", "D", Some("fig d"))
                .contains("\\includesvg[width=\\linewidth]{figures/d}")
        );
        assert!(
            snippet(Engine::Latex, "figures/d.svg", "D", Some("fig d")).contains("\\label{fig-d}")
        );
    }

    #[test]
    fn preamble_gets_graphicx() {
        let doc = "\\documentclass[11pt]{article}\n\\usepackage{amsmath}\n% \\usepackage{graphicx}\n\\usepackage[utf8]{inputenc}\n\\begin{document}\nHi\n\\end{document}\n";
        let out = ensure_preamble(Engine::Latex, doc).unwrap();
        assert_eq!(
            out,
            "\\documentclass[11pt]{article}\n\\usepackage{amsmath}\n% \\usepackage{graphicx}\n\\usepackage[utf8]{inputenc}\n\\usepackage{graphicx}\n\\begin{document}\nHi\n\\end{document}\n"
        );
        assert_eq!(ensure_preamble(Engine::Latex, &out), None);
        let bare = "\\documentclass{article}\n\\begin{document}\n\\end{document}";
        assert_eq!(
            ensure_preamble(Engine::Latex, bare).unwrap(),
            "\\documentclass{article}\n\\usepackage{graphicx}\n\\begin{document}\n\\end{document}"
        );
        let listed = "\\documentclass{article}\n\\usepackage{amsmath,graphicx}\n\\begin{document}\\end{document}";
        assert_eq!(ensure_preamble(Engine::Latex, listed), None);
        let svg = "\\documentclass{article}\n\\usepackage{graphicx}\n\\begin{document}\n\\includesvg{figures/d}\n\\end{document}";
        assert!(
            ensure_preamble(Engine::Latex, svg)
                .unwrap()
                .contains("\\usepackage{graphicx}\n\\usepackage{svg}\n")
        );
        assert_eq!(
            ensure_preamble(Engine::Latex, "\\section{Only a chapter}"),
            None
        );
        assert_eq!(ensure_preamble(Engine::Typst, "= Title"), None);
    }
}
