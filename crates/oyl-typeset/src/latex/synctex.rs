//! SyncTeX: from a point on a PDF page to the TeX source line that produced
//! it, and back.
//!
//! The `.synctex(.gz)` file lists, per page, the boxes TeX shipped out
//! (`[`/`(` … `]`/`)`, void boxes `v`/`h`) and positioned nodes inside them
//! (`x` characters, `k` kerns, `g` glue, `$` math), each tagged with an input
//! file and a line. Coordinates are TeX scaled points measured from the top
//! left corner of the page; we convert them to PDF points.

use std::io::Read;
use std::path::{Path, PathBuf};

use oyl_model::SourceLocation;
use oyl_model::typeset::PagePosition;

use crate::paths::{absolutize, canonical, normalize, relative_inside};

/// Scaled points per PDF point (big point): 65536 · 72.27 / 72.
const SP_PER_BP: f64 = 65_781.76;

/// A parsed SyncTeX file.
#[derive(Debug, Default)]
pub(crate) struct SyncTex {
    inputs: Vec<Input>,
    pages: Vec<SyncPage>,
}

#[derive(Debug)]
struct Input {
    tag: u32,
    /// Absolute path, in the form the project root was given in.
    path: PathBuf,
    canonical: PathBuf,
}

#[derive(Debug, Default)]
struct SyncPage {
    boxes: Vec<SyncBox>,
    nodes: Vec<SyncNode>,
}

#[derive(Debug)]
struct SyncBox {
    tag: u32,
    line: u32,
    left: f32,
    right: f32,
    top: f32,
    bottom: f32,
    /// Nesting level on the page.
    depth: u16,
    horizontal: bool,
}

impl SyncBox {
    fn contains(&self, x: f32, y: f32) -> bool {
        (self.left..=self.right).contains(&x) && (self.top..=self.bottom).contains(&y)
    }

    fn area(&self) -> f32 {
        (self.right - self.left) * (self.bottom - self.top)
    }

    fn distance(&self, x: f32, y: f32) -> f32 {
        let dx = (self.left - x).max(x - self.right).max(0.0);
        let dy = (self.top - y).max(y - self.bottom).max(0.0);
        dx.hypot(dy)
    }
}

#[derive(Debug)]
struct SyncNode {
    tag: u32,
    line: u32,
    x: f32,
    y: f32,
    /// Index of the enclosing box in [`SyncPage::boxes`].
    parent: Option<usize>,
}

/// A record's `tag,line[,column]` and coordinates, still in TeX units.
struct Fields {
    tag: u32,
    line: u32,
    x: f64,
    y: f64,
    size: Option<(f64, f64, f64)>,
}

fn parse_fields(record: &str) -> Option<Fields> {
    let mut parts = record.split(':');
    let mut link = parts.next()?.split(',');
    let tag = link.next()?.trim().parse().ok()?;
    let line = link.next()?.trim().parse::<i64>().ok()?.max(0);
    let mut point = parts.next()?.split(',');
    let x = point.next()?.trim().parse::<f64>().ok()?;
    let y = point.next()?.trim().parse::<f64>().ok()?;
    let size = parts.next().map(|size| {
        let mut values = size
            .split(',')
            .map(|value| value.trim().parse::<f64>().unwrap_or(0.0));
        let width = values.next().unwrap_or(0.0);
        let height = values.next().unwrap_or(0.0);
        let depth = values.next().unwrap_or(0.0);
        (width, height, depth)
    });
    Some(Fields {
        tag,
        line: u32::try_from(line).unwrap_or(u32::MAX),
        x,
        y,
        size,
    })
}

impl SyncTex {
    /// Parses a gzip-compressed SyncTeX file.
    ///
    /// `run_dir` is the directory TeX ran in (relative input paths resolve
    /// against it); `root` is the project root as the app names it, so that
    /// returned paths match the editor's even when the folder is reached
    /// through a symlink.
    pub(crate) fn from_gz(bytes: &[u8], run_dir: &Path, root: &Path) -> Option<Self> {
        let mut text = String::new();
        flate2::read::GzDecoder::new(bytes)
            .read_to_string(&mut text)
            .ok()?;
        Self::parse(&text, run_dir, root)
    }

    /// Parses an uncompressed SyncTeX file. See [`SyncTex::from_gz`].
    pub(crate) fn parse(text: &str, run_dir: &Path, root: &Path) -> Option<Self> {
        let canonical_root = canonical(root);
        let mut sync = SyncTex::default();
        let mut unit = 1.0_f64;
        let mut magnification = 1000.0_f64;
        let mut x_offset = 0.0_f64;
        let mut y_offset = 0.0_f64;
        let mut in_content = false;
        let mut current: Option<usize> = None;
        let mut stack: Vec<usize> = Vec::new();
        let mut form_depth = 0_u32;
        let mut seen_version = false;

        for line in text.lines() {
            if let Some(rest) = line.strip_prefix("Input:") {
                // tectonic lists bundle files with an empty path.
                if let Some((tag, path)) = rest.split_once(':')
                    && !path.trim().is_empty()
                    && let Ok(tag) = tag.trim().parse()
                {
                    let absolute = absolutize(run_dir, Path::new(path));
                    let path = match relative_inside(&canonical_root, &absolute) {
                        Some(relative) => normalize(&root.join(relative)),
                        None => absolute,
                    };
                    sync.inputs.push(Input {
                        tag,
                        canonical: canonical(&path),
                        path,
                    });
                }
                continue;
            }
            if !in_content {
                if line.starts_with("SyncTeX Version:") {
                    seen_version = true;
                } else if let Some(value) = line.strip_prefix("Unit:") {
                    unit = value.trim().parse().unwrap_or(1.0);
                } else if let Some(value) = line.strip_prefix("Magnification:") {
                    magnification = value.trim().parse().unwrap_or(1000.0);
                } else if let Some(value) = line.strip_prefix("X Offset:") {
                    x_offset = value.trim().parse().unwrap_or(0.0);
                } else if let Some(value) = line.strip_prefix("Y Offset:") {
                    y_offset = value.trim().parse().unwrap_or(0.0);
                } else if line.starts_with("Content:") {
                    in_content = true;
                }
                continue;
            }
            if line.starts_with("Postamble:") {
                break;
            }

            let factor = unit * magnification / 1000.0 / SP_PER_BP;
            let to_x = |value: f64| ((value + x_offset) * factor) as f32;
            let to_y = |value: f64| ((value + y_offset) * factor) as f32;
            let Some(kind) = line.chars().next() else {
                continue;
            };
            let record = &line[kind.len_utf8()..];
            match kind {
                '{' => {
                    let number: usize = record.trim().parse().unwrap_or(sync.pages.len() + 1);
                    let index = number.max(1) - 1;
                    if sync.pages.len() <= index {
                        sync.pages.resize_with(index + 1, SyncPage::default);
                    }
                    current = Some(index);
                    stack.clear();
                }
                '}' => {
                    current = None;
                    stack.clear();
                }
                '<' => form_depth += 1,
                '>' => form_depth = form_depth.saturating_sub(1),
                '[' | '(' | 'v' | 'h' if form_depth == 0 => {
                    let (Some(page), Some(fields)) = (current, parse_fields(record)) else {
                        continue;
                    };
                    let (width, height, depth) = fields.size.unwrap_or((0.0, 0.0, 0.0));
                    let (x0, x1) = (to_x(fields.x), to_x(fields.x + width));
                    let page = &mut sync.pages[page];
                    page.boxes.push(SyncBox {
                        tag: fields.tag,
                        line: fields.line,
                        left: x0.min(x1),
                        right: x0.max(x1),
                        top: to_y(fields.y - height),
                        bottom: to_y(fields.y + depth),
                        depth: u16::try_from(stack.len()).unwrap_or(u16::MAX),
                        horizontal: matches!(kind, '(' | 'h'),
                    });
                    if matches!(kind, '[' | '(') {
                        stack.push(page.boxes.len() - 1);
                    }
                }
                ']' | ')' if form_depth == 0 => {
                    stack.pop();
                }
                'x' | 'k' | 'g' | '$' if form_depth == 0 => {
                    let (Some(page), Some(fields)) = (current, parse_fields(record)) else {
                        continue;
                    };
                    sync.pages[page].nodes.push(SyncNode {
                        tag: fields.tag,
                        line: fields.line,
                        x: to_x(fields.x),
                        y: to_y(fields.y),
                        parent: stack.last().copied(),
                    });
                }
                _ => {}
            }
        }

        (seen_version && in_content).then_some(sync)
    }

    fn path_of(&self, tag: u32) -> Option<&Path> {
        self.inputs
            .iter()
            .find(|input| input.tag == tag)
            .map(|input| input.path.as_path())
    }

    /// The source line under a point of a page (0-based page index, PDF
    /// points from the top left).
    pub(crate) fn source_at(&self, page: usize, x: f32, y: f32) -> Option<SourceLocation> {
        let page = self.pages.get(page)?;
        let horizontal = || page.boxes.iter().enumerate().filter(|(_, b)| b.horizontal);
        let hit = horizontal()
            .filter(|(_, b)| b.contains(x, y))
            .max_by(|(_, a), (_, b)| a.depth.cmp(&b.depth).then(b.area().total_cmp(&a.area())))
            .or_else(|| {
                horizontal().min_by(|(_, a), (_, b)| a.distance(x, y).total_cmp(&b.distance(x, y)))
            });

        let (tag, line) = match hit {
            Some((index, hbox)) => {
                // A paragraph line's box carries the line where the paragraph
                // ended (TeX breaks lines at the end of the paragraph); the
                // nodes inside carry the lines their material came from.
                let children: Vec<&SyncNode> = page
                    .nodes
                    .iter()
                    .filter(|node| node.parent == Some(index))
                    .collect();
                let nearest = |own_line: bool| {
                    children
                        .iter()
                        .filter(|node| {
                            own_line || !(node.tag == hbox.tag && node.line == hbox.line)
                        })
                        .min_by(|a, b| (a.x - x).abs().total_cmp(&(b.x - x).abs()))
                        .copied()
                };
                let node = nearest(false).or_else(|| nearest(true));
                match node {
                    Some(node) => (node.tag, node.line),
                    None => (hbox.tag, hbox.line),
                }
            }
            None => {
                let node = page.nodes.iter().min_by(|a, b| {
                    (a.x - x)
                        .hypot(a.y - y)
                        .total_cmp(&(b.x - x).hypot(b.y - y))
                })?;
                (node.tag, node.line)
            }
        };
        Some(SourceLocation::new(self.path_of(tag)?, line.max(1), 1))
    }

    /// Where a source line was typeset: the first record of that line in
    /// reading order, or of the nearest following (else preceding) line
    /// that produced output.
    pub(crate) fn position_of(&self, file: &Path, line: u32) -> Option<PagePosition> {
        let lexical = normalize(file);
        let wanted = canonical(file);
        let tags: Vec<u32> = self
            .inputs
            .iter()
            .filter(|input| input.path == lexical || input.canonical == wanted)
            .map(|input| input.tag)
            .collect();
        if tags.is_empty() {
            return None;
        }

        // Every record of this file in reading order: (page, line, x, top).
        let mut records: Vec<(usize, u32, f32, f32)> = Vec::new();
        for (index, page) in self.pages.iter().enumerate() {
            for node in page.nodes.iter().filter(|node| tags.contains(&node.tag)) {
                let top = node
                    .parent
                    .and_then(|parent| page.boxes.get(parent))
                    .map_or(node.y, |parent| parent.top);
                records.push((index, node.line, node.x, top));
            }
            for hbox in page
                .boxes
                .iter()
                .filter(|b| b.horizontal && tags.contains(&b.tag))
            {
                records.push((index, hbox.line, hbox.left, hbox.top));
            }
        }

        let lines = || records.iter().map(|record| record.1);
        let target = if lines().any(|l| l == line) {
            line
        } else {
            lines()
                .filter(|&l| l > line)
                .min()
                .or_else(|| lines().filter(|&l| l < line).max())?
        };
        let &(page, _, x, top) = records.iter().find(|record| record.1 == target)?;
        Some(PagePosition {
            page,
            x_pt: x,
            y_pt: top,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A trimmed SyncTeX file as pdfTeX writes it for a one-paragraph page.
    const SAMPLE: &str = "SyncTeX Version:1
Input:1:/paper/./main.tex
Input:2:/texmf/article.cls
Input:3:
Output:pdf
Magnification:1000
Unit:1
X Offset:0
Y Offset:0
Content:
!1237
{1
[1,10:4736286,46220574:26673152,41484288,0
(1,5:8799518,10300473:22609920,491520,163840
x1,5:10274081,10300473
g1,4:10492534,10300473
x1,4:10965850,10300473
g1,4:16181431,10300473
k1,5:31409438,10300473:12004000
g1,5:31409438,10300473
)
(1,7:8799518,11086905:22609920,455111,0
x1,6:9127199,11086905
x1,6:10913059,11086905
g1,7:54019358,11086905
)
]
}1
Postamble:
Count:12
";

    fn sample() -> SyncTex {
        SyncTex::parse(SAMPLE, Path::new("/paper"), Path::new("/paper")).unwrap()
    }

    #[test]
    fn converts_coordinates_to_points() {
        let sync = sample();
        let line_box = &sync.pages[0].boxes[1];
        assert!((line_box.left - 133.768).abs() < 0.01, "{}", line_box.left);
        // Baseline 156.585 bp, height 7.47 bp, depth 2.49 bp.
        assert!((line_box.top - 149.114).abs() < 0.01, "{}", line_box.top);
        assert!(
            (line_box.bottom - 159.076).abs() < 0.01,
            "{}",
            line_box.bottom
        );
        assert_eq!(sync.inputs[0].path, PathBuf::from("/paper/main.tex"));
        assert_eq!(sync.inputs.len(), 2, "empty inputs are skipped");
    }

    #[test]
    fn click_finds_the_line_of_the_material() {
        let sync = sample();
        // Inside the first line box: material from line 4, not the
        // paragraph-end line 5 the box itself carries.
        let at = sync.source_at(0, 160.0, 154.0).unwrap();
        assert_eq!(at, SourceLocation::new("/paper/main.tex", 4, 1));
        // Second line box: line 6.
        let at = sync.source_at(0, 150.0, 167.0).unwrap();
        assert_eq!(at.line, 6);
        // Outside any box: the nearest one.
        let at = sync.source_at(0, 10.0, 150.0).unwrap();
        assert_eq!(at.line, 4);
        assert!(sync.source_at(3, 10.0, 10.0).is_none());
    }

    #[test]
    fn line_maps_to_a_page_position() {
        let sync = sample();
        let position = sync.position_of(Path::new("/paper/main.tex"), 4).unwrap();
        assert_eq!(position.page, 0);
        assert!((position.x_pt - 159.505).abs() < 0.01, "{}", position.x_pt);
        assert!((position.y_pt - 149.114).abs() < 0.01, "{}", position.y_pt);
        // Line 2 produced nothing; the next line that did is used.
        let position = sync.position_of(Path::new("/paper/main.tex"), 2).unwrap();
        assert!((position.y_pt - 149.114).abs() < 0.01);
        // Beyond the end: the last line with output.
        let position = sync.position_of(Path::new("/paper/main.tex"), 99).unwrap();
        assert!(position.y_pt > 155.0);
        assert!(sync.position_of(Path::new("/paper/other.tex"), 1).is_none());
    }

    #[test]
    fn rejects_garbage() {
        assert!(SyncTex::parse("hello", Path::new("/"), Path::new("/")).is_none());
        assert!(SyncTex::from_gz(b"not gzip", Path::new("/"), Path::new("/")).is_none());
    }
}
