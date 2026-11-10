//! The figures as the panel shows them.

use std::path::Path;

use oyl_model::Engine;
use oyl_sources::Figure;
use oyl_sources::figures::{self, EXTENSIONS};

/// The longest side of a thumbnail, in pixels (two columns of a 420 px
/// card at 2× scale).
pub const THUMBNAIL_SIDE: u32 = 400;

/// Whether the panel draws a thumbnail of this file itself (PNG and JPEG;
/// the formats figures are imported as).
pub fn has_thumbnail(path: &Path) -> bool {
    matches!(
        extension(path).as_str(),
        "png" | "jpg" | "jpeg"
    )
}

/// The file's extension, lowercase.
pub fn extension(path: &Path) -> String {
    path.extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default()
}

/// Whether a dropped or picked file is an image the figures folder takes.
pub fn is_importable(path: &Path) -> bool {
    let ext = extension(path);
    EXTENSIONS.iter().any(|e| *e == ext)
}

/// "1200 × 800 px", "432 × 288 pt" (vector formats), or the file size alone
/// when the dimensions are unknown.
pub fn size_label(figure: &Figure) -> String {
    let unit = match extension(&figure.path).as_str() {
        "pdf" | "svg" | "eps" => "pt",
        _ => "px",
    };
    match (figure.width, figure.height) {
        (Some(w), Some(h)) => format!("{w} × {h} {unit}"),
        _ => bytes_label(figure.bytes),
    }
}

/// "240 KB", "3.1 MB".
pub fn bytes_label(bytes: u64) -> String {
    const KB: f64 = 1024.;
    let b = bytes as f64;
    if b < KB {
        format!("{bytes} B")
    } else if b < KB * KB {
        format!("{:.0} KB", b / KB)
    } else {
        format!("{:.1} MB", b / (KB * KB))
    }
}

/// The label a figure block for this image gets unless another is typed
/// (`fig:model` for `figures/model.png`), exactly as
/// [`figures::snippet`] makes it.
pub fn derived_label(engine: Engine, rel_path: &str) -> String {
    let block = figures::snippet(engine, rel_path, "", None);
    let found = match engine {
        Engine::Typst => block
            .rfind('<')
            .and_then(|start| block[start + 1..].find('>').map(|end| &block[start + 1..start + 1 + end])),
        Engine::Latex => block.find("\\label{").and_then(|start| {
            let rest = &block[start + 7..];
            rest.find('}').map(|end| &rest[..end])
        }),
    };
    found.unwrap_or("fig:figure").to_string()
}

/// The figure block to insert. A label equal to the derived one (or empty)
/// is left to the snippet to make.
pub fn figure_block(engine: Engine, rel_path: &str, caption: &str, label: &str) -> String {
    let label = label.trim();
    let label = (!label.is_empty() && label != derived_label(engine, rel_path)).then_some(label);
    figures::snippet(engine, rel_path, caption, label)
}

/// The name shown under a thumbnail: the path inside `figures/`.
pub fn display_name(figure: &Figure) -> String {
    figure
        .rel_path
        .strip_prefix("figures/")
        .unwrap_or(&figure.rel_path)
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn figure(rel: &str, w: Option<u32>, used: bool) -> Figure {
        Figure {
            path: PathBuf::from("/p").join(rel),
            rel_path: rel.into(),
            width: w,
            height: w.map(|w| w / 2),
            bytes: 250_000,
            used,
        }
    }

    #[test]
    fn sizes_read_in_their_unit() {
        assert_eq!(size_label(&figure("figures/a.png", Some(1200), true)), "1200 × 600 px");
        assert_eq!(size_label(&figure("figures/a.pdf", Some(432), true)), "432 × 216 pt");
        assert_eq!(size_label(&figure("figures/a.png", None, true)), "244 KB");
        assert_eq!(bytes_label(3_250_000), "3.1 MB");
        assert_eq!(bytes_label(12), "12 B");
    }

    #[test]
    fn labels_are_derived_like_the_snippet_does() {
        assert_eq!(derived_label(Engine::Typst, "figures/Model Overview.png"), "fig:model-overview");
        assert_eq!(derived_label(Engine::Latex, "figures/plot.pdf"), "fig:plot");
        let typst = figure_block(Engine::Typst, "figures/plot.png", "Results", "fig:plot");
        assert!(typst.ends_with("<fig:plot>"), "{typst}");
        assert!(typst.contains("caption: [Results]"), "{typst}");
        let custom = figure_block(Engine::Latex, "figures/plot.png", "Results", "fig:results");
        assert!(custom.contains("\\label{fig:results}"), "{custom}");
    }

    #[test]
    fn what_can_be_shown_and_imported() {
        assert!(has_thumbnail(Path::new("a/B.PNG")));
        assert!(has_thumbnail(Path::new("a/b.jpeg")));
        assert!(!has_thumbnail(Path::new("a/b.pdf")));
        assert!(is_importable(Path::new("x.heic")));
        assert!(!is_importable(Path::new("x.txt")));
        assert_eq!(display_name(&figure("figures/sub/a.png", None, false)), "sub/a.png");
    }
}
