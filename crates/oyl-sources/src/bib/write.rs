//! Writing entries: a fresh entry in Oyl's layout, or the smallest edit of
//! an entry as the user wrote it.

use std::collections::HashMap;
use std::ops::Range;

use super::names::to_bibtex_list;
use super::{Origin, Source};
use crate::latex::{balanced, escape, escape_title, escape_verbatim};

/// The field a source's venue is written to, by entry type.
pub(crate) fn venue_field(kind: &str) -> &'static str {
    match kind {
        "article" | "periodical" | "suppperiodical" => "journal",
        "inproceedings" | "conference" | "incollection" | "inbook" | "inreference" | "suppbook"
        | "suppcollection" => "booktitle",
        "book" | "mvbook" | "booklet" | "proceedings" | "mvproceedings" | "collection"
        | "mvcollection" | "manual" | "reference" | "mvreference" => "publisher",
        "phdthesis" | "mastersthesis" | "thesis" => "school",
        "techreport" | "report" => "institution",
        _ => "howpublished",
    }
}

const MONTHS: [&str; 12] = [
    "jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec",
];

/// Whether `name` is one of BibTeX's predefined month macros.
pub(crate) fn is_month_macro(name: &str) -> bool {
    MONTHS.contains(&name.to_ascii_lowercase().as_str())
}

/// A raw TeX value in braces, or as a bare month macro.
fn tex_value(name: &str, tex: &str) -> String {
    if name.eq_ignore_ascii_case("month") && is_month_macro(tex.trim()) {
        return tex.trim().to_ascii_lowercase();
    }
    if balanced(tex) {
        format!("{{{tex}}}")
    } else {
        format!("{{{}}}", tex.replace('{', "\\{").replace('}', "\\}"))
    }
}

/// The member fields of a source as (field name, TeX value), in Oyl's
/// order, `None` for members that are empty.
fn members(
    source: &Source,
    known_names: &[(String, String)],
) -> [(&'static str, Option<String>); 7] {
    let text = |s: &Option<String>| {
        s.as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(escape)
    };
    let verbatim = |s: &Option<String>| {
        s.as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(escape_verbatim)
    };
    let title = Some(source.title.trim())
        .filter(|t| !t.is_empty())
        .map(escape_title);
    let authors = Some(to_bibtex_list(&source.authors, known_names)).filter(|a| !a.is_empty());
    [
        ("title", title),
        ("author", authors),
        ("year", source.year.map(|y| y.to_string())),
        ("venue", text(&source.venue)),
        ("doi", verbatim(&source.doi)),
        ("url", verbatim(&source.url)),
        ("abstract", text(&source.abstract_text)),
    ]
}

/// A new entry in Oyl's layout: two-space indent, braced values, the
/// members first and every other field after them in their order.
pub(crate) fn entry(source: &Source) -> String {
    let kind = if source.kind.trim().is_empty() {
        "misc".to_string()
    } else {
        source.kind.trim().to_ascii_lowercase()
    };
    let mut lines: Vec<String> = Vec::new();
    let [title, author, year, venue, doi, url, abs] = members(source, &[]);
    for (name, value) in [title, author, year] {
        if let Some(v) = value {
            lines.push(format!("  {name} = {{{v}}}"));
        }
    }
    if let Some(v) = venue.1 {
        lines.push(format!("  {} = {{{v}}}", venue_field(&kind)));
    }
    for (name, value) in &source.fields {
        let name = name.trim().to_ascii_lowercase();
        if name.is_empty() {
            continue;
        }
        lines.push(format!("  {name} = {}", tex_value(&name, value)));
    }
    for (name, value) in [doi, url, abs] {
        if let Some(v) = value {
            lines.push(format!("  {name} = {{{v}}}"));
        }
    }
    let mut out = format!("@{kind}{{{},", source.key);
    if lines.is_empty() {
        out.push_str("\n}");
        return out;
    }
    out.push('\n');
    out.push_str(&lines.join(",\n"));
    out.push_str("\n}");
    out
}

/// How the entry being edited is laid out, so added fields look like the
/// ones around them.
struct Style {
    /// Leading whitespace of a field line; `None` when fields share a line.
    indent: Option<String>,
    /// Width names are padded to before `=`, for aligned entries.
    align: Option<usize>,
    before_eq: String,
    after_eq: String,
    case: NameCase,
}

/// How the entry writes its field names.
#[derive(Clone, Copy, PartialEq, Eq)]
enum NameCase {
    Lower,
    Upper,
    Capitalized,
}

fn style(origin: &Origin) -> Style {
    let text = &origin.text;
    let mut st = Style {
        indent: Some("  ".to_string()),
        align: None,
        before_eq: " ".to_string(),
        after_eq: " ".to_string(),
        case: NameCase::Lower,
    };
    let fields = &origin.entry.fields;
    let Some(first) = fields.first() else {
        return st;
    };
    let line_start = text[..first.span.start].rfind('\n').map(|i| i + 1);
    st.indent = match line_start {
        Some(ls)
            if text[ls..first.span.start]
                .chars()
                .all(|c| c == ' ' || c == '\t') =>
        {
            Some(text[ls..first.span.start].to_string())
        }
        _ => None,
    };
    let name_end = first.span.start + first.name.len();
    let between = &text[name_end..first.value_span.start];
    if let Some(eq) = between.find('=') {
        let before = &between[..eq];
        let after = &between[eq + 1..];
        if before.chars().all(|c| c == ' ' || c == '\t')
            && after.chars().all(|c| c == ' ' || c == '\t')
        {
            st.after_eq = after.to_string();
            let widths: Vec<usize> = fields
                .iter()
                .filter_map(|f| {
                    let between = &text[f.span.start..f.value_span.start];
                    between.find('=')
                })
                .collect();
            let aligned = widths.len() > 1
                && widths.iter().all(|w| *w == widths[0])
                && fields.iter().any(|f| f.name.len() + 1 < widths[0]);
            if aligned {
                st.align = Some(widths[0]);
            } else {
                st.before_eq = before.to_string();
            }
        }
    }
    let upper = |f: &super::parse::RawField| !f.name.chars().any(|c| c.is_ascii_lowercase());
    let capitalized = |f: &super::parse::RawField| {
        f.name
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_uppercase())
            && !f.name.chars().skip(1).any(|c| c.is_ascii_uppercase())
    };
    st.case = if fields.iter().all(upper) {
        NameCase::Upper
    } else if fields.iter().all(capitalized) {
        NameCase::Capitalized
    } else {
        NameCase::Lower
    };
    st
}

impl Style {
    fn field(&self, name: &str, value: &str) -> String {
        let name = match self.case {
            NameCase::Lower => name.to_string(),
            NameCase::Upper => name.to_ascii_uppercase(),
            NameCase::Capitalized => {
                let mut chars = name.chars();
                chars
                    .next()
                    .map(|c| c.to_ascii_uppercase().to_string() + chars.as_str())
                    .unwrap_or_default()
            }
        };
        match self.align {
            Some(width) if width > name.len() => {
                format!(
                    "{name}{}={}{value}",
                    " ".repeat(width - name.len()),
                    self.after_eq
                )
            }
            Some(_) => format!("{name} ={}{value}", self.after_eq),
            None => format!("{name}{}={}{value}", self.before_eq, self.after_eq),
        }
    }
}

/// A replacement value in the delimiters the field used: quotes stay
/// quotes, bare numbers stay bare.
fn in_style(original: &str, tex: &str) -> String {
    let t = original.trim_start();
    if t.starts_with('"') && balanced(tex) && !tex.contains('"') {
        format!("\"{tex}\"")
    } else if t.chars().next().is_some_and(|c| c.is_ascii_digit())
        && !tex.is_empty()
        && tex.chars().all(|c| c.is_ascii_digit())
    {
        tex.to_string()
    } else {
        tex_value("", tex)
    }
}

/// The entry as the user wrote it, with only what changed between
/// `origin.derived` and `source` edited: changed values replaced in place,
/// removed fields cut with their line, new fields added after the last one
/// in the entry's own layout.
pub(crate) fn splice(origin: &Origin, source: &Source) -> String {
    let text = &origin.text;
    let entry = &origin.entry;
    let derived = &origin.derived;
    let mut edits: Vec<(Range<usize>, String)> = Vec::new();
    let mut removed = vec![false; entry.fields.len()];
    let mut added: Vec<(String, String)> = Vec::new();

    let kind = source.kind.trim().to_ascii_lowercase();
    if !kind.is_empty() && kind != derived.kind {
        edits.push((entry.kind_span.clone(), kind.clone()));
    }
    if source.key != derived.key {
        edits.push((entry.key_span.clone(), source.key.clone()));
    }

    // A year that lives in `date` (biblatex) is changed there.
    let mut target_fields = source.fields.clone();
    let mut year_handled = false;
    if source.year != derived.year
        && origin.map.year.is_none()
        && let (Some(new), Some(old)) = (source.year, derived.year)
    {
        for (name, value) in &mut target_fields {
            if name.eq_ignore_ascii_case("date") && value.trim_start().starts_with(&old.to_string())
            {
                *value = value.replacen(&old.to_string(), &new.to_string(), 1);
                year_handled = true;
                break;
            }
        }
    }

    let new_members = members(source, &origin.names);
    let slots = [
        origin.map.title,
        origin.map.author,
        origin.map.year,
        origin.map.venue,
        origin.map.doi,
        origin.map.url,
        origin.map.abstract_,
    ];
    let same_member = |i: usize| match i {
        0 => source.title == derived.title,
        1 => source.authors == derived.authors,
        2 => source.year == derived.year || year_handled,
        3 => source.venue == derived.venue,
        4 => source.doi == derived.doi,
        5 => source.url == derived.url,
        _ => source.abstract_text == derived.abstract_text,
    };
    for (i, (name, new)) in new_members.iter().enumerate() {
        if same_member(i) {
            continue;
        }
        let field_name = if *name == "venue" {
            venue_field(&kind)
        } else {
            name
        };
        match (slots[i], new) {
            (Some(f), Some(v)) => {
                let field = &entry.fields[f];
                let original = &text[field.value_span.clone()];
                edits.push((field.value_span.clone(), in_style(original, v)));
            }
            (Some(f), None) => removed[f] = true,
            (None, Some(v)) => added.push((field_name.to_string(), format!("{{{v}}}"))),
            (None, None) => {}
        }
    }

    // Every other field, matched by name and occurrence.
    let mapped: Vec<usize> = slots.iter().flatten().copied().collect();
    let mut want: HashMap<String, Vec<&String>> = HashMap::new();
    for (name, value) in &target_fields {
        want.entry(name.trim().to_ascii_lowercase())
            .or_default()
            .push(value);
    }
    let mut had: HashMap<String, Vec<&String>> = HashMap::new();
    for (name, value) in &derived.fields {
        had.entry(name.to_ascii_lowercase())
            .or_default()
            .push(value);
    }
    let mut seen: HashMap<String, usize> = HashMap::new();
    for (f, field) in entry.fields.iter().enumerate() {
        if mapped.contains(&f) {
            continue;
        }
        let name = field.name.to_ascii_lowercase();
        let k = seen.entry(name.clone()).or_insert(0);
        let old = had.get(&name).and_then(|v| v.get(*k)).copied();
        let new = want.get(&name).and_then(|v| v.get(*k)).copied();
        *k += 1;
        match new {
            None => removed[f] = true,
            Some(new) if Some(new) != old => {
                let original = &text[field.value_span.clone()];
                let value = if name == "month" && is_month_macro(new.trim()) {
                    new.trim().to_ascii_lowercase()
                } else {
                    in_style(original, new)
                };
                edits.push((field.value_span.clone(), value));
            }
            Some(_) => {}
        }
    }
    // Fields the entry did not have yet, in the order the source lists them.
    let mut counts: HashMap<String, usize> = HashMap::new();
    for (name, value) in &target_fields {
        let name = name.trim().to_ascii_lowercase();
        if name.is_empty() {
            continue;
        }
        let k = counts.entry(name.clone()).or_insert(0);
        *k += 1;
        if *k > seen.get(&name).copied().unwrap_or(0) {
            added.push((name.clone(), tex_value(&name, value)));
        }
    }

    for (f, gone) in removed.iter().enumerate() {
        if *gone {
            edits.push((removal_range(text, &entry.fields[f]), String::new()));
        }
    }

    if !added.is_empty() {
        let st = style(origin);
        let rendered: Vec<String> = added.iter().map(|(n, v)| st.field(n, v)).collect();
        let last_kept = (0..entry.fields.len()).rev().find(|&f| !removed[f]);
        match (last_kept, &st.indent) {
            (Some(f), indent) => {
                let field = &entry.fields[f];
                let sep = match indent {
                    Some(ind) => format!(",\n{ind}"),
                    None => ", ".to_string(),
                };
                match field.comma {
                    Some(c) => {
                        // The entry ends its fields with a comma: so do the new ones.
                        let ins: String = rendered
                            .iter()
                            .map(|r| format!("{}{r},", &sep[1..]))
                            .collect();
                        edits.push((c + 1..c + 1, ins));
                    }
                    None => {
                        let ins: String = rendered.iter().map(|r| format!("{sep}{r}")).collect();
                        let at = field.value_span.end;
                        edits.push((at..at, ins));
                    }
                }
            }
            (None, indent) => {
                let ind = indent.clone().unwrap_or_else(|| "  ".to_string());
                let after_key = &text[entry.key_span.end..entry.close];
                if !after_key.trim_start().starts_with(',') {
                    edits.push((entry.key_span.end..entry.key_span.end, ",".to_string()));
                }
                let mut ins = String::new();
                if !text[..entry.close].ends_with('\n') {
                    ins.push('\n');
                }
                ins.push_str(
                    &rendered
                        .iter()
                        .map(|r| format!("{ind}{r}"))
                        .collect::<Vec<_>>()
                        .join(",\n"),
                );
                ins.push('\n');
                edits.push((entry.close..entry.close, ins));
            }
        }
    }

    edits.sort_by(|a, b| b.0.start.cmp(&a.0.start).then(b.0.end.cmp(&a.0.end)));
    let mut out = text.clone();
    for (range, replacement) in edits {
        out.replace_range(range, &replacement);
    }
    out
}

/// The text a removed field takes with it: its whole line when it has one
/// of its own, else just the field and its comma.
fn removal_range(text: &str, field: &super::parse::RawField) -> Range<usize> {
    let b = text.as_bytes();
    let mut start = field.span.start;
    let mut end = field.comma.map(|c| c + 1).unwrap_or(field.span.end);
    let ls = text[..start].rfind('\n').map(|i| i + 1).unwrap_or(0);
    let own_line_start = text[ls..start].chars().all(|c| c == ' ' || c == '\t');
    let mut e = end;
    while e < b.len() && (b[e] == b' ' || b[e] == b'\t' || b[e] == b'\r') {
        e += 1;
    }
    let own_line_end = e >= b.len() || b[e] == b'\n';
    if own_line_start && own_line_end && ls > 0 {
        start = ls;
        end = if e < b.len() { e + 1 } else { e };
    } else {
        end = e;
    }
    start..end
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fresh_entry_layout() {
        let s = Source {
            key: "vaswani2017".into(),
            kind: "inproceedings".into(),
            title: "Attention Is All You Need".into(),
            authors: vec!["Ashish Vaswani".into(), "Noam Shazeer".into()],
            year: Some(2017),
            venue: Some("Advances in Neural Information Processing Systems".into()),
            doi: None,
            url: Some("https://arxiv.org/abs/1706.03762".into()),
            abstract_text: None,
            fields: vec![
                ("pages".into(), "5998--6008".into()),
                ("month".into(), "jun".into()),
            ],
        };
        assert_eq!(
            entry(&s),
            "@inproceedings{vaswani2017,\n  title = {Attention Is All You Need},\n  author = {Vaswani, Ashish and Shazeer, Noam},\n  year = {2017},\n  booktitle = {Advances in Neural Information Processing Systems},\n  pages = {5998--6008},\n  month = jun,\n  url = {https://arxiv.org/abs/1706.03762}\n}"
        );
    }

    #[test]
    fn empty_entry() {
        let s = Source {
            key: "k".into(),
            ..Source::default()
        };
        assert_eq!(entry(&s), "@misc{k,\n}");
    }
}
