//! The built-in templates a new paper starts from: article, conference
//! (two columns), thesis (several files) and notes, for each engine.
//!
//! Every template compiles offline as shipped: Typst templates use no
//! packages and only the fonts embedded in Typst; LaTeX templates use only
//! packages of a standard TeX distribution, with natbib and BibTeX. Each
//! cites one real entry of its `refs.bib`, so a new paper shows a
//! bibliography from the first compile.

use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
};

use oyl_model::Engine;

use crate::{
    error::{IoContext, Result},
    project::CiteStyle,
};

/// The id of the template a new paper uses unless another is picked.
pub const DEFAULT_TEMPLATE_ID: &str = "article";

const REFS_BIB: &str = include_str!("../templates/refs.bib");

/// A built-in starting point for a paper.
#[derive(Debug)]
pub struct Template {
    /// Stable id: `article`, `conference`, `thesis`, `notes`.
    pub id: &'static str,
    /// Human name, as shown when creating a paper.
    pub name: &'static str,
    /// One line on what it is for.
    pub description: &'static str,
    /// The engine the template is written for.
    pub engine: Engine,
    /// The entry file, relative to the paper root.
    pub main: &'static str,
    /// The citation style the template is set up for.
    pub cite_style: CiteStyle,
    /// Files relative to the paper root, with `{{title}}`, `{{author}}` and
    /// `{{date}}` placeholders.
    files: &'static [(&'static str, &'static str)],
}

/// The values filled into a template.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TemplateVars {
    /// The paper's title.
    pub title: String,
    /// The author line.
    pub author: String,
    /// Already formatted for print, e.g. "November 9, 2026".
    pub date: String,
}

impl TemplateVars {
    /// Today's date, and "Your Name" when no author is known yet.
    pub fn new(title: &str, author: &str) -> Self {
        let author = author.trim();
        Self {
            title: title.trim().to_owned(),
            author: if author.is_empty() {
                "Your Name".to_owned()
            } else {
                author.to_owned()
            },
            date: chrono::Local::now().format("%B %-d, %Y").to_string(),
        }
    }
}

impl Template {
    /// The template's files as shipped (with placeholders), relative paths
    /// first.
    pub fn raw_files(&self) -> impl Iterator<Item = (&'static str, &'static str)> + '_ {
        self.files.iter().copied()
    }

    /// The template's files with the placeholders filled in, escaped for the
    /// engine (LaTeX special characters, Typst string literals).
    pub fn instantiate(&self, vars: &TemplateVars) -> Vec<(PathBuf, String)> {
        let escape = |text: &str| match self.engine {
            Engine::Latex => escape_latex(text),
            Engine::Typst => escape_typst_string(text),
        };
        let title = escape(&vars.title);
        let author = escape(&vars.author);
        let date = escape(&vars.date);
        self.files
            .iter()
            .map(|(path, text)| {
                let filled = if path.ends_with(".bib") {
                    (*text).to_owned()
                } else {
                    text.replace("{{title}}", &title)
                        .replace("{{author}}", &author)
                        .replace("{{date}}", &date)
                };
                (PathBuf::from(path), filled)
            })
            .collect()
    }

    /// Writes the filled-in files into `dir`, creating sub-folders. Fails
    /// rather than overwrite a file that already exists.
    pub fn write_to(&self, dir: &Path, vars: &TemplateVars) -> Result<()> {
        for (rel, text) in self.instantiate(vars) {
            let path = dir.join(rel);
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent).ctx("create folder", parent)?;
            }
            let mut file = fs::File::create_new(&path).ctx("create", &path)?;
            file.write_all(text.as_bytes()).ctx("write", &path)?;
        }
        Ok(())
    }
}

/// The templates for an engine, the default first.
pub fn templates(engine: Engine) -> &'static [Template] {
    match engine {
        Engine::Typst => TYPST,
        Engine::Latex => LATEX,
    }
}

/// The template a new paper uses unless another is picked: the article.
pub fn default_template(engine: Engine) -> &'static Template {
    &templates(engine)[0]
}

/// The template with that id for that engine.
pub fn template(engine: Engine, id: &str) -> Option<&'static Template> {
    templates(engine).iter().find(|template| template.id == id)
}

/// Escapes text for use in LaTeX running text.
fn escape_latex(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '\\' => out.push_str("\\textbackslash{}"),
            '~' => out.push_str("\\textasciitilde{}"),
            '^' => out.push_str("\\textasciicircum{}"),
            '&' | '%' | '$' | '#' | '_' | '{' | '}' => {
                out.push('\\');
                out.push(c);
            }
            '\n' | '\r' | '\t' => out.push(' '),
            _ => out.push(c),
        }
    }
    out
}

/// Escapes text for use inside a Typst string literal.
fn escape_typst_string(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' | '\r' | '\t' => out.push(' '),
            _ => out.push(c),
        }
    }
    out
}

static TYPST: &[Template] = &[
    Template {
        id: "article",
        name: "Article",
        description: "A single-column paper with an abstract, sections and references.",
        engine: Engine::Typst,
        main: "main.typ",
        cite_style: CiteStyle::AuthorYear,
        files: &[
            (
                "main.typ",
                include_str!("../templates/typst/article/main.typ"),
            ),
            ("refs.bib", REFS_BIB),
        ],
    },
    Template {
        id: "conference",
        name: "Conference paper",
        description: "Two columns, abstract and keywords, numbered citations.",
        engine: Engine::Typst,
        main: "main.typ",
        cite_style: CiteStyle::Numeric,
        files: &[
            (
                "main.typ",
                include_str!("../templates/typst/conference/main.typ"),
            ),
            ("refs.bib", REFS_BIB),
        ],
    },
    Template {
        id: "thesis",
        name: "Thesis",
        description: "Title page, contents and one file per chapter in chapters/.",
        engine: Engine::Typst,
        main: "main.typ",
        cite_style: CiteStyle::AuthorYear,
        files: &[
            (
                "main.typ",
                include_str!("../templates/typst/thesis/main.typ"),
            ),
            (
                "chapters/introduction.typ",
                include_str!("../templates/typst/thesis/chapters/introduction.typ"),
            ),
            (
                "chapters/background.typ",
                include_str!("../templates/typst/thesis/chapters/background.typ"),
            ),
            (
                "chapters/conclusion.typ",
                include_str!("../templates/typst/thesis/chapters/conclusion.typ"),
            ),
            ("refs.bib", REFS_BIB),
        ],
    },
    Template {
        id: "notes",
        name: "Notes",
        description: "Loose notes: lists, sources and next steps.",
        engine: Engine::Typst,
        main: "main.typ",
        cite_style: CiteStyle::AuthorYear,
        files: &[
            (
                "main.typ",
                include_str!("../templates/typst/notes/main.typ"),
            ),
            ("refs.bib", REFS_BIB),
        ],
    },
];

static LATEX: &[Template] = &[
    Template {
        id: "article",
        name: "Article",
        description: "A single-column paper with an abstract, sections and references.",
        engine: Engine::Latex,
        main: "main.tex",
        cite_style: CiteStyle::AuthorYear,
        files: &[
            (
                "main.tex",
                include_str!("../templates/latex/article/main.tex"),
            ),
            ("refs.bib", REFS_BIB),
        ],
    },
    Template {
        id: "conference",
        name: "Conference paper",
        description: "Two columns, abstract and keywords, numbered citations.",
        engine: Engine::Latex,
        main: "main.tex",
        cite_style: CiteStyle::Numeric,
        files: &[
            (
                "main.tex",
                include_str!("../templates/latex/conference/main.tex"),
            ),
            ("refs.bib", REFS_BIB),
        ],
    },
    Template {
        id: "thesis",
        name: "Thesis",
        description: "Title page, contents and one file per chapter in chapters/.",
        engine: Engine::Latex,
        main: "main.tex",
        cite_style: CiteStyle::AuthorYear,
        files: &[
            (
                "main.tex",
                include_str!("../templates/latex/thesis/main.tex"),
            ),
            (
                "chapters/introduction.tex",
                include_str!("../templates/latex/thesis/chapters/introduction.tex"),
            ),
            (
                "chapters/background.tex",
                include_str!("../templates/latex/thesis/chapters/background.tex"),
            ),
            (
                "chapters/conclusion.tex",
                include_str!("../templates/latex/thesis/chapters/conclusion.tex"),
            ),
            ("refs.bib", REFS_BIB),
        ],
    },
    Template {
        id: "notes",
        name: "Notes",
        description: "Loose notes: lists, sources and next steps.",
        engine: Engine::Latex,
        main: "main.tex",
        cite_style: CiteStyle::AuthorYear,
        files: &[
            (
                "main.tex",
                include_str!("../templates/latex/notes/main.tex"),
            ),
            ("refs.bib", REFS_BIB),
        ],
    },
];

#[cfg(test)]
mod tests {
    use std::process::Command;

    use super::*;

    const IDS: [&str; 4] = ["article", "conference", "thesis", "notes"];

    fn vars() -> TemplateVars {
        TemplateVars {
            title: "A Study of Careful Writing".into(),
            author: "Ada Lovelace".into(),
            date: "November 9, 2026".into(),
        }
    }

    /// A Typst world over a folder with only the fonts embedded in Typst and
    /// no packages: what a fresh, offline Oyl has.
    struct OfflineWorld {
        root: PathBuf,
        main: typst::syntax::FileId,
        library: typst::utils::LazyHash<typst::Library>,
        fonts: typst_kit::fonts::FontStore,
    }

    impl OfflineWorld {
        fn new(root: &Path) -> Self {
            use typst::{
                LibraryExt,
                syntax::{RootedPath, VirtualPath, VirtualRoot},
            };
            let mut fonts = typst_kit::fonts::FontStore::new();
            fonts.extend(typst_kit::fonts::embedded());
            let main = RootedPath::new(VirtualRoot::Project, VirtualPath::new("main.typ").unwrap())
                .intern();
            Self {
                root: root.to_path_buf(),
                main,
                library: typst::utils::LazyHash::new(typst::Library::default()),
                fonts,
            }
        }

        fn path(&self, id: typst::syntax::FileId) -> typst::diag::FileResult<PathBuf> {
            use typst::{diag::FileError, syntax::VirtualRoot};
            let rooted = id.get();
            match rooted.root() {
                VirtualRoot::Project => rooted
                    .vpath()
                    .realize(&self.root)
                    .map_err(|_| FileError::AccessDenied),
                VirtualRoot::Package(_) => Err(FileError::AccessDenied),
            }
        }
    }

    impl typst::World for OfflineWorld {
        fn library(&self) -> &typst::utils::LazyHash<typst::Library> {
            &self.library
        }

        fn book(&self) -> &typst::utils::LazyHash<typst::text::FontBook> {
            self.fonts.book()
        }

        fn main(&self) -> typst::syntax::FileId {
            self.main
        }

        fn source(
            &self,
            id: typst::syntax::FileId,
        ) -> typst::diag::FileResult<typst::syntax::Source> {
            let path = self.path(id)?;
            let text = fs::read_to_string(&path)
                .map_err(|err| typst::diag::FileError::from_io(err, &path))?;
            Ok(typst::syntax::Source::new(id, text))
        }

        fn file(
            &self,
            id: typst::syntax::FileId,
        ) -> typst::diag::FileResult<typst::foundations::Bytes> {
            let path = self.path(id)?;
            fs::read(&path)
                .map(typst::foundations::Bytes::new)
                .map_err(|err| typst::diag::FileError::from_io(err, &path))
        }

        fn font(&self, index: usize) -> Option<typst::text::Font> {
            self.fonts.font(index)
        }

        fn today(
            &self,
            _offset: Option<typst::foundations::Duration>,
        ) -> Option<typst::foundations::Datetime> {
            typst::foundations::Datetime::from_ymd(2026, 11, 9)
        }
    }

    /// Compiles every Typst template with Typst 0.15 itself, offline (no
    /// packages, embedded fonts only), and requires no errors, no warnings,
    /// and a resolved citation.
    #[test]
    fn typst_templates_compile_offline_without_warnings() {
        for template in templates(Engine::Typst) {
            let dir = tempfile::tempdir().unwrap();
            template.write_to(dir.path(), &vars()).unwrap();
            let world = OfflineWorld::new(dir.path());
            let warned = typst::compile::<typst_layout::PagedDocument>(&world);
            let messages = |diags: &[typst::diag::SourceDiagnostic]| {
                diags
                    .iter()
                    .map(|diag| diag.message.to_string())
                    .collect::<Vec<_>>()
            };
            let document = match warned.output {
                Ok(document) => document,
                Err(errors) => panic!("{} failed: {:?}", template.id, messages(&errors)),
            };
            assert!(
                warned.warnings.is_empty(),
                "{} warns: {:?}",
                template.id,
                messages(&warned.warnings)
            );
            assert!(!document.pages().is_empty(), "{}: no pages", template.id);
        }
    }

    #[test]
    fn every_engine_has_the_four_templates_article_first() {
        for engine in Engine::ALL {
            let ids: Vec<_> = templates(engine).iter().map(|t| t.id).collect();
            assert_eq!(ids, IDS);
            assert_eq!(default_template(engine).id, DEFAULT_TEMPLATE_ID);
            for template in templates(engine) {
                assert_eq!(template.engine, engine);
                assert_eq!(template.main, engine.default_main());
                assert!(template.raw_files().any(|(path, _)| path == template.main));
                assert!(template.raw_files().any(|(path, _)| path == "refs.bib"));
            }
        }
        assert!(template(Engine::Typst, "poster").is_none());
    }

    #[test]
    fn templates_cite_the_entry_they_ship() {
        for engine in Engine::ALL {
            for template in templates(engine) {
                let sources: String = template
                    .raw_files()
                    .filter(|(path, _)| !path.ends_with(".bib"))
                    .map(|(_, text)| text)
                    .collect();
                let cites = match engine {
                    Engine::Typst => sources.contains("@knuth1984"),
                    Engine::Latex => sources.contains("\\citep{knuth1984}"),
                };
                assert!(cites, "{} {} cites nothing", engine, template.id);
                assert!(REFS_BIB.contains("@article{knuth1984,"));
                if engine == Engine::Typst {
                    assert!(
                        !sources.contains("@preview"),
                        "{} uses a package",
                        template.id
                    );
                    assert!(sources.contains("#bibliography(\"refs.bib\""));
                } else {
                    assert!(sources.contains("\\bibliography{refs}"));
                }
            }
        }
    }

    #[test]
    fn placeholders_are_filled_and_escaped() {
        let vars = TemplateVars {
            title: "Cost \"&\" 100% of C:\\ #1".into(),
            author: "A_B".into(),
            date: "today".into(),
        };
        let typst = template(Engine::Typst, "article")
            .unwrap()
            .instantiate(&vars);
        let main = &typst[0].1;
        assert!(main.contains(r#"#let title = "Cost \"&\" 100% of C:\\ #1""#));
        assert!(
            ["{{title}}", "{{author}}", "{{date}}"]
                .iter()
                .all(|p| !main.contains(p))
        );

        let latex = template(Engine::Latex, "article")
            .unwrap()
            .instantiate(&vars);
        let main = &latex[0].1;
        assert!(main.contains(r#"\title{Cost "\&" 100\% of C:\textbackslash{} \#1}"#));
        assert!(main.contains(r"\author{A\_B}"));
        assert!(
            ["{{title}}", "{{author}}", "{{date}}"]
                .iter()
                .all(|p| !main.contains(p))
        );
    }

    #[test]
    fn write_to_refuses_to_overwrite() {
        let dir = tempfile::tempdir().unwrap();
        let template = template(Engine::Latex, "thesis").unwrap();
        template.write_to(dir.path(), &vars()).unwrap();
        assert!(dir.path().join("chapters/background.tex").is_file());
        assert!(template.write_to(dir.path(), &vars()).is_err());
    }

    /// Compiles every LaTeX template with latexmk (pdflatex + BibTeX), in a
    /// temporary copy, when latexmk is installed; checks that the citation
    /// resolved and the bibliography was built.
    #[test]
    fn latex_templates_compile_with_latexmk() {
        if Command::new("latexmk").arg("-v").output().is_err() {
            eprintln!("latexmk is not installed; skipping the LaTeX compile check");
            return;
        }
        for template in templates(Engine::Latex) {
            let dir = tempfile::tempdir().unwrap();
            template.write_to(dir.path(), &vars()).unwrap();
            let output = Command::new("latexmk")
                .args([
                    "-pdf",
                    "-interaction=nonstopmode",
                    "-halt-on-error",
                    "main.tex",
                ])
                .current_dir(dir.path())
                .output()
                .unwrap();
            let log = fs::read_to_string(dir.path().join("main.log")).unwrap_or_default();
            assert!(
                output.status.success(),
                "{} failed to compile:\n{}\n{}",
                template.id,
                String::from_utf8_lossy(&output.stdout),
                log
            );
            assert!(
                dir.path().join("main.pdf").is_file(),
                "{}: no PDF",
                template.id
            );
            let bbl = fs::read_to_string(dir.path().join("main.bbl")).unwrap();
            assert!(
                bbl.contains("knuth1984"),
                "{}: empty bibliography",
                template.id
            );
            assert!(
                !log.contains("undefined"),
                "{}: undefined references",
                template.id
            );
        }
    }
}
