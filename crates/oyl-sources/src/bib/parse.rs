//! A lossless, forgiving BibTeX reader.
//!
//! Every byte of the file ends up in exactly one [`Block`], so the file can be
//! written back unchanged. A malformed entry does not stop the rest: it is
//! kept as text, reported as a [`Problem`], and reading goes on at the next
//! entry. Entries may be delimited by braces or parentheses; values may be
//! braced, quoted, numbers, `@string` macros, and `#` concatenations of these.

use std::ops::Range;

/// One part of a field value.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Piece {
    /// `{text}`, without the outer braces.
    Braced(String),
    /// `"text"`, without the quotes.
    Quoted(String),
    /// A bare number.
    Number(String),
    /// A bare `@string` macro name.
    Macro(String),
}

/// A `name = value` pair.
#[derive(Clone, Debug)]
pub(crate) struct RawField {
    /// The name as written.
    pub name: String,
    /// From the name's first byte to the value's last.
    pub span: Range<usize>,
    /// The value, delimiters included.
    pub value_span: Range<usize>,
    pub pieces: Vec<Piece>,
    /// The byte index of the comma after the value, if there is one.
    pub comma: Option<usize>,
}

/// An entry: `@kind{key, fields…}`.
#[derive(Clone, Debug)]
pub(crate) struct RawEntry {
    /// From the `@` to the closing delimiter, inclusive.
    pub span: Range<usize>,
    pub kind: String,
    pub kind_span: Range<usize>,
    pub key: String,
    pub key_span: Range<usize>,
    pub fields: Vec<RawField>,
    /// The byte index of the closing delimiter.
    pub close: usize,
}

impl RawEntry {
    /// The same entry with every span moved `by` bytes to the left, for an
    /// entry cut out of its file.
    pub(crate) fn shifted(mut self, by: usize) -> Self {
        let sh = |r: &mut Range<usize>| {
            r.start -= by;
            r.end -= by;
        };
        sh(&mut self.span);
        sh(&mut self.kind_span);
        sh(&mut self.key_span);
        self.close -= by;
        for f in &mut self.fields {
            sh(&mut f.span);
            sh(&mut f.value_span);
            if let Some(c) = f.comma.as_mut() {
                *c -= by;
            }
        }
        self
    }
}

/// A run of the file.
#[derive(Clone, Debug)]
pub(crate) enum Block {
    /// Anything that is not an entry or a string definition: whitespace,
    /// comments, `@comment`, `@preamble`, and entries that could not be read.
    Text(Range<usize>),
    /// `@string{name = value, …}`.
    Strings(Range<usize>, Vec<RawField>),
    Entry(RawEntry),
}

/// Something that could not be read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Problem {
    pub offset: usize,
    pub message: String,
}

/// The blocks of a file, in order, covering it entirely.
pub(crate) struct Parsed {
    pub blocks: Vec<Block>,
    pub problems: Vec<Problem>,
}

/// Reads a BibTeX file.
pub(crate) fn parse(src: &str) -> Parsed {
    let mut p = Parser {
        src,
        b: src.as_bytes(),
        pos: 0,
    };
    let mut blocks = Vec::new();
    let mut problems = Vec::new();
    let mut text_start = 0;
    while p.pos < p.b.len() {
        match p.b[p.pos] {
            b'@' => {
                let start = p.pos;
                match p.block() {
                    Ok(Some(block)) => {
                        if text_start < start {
                            blocks.push(Block::Text(text_start..start));
                        }
                        blocks.push(block);
                        text_start = p.pos;
                    }
                    Ok(None) => {}
                    Err(message) => {
                        problems.push(Problem {
                            offset: start,
                            message,
                        });
                        p.pos = p.recover(start + 1);
                    }
                }
            }
            b'%' => p.skip_line(),
            _ => p.pos += 1,
        }
    }
    if text_start < p.b.len() {
        blocks.push(Block::Text(text_start..p.b.len()));
    }
    Parsed { blocks, problems }
}

struct Parser<'s> {
    src: &'s str,
    b: &'s [u8],
    pos: usize,
}

type PResult<T> = Result<T, String>;

fn is_name_byte(c: u8) -> bool {
    !c.is_ascii_whitespace()
        && !matches!(
            c,
            b'=' | b',' | b'{' | b'}' | b'(' | b')' | b'"' | b'#' | b'%' | b'@' | b'\''
        )
}

impl<'s> Parser<'s> {
    fn at(&self) -> Option<u8> {
        self.b.get(self.pos).copied()
    }

    fn skip_line(&mut self) {
        while let Some(c) = self.at() {
            self.pos += 1;
            if c == b'\n' {
                break;
            }
        }
    }

    fn skip_ws(&mut self) {
        while matches!(self.at(), Some(c) if c.is_ascii_whitespace()) {
            self.pos += 1;
        }
    }

    /// Whitespace and `%` comments between the parts of an entry.
    fn skip_ws_comments(&mut self) {
        loop {
            self.skip_ws();
            if self.at() == Some(b'%') {
                self.skip_line();
            } else {
                break;
            }
        }
    }

    fn name(&mut self) -> &'s str {
        let start = self.pos;
        while matches!(self.at(), Some(c) if is_name_byte(c)) {
            self.pos += 1;
        }
        &self.src[start..self.pos]
    }

    /// Where reading goes on after a broken entry: the next `@` that starts
    /// a line, else the next `@`, else the end.
    fn recover(&self, from: usize) -> usize {
        let mut line_start = true;
        for (i, &c) in self.b.iter().enumerate().skip(from) {
            if c == b'@' && line_start {
                return i;
            }
            if c == b'\n' {
                line_start = true;
            } else if !(c == b' ' || c == b'\t' || c == b'\r') {
                line_start = false;
            }
        }
        self.b[from.min(self.b.len())..]
            .iter()
            .position(|&c| c == b'@')
            .map(|i| from + i)
            .unwrap_or(self.b.len())
    }

    /// Reads the block at an `@`. `Ok(None)` for blocks kept as text.
    fn block(&mut self) -> PResult<Option<Block>> {
        let start = self.pos;
        self.pos += 1;
        self.skip_ws();
        let kind_start = self.pos;
        let kind = self.name();
        if kind.is_empty() {
            return Err("expected an entry type after @".into());
        }
        let kind_span = kind_start..self.pos;
        self.skip_ws();
        let close = match self.at() {
            Some(b'{') => b'}',
            Some(b'(') => b')',
            _ => return Err(format!("expected {{ after @{kind}")),
        };
        self.pos += 1;
        match kind.to_ascii_lowercase().as_str() {
            "comment" | "preamble" => {
                self.skip_balanced(close)?;
                Ok(None)
            }
            "string" => {
                let fields = self.fields(close)?;
                Ok(Some(Block::Strings(start..self.pos, fields)))
            }
            _ => {
                self.skip_ws_comments();
                let key_start = self.pos;
                while matches!(self.at(), Some(c) if !c.is_ascii_whitespace() && c != b',' && c != close && c != b'}')
                {
                    self.pos += 1;
                }
                let key_span = key_start..self.pos;
                let key = self.src[key_span.clone()].to_string();
                self.skip_ws_comments();
                match self.at() {
                    Some(b',') => self.pos += 1,
                    Some(c) if c == close => {}
                    _ => return Err(format!("expected a comma after the key {key:?}")),
                }
                let fields = self.fields(close)?;
                Ok(Some(Block::Entry(RawEntry {
                    span: start..self.pos,
                    kind: kind.to_string(),
                    kind_span,
                    key,
                    key_span,
                    fields,
                    close: self.pos - 1,
                })))
            }
        }
    }

    /// Skips to the delimiter closing a `@comment` or `@preamble`.
    fn skip_balanced(&mut self, close: u8) -> PResult<()> {
        let mut depth = 0i32;
        while let Some(c) = self.at() {
            self.pos += 1;
            match c {
                b'{' => depth += 1,
                b'}' if close == b'}' && depth == 0 => return Ok(()),
                b'}' => depth -= 1,
                b')' if close == b')' && depth == 0 => return Ok(()),
                _ => {}
            }
        }
        Err("the block is not closed".into())
    }

    /// Reads `name = value` pairs up to and including the closing delimiter.
    fn fields(&mut self, close: u8) -> PResult<Vec<RawField>> {
        let mut fields = Vec::new();
        loop {
            self.skip_ws_comments();
            match self.at() {
                None => return Err("the entry is not closed".into()),
                Some(c) if c == close => {
                    self.pos += 1;
                    return Ok(fields);
                }
                Some(b',') => {
                    self.pos += 1;
                    continue;
                }
                _ => {}
            }
            let name_start = self.pos;
            let name = self.name();
            if name.is_empty() {
                return Err(format!(
                    "expected a field name, found {:?}",
                    self.src[self.pos..].chars().next().unwrap_or(' ')
                ));
            }
            self.skip_ws_comments();
            if self.at() != Some(b'=') {
                return Err(format!("expected = after the field {name}"));
            }
            self.pos += 1;
            self.skip_ws_comments();
            let value_start = self.pos;
            let mut pieces = Vec::new();
            let mut value_end;
            loop {
                pieces.push(self.piece(name)?);
                value_end = self.pos;
                self.skip_ws_comments();
                if self.at() == Some(b'#') {
                    self.pos += 1;
                    self.skip_ws_comments();
                } else {
                    break;
                }
            }
            let comma = match self.at() {
                Some(b',') => {
                    self.pos += 1;
                    Some(self.pos - 1)
                }
                Some(c) if c == close => None,
                None => return Err("the entry is not closed".into()),
                _ => return Err(format!("expected a comma after the field {name}")),
            };
            fields.push(RawField {
                name: name.to_string(),
                span: name_start..value_end,
                value_span: value_start..value_end,
                pieces,
                comma,
            });
        }
    }

    fn piece(&mut self, field: &str) -> PResult<Piece> {
        match self.at() {
            Some(b'{') => {
                self.pos += 1;
                let start = self.pos;
                let mut depth = 1;
                while let Some(c) = self.at() {
                    match c {
                        b'\\' => self.pos += 1,
                        b'{' => depth += 1,
                        b'}' => {
                            depth -= 1;
                            if depth == 0 {
                                let text = self.src[start..self.pos].to_string();
                                self.pos += 1;
                                return Ok(Piece::Braced(text));
                            }
                        }
                        _ => {}
                    }
                    self.pos += 1;
                }
                Err(format!("unclosed brace in the field {field}"))
            }
            Some(b'"') => {
                self.pos += 1;
                let start = self.pos;
                let mut depth = 0;
                while let Some(c) = self.at() {
                    match c {
                        b'\\' => self.pos += 1,
                        b'{' => depth += 1,
                        b'}' => depth -= 1,
                        b'"' if depth <= 0 => {
                            let text = self.src[start..self.pos].to_string();
                            self.pos += 1;
                            return Ok(Piece::Quoted(text));
                        }
                        _ => {}
                    }
                    self.pos += 1;
                }
                Err(format!("unclosed quote in the field {field}"))
            }
            Some(c) if c.is_ascii_digit() => {
                let start = self.pos;
                while matches!(self.at(), Some(c) if c.is_ascii_digit()) {
                    self.pos += 1;
                }
                Ok(Piece::Number(self.src[start..self.pos].to_string()))
            }
            Some(c) if is_name_byte(c) => Ok(Piece::Macro(self.name().to_string())),
            _ => Err(format!("expected a value for the field {field}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entries(p: &Parsed) -> Vec<&RawEntry> {
        p.blocks
            .iter()
            .filter_map(|b| match b {
                Block::Entry(e) => Some(e),
                _ => None,
            })
            .collect()
    }

    fn covers(src: &str, p: &Parsed) {
        let mut at = 0;
        for b in &p.blocks {
            let r = match b {
                Block::Text(r) | Block::Strings(r, _) => r.clone(),
                Block::Entry(e) => e.span.clone(),
            };
            assert_eq!(r.start, at, "blocks must be contiguous");
            at = r.end;
        }
        assert_eq!(at, src.len());
    }

    #[test]
    fn reads_entries_values_and_spans() {
        let src = "% my refs\n@Article{vaswani2017,\n  title = {Attention Is {All} You Need},\n  author = \"Vaswani, Ashish and Shazeer, Noam\",\n  year = 2017,\n  journal = nips # \" 30\",\n}\n";
        let p = parse(src);
        assert!(p.problems.is_empty(), "{:?}", p.problems);
        covers(src, &p);
        let e = entries(&p);
        assert_eq!(e.len(), 1);
        let e = e[0];
        assert_eq!(e.kind, "Article");
        assert_eq!(e.key, "vaswani2017");
        assert_eq!(e.fields.len(), 4);
        assert_eq!(
            e.fields[0].pieces,
            vec![Piece::Braced("Attention Is {All} You Need".into())]
        );
        assert_eq!(
            &src[e.fields[0].value_span.clone()],
            "{Attention Is {All} You Need}"
        );
        assert_eq!(e.fields[2].pieces, vec![Piece::Number("2017".into())]);
        assert_eq!(
            e.fields[3].pieces,
            vec![Piece::Macro("nips".into()), Piece::Quoted(" 30".into())]
        );
        assert_eq!(e.fields[3].comma.map(|c| &src[c..c + 1]), Some(","));
        assert_eq!(&src[e.close..e.close + 1], "}");
    }

    #[test]
    fn parentheses_strings_comments_and_preamble() {
        let src = "@string{nips = \"NeurIPS\"}\n@preamble{ \"\\newcommand{\\noop}[1]{}\" }\n@comment{ nested {braces} here }\n@misc(key2, title={X})\n";
        let p = parse(src);
        assert!(p.problems.is_empty(), "{:?}", p.problems);
        covers(src, &p);
        assert!(matches!(&p.blocks[0], Block::Strings(_, f) if f[0].name == "nips"));
        let e = entries(&p);
        assert_eq!(e.len(), 1);
        assert_eq!(e[0].key, "key2");
        assert_eq!(&src[e[0].close..e[0].close + 1], ")");
    }

    #[test]
    fn broken_entry_is_kept_and_reading_goes_on() {
        let src = "@article{good1, title={A}}\n@article{bad, title={unclosed}\n  year = \n@book{good2, title={B}}\n";
        let p = parse(src);
        covers(src, &p);
        assert_eq!(p.problems.len(), 1);
        let keys: Vec<_> = entries(&p).iter().map(|e| e.key.clone()).collect();
        assert_eq!(keys, vec!["good1", "good2"]);
    }

    #[test]
    fn email_in_comment_text_is_not_an_entry() {
        let src =
            "Written by someone at example.org\n% contact: me@example.org\n@misc{k, note={x}}";
        let p = parse(src);
        covers(src, &p);
        assert_eq!(entries(&p).len(), 1);
    }

    #[test]
    fn utf8_and_escapes() {
        let src = "@misc{k, title = {Über \\{x\\} {\\\"o}}, note = \"a {\"} b\"}";
        let p = parse(src);
        assert!(p.problems.is_empty(), "{:?}", p.problems);
        let e = entries(&p)[0];
        assert_eq!(
            e.fields[0].pieces,
            vec![Piece::Braced("Über \\{x\\} {\\\"o}".into())]
        );
        assert_eq!(e.fields[1].pieces, vec![Piece::Quoted("a {\"} b".into())]);
    }
}
