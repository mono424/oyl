//! Helpers shared by the integration tests.

#![allow(dead_code)]

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use oyl_model::Engine;
use oyl_typeset::{CompileOutput, CompileRequest, PageImage};

/// A temporary paper folder.
pub struct Paper {
    pub dir: tempfile::TempDir,
}

impl Paper {
    pub fn new() -> Self {
        Self {
            dir: tempfile::tempdir().expect("temp dir"),
        }
    }

    pub fn root(&self) -> PathBuf {
        self.dir.path().to_path_buf()
    }

    pub fn path(&self, name: &str) -> PathBuf {
        self.dir.path().join(name)
    }

    pub fn write(&self, name: &str, text: &str) -> PathBuf {
        let path = self.path(name);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("create dirs");
        }
        fs::write(&path, text).expect("write file");
        path
    }

    pub fn request(&self, engine: Engine, main: &str) -> CompileRequest {
        CompileRequest {
            root: self.root(),
            main: self.path(main),
            engine,
            overlays: HashMap::new(),
        }
    }

    /// Names in the folder, sorted.
    pub fn listing(&self) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(self.dir.path())
            .expect("read dir")
            .map(|entry| {
                entry
                    .expect("entry")
                    .file_name()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        names.sort();
        names
    }
}

/// Panics with the diagnostics when the compile reported an error.
pub fn assert_no_errors(output: &CompileOutput) {
    assert!(
        !output.has_errors(),
        "unexpected errors: {:#?}",
        output.diagnostics
    );
}

/// Whether the image has dark (ink) pixels.
pub fn has_ink(image: &PageImage) -> bool {
    image
        .rgba
        .as_chunks::<4>()
        .0
        .iter()
        .any(|pixel| pixel[0] < 80 && pixel[1] < 80 && pixel[2] < 80)
}

/// Whether every pixel is opaque and the corners are white paper.
pub fn is_opaque_white_paper(image: &PageImage) -> bool {
    assert_eq!(image.rgba.len(), (image.width * image.height * 4) as usize);
    let opaque = image
        .rgba
        .as_chunks::<4>()
        .0
        .iter()
        .all(|pixel| pixel[3] == 255);
    let pixel = |x: u32, y: u32| {
        let offset = ((y * image.width + x) * 4) as usize;
        &image.rgba[offset..offset + 4]
    };
    let white = [255, 255, 255, 255];
    opaque
        && pixel(0, 0) == white
        && pixel(image.width - 1, 0) == white
        && pixel(0, image.height - 1) == white
        && pixel(image.width - 1, image.height - 1) == white
}

/// The expected pixel size of a page.
pub fn expected_size(size_pt: (f32, f32), scale: f32) -> (u32, u32) {
    (
        (f64::from(size_pt.0) * f64::from(scale)).round() as u32,
        (f64::from(size_pt.1) * f64::from(scale)).round() as u32,
    )
}

/// Whether two paths name the same file (temp dirs on macOS live behind
/// the `/var` → `/private/var` symlink).
pub fn same_file(a: &Path, b: &Path) -> bool {
    a == b
        || a.canonicalize()
            .ok()
            .is_some_and(|a| Some(a) == b.canonicalize().ok())
}

/// A data folder for a [`oyl_typeset::Compiler`].
pub fn data_dir() -> tempfile::TempDir {
    tempfile::tempdir().expect("temp dir")
}
