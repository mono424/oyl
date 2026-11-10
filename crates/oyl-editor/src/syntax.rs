//! Syntax colors: Oyl's restrained theme, and the Typst highlighter.
//!
//! The theme uses few colors on purpose: amber for commands, functions and
//! citations; a softer ink for math, strings and brackets; a faint one for
//! comments; weight and slant for headings and emphasis. Everything else is
//! plain text. It is built from the active [`Tokens`], so it follows light and
//! dark.
//!
//! LaTeX and BibTeX are highlighted by the editor's built-in (syntect)
//! highlighter, which names what it finds with the same vocabulary
//! (`function`, `comment`, `title`, …). Typst is highlighted here with
//! `typst-syntax`, the parser the Typst compiler itself uses.

use std::collections::HashMap;
use std::ops::Range;
use std::rc::Rc;

use gpui_kit::base::input::{
    EditorState, FoldRange, HighlightStyleResolver, InputEdit, InputHighlighter,
    InputHighlighterFactory, Rope,
};
use gpui_kit::{Context, FontStyle, FontWeight, HighlightStyle, SharedString, Window};
use oyl_ui::Tokens;
use typst_syntax::{LinkedNode, Source, SyntaxKind, SyntaxNode, Tag};

/// Oyl's syntax theme, resolved for one appearance.
pub(crate) struct OylSyntaxTheme {
    styles: HashMap<&'static str, HighlightStyle>,
}

impl OylSyntaxTheme {
    pub(crate) fn new(tokens: &Tokens) -> Self {
        let color = |color| HighlightStyle {
            color: Some(color),
            ..Default::default()
        };
        let accent = color(tokens.accent);
        let soft = color(tokens.text_soft);
        let comment = HighlightStyle {
            color: Some(tokens.text_faint),
            font_style: Some(FontStyle::Italic),
            ..Default::default()
        };
        let bold = HighlightStyle {
            font_weight: Some(FontWeight::BOLD),
            ..Default::default()
        };
        let italic = HighlightStyle {
            font_style: Some(FontStyle::Italic),
            ..Default::default()
        };
        let heading = HighlightStyle {
            color: Some(tokens.text),
            font_weight: Some(FontWeight::BOLD),
            ..Default::default()
        };

        let styles = HashMap::from([
            ("comment", comment),
            // Commands, functions, keywords: the one accent.
            ("function", accent),
            ("keyword", accent),
            ("preproc", accent),
            ("variable.special", accent),
            ("link_uri", accent),
            // Citation keys, references and labels: the accent too.
            ("constant", accent),
            ("label", accent),
            // Structure.
            ("title", heading),
            ("emphasis", italic),
            ("emphasis.strong", bold),
            (
                "emphasis.both",
                HighlightStyle {
                    font_style: Some(FontStyle::Italic),
                    ..bold
                },
            ),
            // Calmer ink: math, literals, brackets.
            ("math", soft),
            ("string", soft),
            ("string.escape", soft),
            ("text.literal", soft),
            ("number", soft),
            ("boolean", soft),
            ("operator", soft),
            ("type", soft),
            ("punctuation", soft),
            ("punctuation.list_marker", accent),
        ]);
        Self { styles }
    }
}

impl HighlightStyleResolver for OylSyntaxTheme {
    /// The style for a highlight name, falling back to its shorter prefixes
    /// (`comment.doc` → `comment`).
    fn style(&self, name: &str) -> Option<HighlightStyle> {
        let mut name = name;
        loop {
            if let Some(style) = self.styles.get(name) {
                return Some(*style);
            }
            name = &name[..name.rfind('.')?];
        }
    }
}

/// Highlight names, ordered and non-overlapping.
type Highlights = Vec<(Range<usize>, &'static str)>;

/// Installs the Typst highlighter for an editor whose language is `typst`.
pub(crate) fn typst_highlighter_factory() -> InputHighlighterFactory {
    Rc::new(|language: &str| {
        (language == "typst")
            .then(|| Box::new(TypstHighlighter::default()) as Box<dyn InputHighlighter>)
    })
}

/// Highlights Typst with the compiler's own parser.
#[derive(Default)]
pub(crate) struct TypstHighlighter {
    source: Option<Source>,
    highlights: Highlights,
}

impl InputHighlighter for TypstHighlighter {
    fn language(&self) -> SharedString {
        "typst".into()
    }

    fn update(
        &mut self,
        _edit: Option<InputEdit>,
        text: &Rope,
        _folding: bool,
        _window: &mut Window,
        _cx: &mut Context<EditorState>,
    ) {
        let text = text.to_string();
        // `replace` reparses only the region that changed.
        let source = match self.source.as_mut() {
            Some(source) => {
                if source.text() != text {
                    source.replace(&text);
                }
                source
            }
            None => self.source.insert(Source::detached(text)),
        };
        self.highlights = typst_highlights(source.root());
    }

    fn update_batch(
        &mut self,
        edits: &[(InputEdit, Rope)],
        folding: bool,
        window: &mut Window,
        cx: &mut Context<EditorState>,
    ) {
        // Every edit carries the whole text after it; the last one is enough.
        if let Some((edit, text)) = edits.last() {
            self.update(Some(*edit), text, folding, window, cx);
        }
    }

    fn styles(
        &self,
        range: &Range<usize>,
        resolver: &dyn HighlightStyleResolver,
    ) -> Vec<(Range<usize>, HighlightStyle)> {
        resolve_styles(&self.highlights, range, resolver)
    }

    fn fold_ranges(&self, _text: &Rope) -> Vec<FoldRange> {
        Vec::new()
    }
}

/// The context a node is highlighted in.
#[derive(Clone, Copy, Default)]
struct Scope {
    math: bool,
    heading: bool,
    strong: bool,
    emph: bool,
    /// The name inherited from a highlighted ancestor (raw text, links,
    /// references…), when there is one.
    inherited: Option<&'static str>,
}

/// The highlight names for a parsed Typst document.
pub(crate) fn typst_highlights(root: &SyntaxNode) -> Highlights {
    let mut highlights = Highlights::new();
    walk(&LinkedNode::new(root), Scope::default(), &mut highlights);
    highlights
}

fn walk(node: &LinkedNode, mut scope: Scope, out: &mut Highlights) {
    match node.kind() {
        SyntaxKind::Equation => scope.math = true,
        SyntaxKind::Heading => scope.heading = true,
        SyntaxKind::Strong => scope.strong = true,
        SyntaxKind::Emph => scope.emph = true,
        _ => {}
    }

    let tag = typst_syntax::highlight(node);
    let own = tag.and_then(|tag| tag_name(tag, &scope));
    if let Some(name) = own {
        scope.inherited = Some(name);
    }

    if node.children().len() == 0 {
        let range = node.range();
        if range.is_empty() {
            return;
        }
        let name = if matches!(tag, Some(Tag::Comment)) {
            Some("comment")
        } else if scope.math {
            Some("math")
        } else {
            scope.inherited.or_else(|| markup_name(&scope))
        };
        if let Some(name) = name {
            push(out, range, name);
        }
        return;
    }
    for child in node.children() {
        walk(&child, scope, out);
    }
}

/// The name for the plain text of headings and emphasis.
fn markup_name(scope: &Scope) -> Option<&'static str> {
    if scope.heading {
        Some("title")
    } else {
        match (scope.strong, scope.emph) {
            (true, true) => Some("emphasis.both"),
            (true, false) => Some("emphasis.strong"),
            (false, true) => Some("emphasis"),
            (false, false) => None,
        }
    }
}

/// The theme name for a tag. Tags that only mark structure (headings,
/// emphasis) are handled through the scope, so their nested markup combines.
fn tag_name(tag: Tag, scope: &Scope) -> Option<&'static str> {
    if scope.math && tag != Tag::Comment {
        return Some("math");
    }
    Some(match tag {
        Tag::Comment => "comment",
        Tag::Punctuation => "punctuation",
        Tag::Escape => "string.escape",
        Tag::Strong | Tag::Emph | Tag::Heading => return None,
        Tag::Link => "link_uri",
        Tag::Raw => "text.literal",
        Tag::Label | Tag::Ref => "label",
        Tag::ListMarker => "punctuation.list_marker",
        Tag::ListTerm => "emphasis.strong",
        Tag::MathDelimiter | Tag::MathOperator | Tag::MathGroupingParens => "math",
        Tag::Keyword => "keyword",
        Tag::Operator => "operator",
        Tag::Number => "number",
        Tag::String => "string",
        Tag::Function => "function",
        Tag::Interpolated => "variable.special",
        Tag::Error => return None,
    })
}

fn push(out: &mut Highlights, range: Range<usize>, name: &'static str) {
    match out.last_mut() {
        Some((last, last_name)) if last.end == range.start && *last_name == name => {
            last.end = range.end;
        }
        _ => out.push((range, name)),
    }
}

/// Turns the highlights overlapping `range` into gap-free style runs.
fn resolve_styles(
    highlights: &[(Range<usize>, &'static str)],
    range: &Range<usize>,
    resolver: &dyn HighlightStyleResolver,
) -> Vec<(Range<usize>, HighlightStyle)> {
    let first = highlights.partition_point(|(highlight, _)| highlight.end <= range.start);
    let mut runs = Vec::new();
    let mut cursor = range.start;
    for (highlight, name) in &highlights[first..] {
        if highlight.start >= range.end {
            break;
        }
        let start = highlight.start.max(range.start);
        let end = highlight.end.min(range.end);
        if start >= end || end <= cursor {
            continue;
        }
        let start = start.max(cursor);
        if cursor < start {
            runs.push((cursor..start, HighlightStyle::default()));
        }
        runs.push((start..end, resolver.style(name).unwrap_or_default()));
        cursor = end;
    }
    if cursor < range.end {
        runs.push((cursor..range.end, HighlightStyle::default()));
    }
    runs
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(text: &str) -> Vec<(&'static str, String)> {
        let root = typst_syntax::parse(text);
        typst_highlights(&root)
            .into_iter()
            .map(|(range, name)| (name, text[range].to_string()))
            .collect()
    }

    fn has(text: &str, name: &str, fragment: &str) -> bool {
        names(text)
            .iter()
            .any(|(n, t)| *n == name && t.contains(fragment))
    }

    #[test]
    fn headings_emphasis_and_strong() {
        let text = "= Introduction\n\nSome _light_ and *heavy* and *_both_* words.\n";
        assert!(has(text, "title", "Introduction"));
        assert!(has(text, "emphasis", "light"));
        assert!(has(text, "emphasis.strong", "heavy"));
        assert!(has(text, "emphasis.both", "both"));
        assert!(!names(text).iter().any(|(_, t)| t.contains("words")));
    }

    #[test]
    fn math_raw_labels_refs_and_comments() {
        let text = "See @vaswani2017 and $x^2 + y$. `raw` here <intro>\n// a comment\n";
        assert!(has(text, "label", "@vaswani2017"));
        assert!(has(text, "label", "<intro>"));
        assert!(has(text, "math", "x"));
        assert!(has(text, "math", "$"));
        assert!(has(text, "text.literal", "raw"));
        assert!(has(text, "comment", "a comment"));
    }

    #[test]
    fn code_functions_strings_and_keywords() {
        let text = "#set text(font: \"Charter\")\n#let x = 1\n#figure(image(\"a.png\"))\n";
        assert!(has(text, "keyword", "set"));
        assert!(has(text, "keyword", "let"));
        assert!(has(text, "function", "figure"));
        assert!(has(text, "function", "image"));
        assert!(has(text, "string", "\"Charter\""));
        assert!(has(text, "number", "1"));
    }

    #[test]
    fn highlights_are_ordered_and_disjoint() {
        let text = "= A *b* $c$ #f(\"d\") // e\n";
        let root = typst_syntax::parse(text);
        let highlights = typst_highlights(&root);
        for pair in highlights.windows(2) {
            assert!(pair[0].0.end <= pair[1].0.start, "{highlights:?}");
        }
    }

    #[test]
    fn style_runs_cover_the_range() {
        let theme = OylSyntaxTheme::new(&Tokens::dark());
        let highlights: Highlights = vec![(2..4, "function"), (6..8, "comment")];
        let runs = resolve_styles(&highlights, &(0..10), &theme);
        let ranges: Vec<_> = runs.iter().map(|(range, _)| range.clone()).collect();
        assert_eq!(ranges, vec![0..2, 2..4, 4..6, 6..8, 8..10]);
        let partial = resolve_styles(&highlights, &(3..7), &theme);
        let ranges: Vec<_> = partial.iter().map(|(range, _)| range.clone()).collect();
        assert_eq!(ranges, vec![3..4, 4..6, 6..7]);
    }

    #[test]
    fn theme_uses_tokens_and_falls_back_to_prefixes() {
        let tokens = Tokens::light();
        let theme = OylSyntaxTheme::new(&tokens);
        assert_eq!(
            theme.style("function").and_then(|s| s.color),
            Some(tokens.accent)
        );
        assert_eq!(
            theme.style("comment.doc").and_then(|s| s.color),
            Some(tokens.text_faint)
        );
        assert_eq!(
            theme.style("punctuation.bracket").and_then(|s| s.color),
            Some(tokens.text_soft)
        );
        assert_eq!(
            theme.style("title").and_then(|s| s.font_weight),
            Some(FontWeight::BOLD)
        );
        assert!(theme.style("variable").is_none());
        let dark = OylSyntaxTheme::new(&Tokens::dark());
        assert_ne!(
            dark.style("function").and_then(|s| s.color),
            theme.style("function").and_then(|s| s.color)
        );
    }
}
