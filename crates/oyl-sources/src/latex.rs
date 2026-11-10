//! BibTeX field text ↔ display text.
//!
//! [`to_unicode`] turns the TeX inside a field (`{\"u}ber`, `--`, `\emph{x}`,
//! `$\alpha$`) into what a person reads (`über`, `–`, `x`, `α`).
//! [`escape`] goes the other way for text a person typed, escaping what TeX
//! would read as commands. Non-ASCII text is written as UTF-8, which BibTeX,
//! biber and Typst all read.

use unicode_normalization::char::compose;

/// The display form of a field's TeX: accents and commands resolved, braces
/// dropped, dashes and quotes typeset, whitespace collapsed.
pub fn to_unicode(tex: &str) -> String {
    let mut d = Decoder {
        chars: tex.chars().collect(),
        pos: 0,
        math: false,
    };
    let raw = d.run(None);
    collapse_ws(&raw)
}

/// The display form of a verbatim field (`url`, `doi`, `file`, `eprint`):
/// only the outer grouping braces and escaped specials are undone.
pub fn verbatim_to_unicode(tex: &str) -> String {
    let mut out = String::with_capacity(tex.len());
    let mut chars = tex.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\\' if matches!(
                chars.peek(),
                Some('_' | '%' | '#' | '&' | '$' | '{' | '}' | '~')
            ) =>
            {
                if let Some(next) = chars.next() {
                    out.push(next);
                }
            }
            '{' | '}' => {}
            _ => out.push(c),
        }
    }
    out.trim().to_string()
}

/// Escapes text a person typed so TeX prints it as written: `&`, `%`, `#`,
/// `$`, `_`, braces, `~`, `^` and backslashes.
pub fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 8);
    for c in text.chars() {
        match c {
            '&' | '%' | '#' | '$' | '_' | '{' | '}' => {
                out.push('\\');
                out.push(c);
            }
            '\\' => out.push_str("\\textbackslash{}"),
            '~' => out.push_str("\\textasciitilde{}"),
            '^' => out.push_str("\\textasciicircum{}"),
            '\n' | '\r' | '\t' => out.push(' '),
            _ => out.push(c),
        }
    }
    out
}

/// Escapes a title like [`escape`] and also protects the words a style must
/// not lowercase — acronyms and words with inner capitals (`{BERT}`,
/// `{ImageNet}`, `{GPT-3}`) — with braces.
pub fn escape_title(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 8);
    let mut word = String::new();
    let flush = |word: &mut String, out: &mut String| {
        if word.is_empty() {
            return;
        }
        let escaped = escape(word);
        let inner_capital = word.chars().skip(1).any(char::is_uppercase);
        if inner_capital {
            out.push('{');
            out.push_str(&escaped);
            out.push('}');
        } else {
            out.push_str(&escaped);
        }
        word.clear();
    };
    for c in text.chars() {
        if c.is_whitespace() {
            flush(&mut word, &mut out);
            out.push(' ');
        } else {
            word.push(c);
        }
    }
    flush(&mut word, &mut out);
    out
}

/// Escapes a verbatim value (a URL, a DOI): only unbalanced braces could
/// break the field, so only they are touched.
pub fn escape_verbatim(text: &str) -> String {
    let text = text.trim();
    if balanced(text) {
        text.to_string()
    } else {
        text.replace('{', "%7B").replace('}', "%7D")
    }
}

/// Whether the braces of `text` balance, ignoring escaped ones.
pub fn balanced(text: &str) -> bool {
    let mut depth: i64 = 0;
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        match c {
            '\\' => {
                chars.next();
            }
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth < 0 {
                    return false;
                }
            }
            _ => {}
        }
    }
    depth == 0
}

fn collapse_ws(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut space = false;
    for c in s.chars() {
        if c.is_whitespace() && c != '\u{a0}' {
            space = true;
        } else {
            if space && !out.is_empty() {
                out.push(' ');
            }
            space = false;
            out.push(c);
        }
    }
    out
}

struct Decoder {
    chars: Vec<char>,
    pos: usize,
    math: bool,
}

impl Decoder {
    fn peek(&self) -> Option<char> {
        self.chars.get(self.pos).copied()
    }

    fn peek_at(&self, offset: usize) -> Option<char> {
        self.chars.get(self.pos + offset).copied()
    }

    /// Decodes until the end, or until the closing brace of the group being
    /// read (`close` is `Some('}')`), which is consumed.
    fn run(&mut self, close: Option<char>) -> String {
        let mut out = String::new();
        while let Some(c) = self.peek() {
            if Some(c) == close {
                self.pos += 1;
                return out;
            }
            self.pos += 1;
            match c {
                '\\' => self.command(&mut out),
                '{' => {
                    let inner = self.run(Some('}'));
                    out.push_str(&inner);
                }
                '}' => {}
                '$' => {
                    // `$$` opens display math, same as `$` for our purpose.
                    if self.peek() == Some('$') {
                        self.pos += 1;
                    }
                    self.math = !self.math;
                }
                '~' => out.push(' '),
                '-' if !self.math => {
                    if self.peek() == Some('-') && self.peek_at(1) == Some('-') {
                        self.pos += 2;
                        out.push('—');
                    } else if self.peek() == Some('-') {
                        self.pos += 1;
                        out.push('–');
                    } else {
                        out.push('-');
                    }
                }
                '`' if !self.math => {
                    if self.peek() == Some('`') {
                        self.pos += 1;
                        out.push('“');
                    } else {
                        out.push('‘');
                    }
                }
                '\'' if !self.math && self.peek() == Some('\'') => {
                    self.pos += 1;
                    out.push('”');
                }
                _ => out.push(c),
            }
        }
        out
    }

    /// Reads one argument: a `{group}`, a `\command`, or a single character.
    fn argument(&mut self) -> String {
        while matches!(self.peek(), Some(c) if c == ' ' || c == '\t' || c == '\n') {
            self.pos += 1;
        }
        match self.peek() {
            Some('{') => {
                self.pos += 1;
                self.run(Some('}'))
            }
            Some('\\') => {
                self.pos += 1;
                let mut s = String::new();
                self.command(&mut s);
                s
            }
            Some(c) => {
                self.pos += 1;
                c.to_string()
            }
            None => String::new(),
        }
    }

    /// Reads a braced argument when one follows (`\emph{x}`), else nothing.
    fn optional_group(&mut self) -> Option<String> {
        let save = self.pos;
        while matches!(self.peek(), Some(' ' | '\t' | '\n')) {
            self.pos += 1;
        }
        if self.peek() == Some('{') {
            self.pos += 1;
            Some(self.run(Some('}')))
        } else {
            self.pos = save;
            None
        }
    }

    fn command(&mut self, out: &mut String) {
        let Some(first) = self.peek() else {
            return;
        };
        let name: String = if first.is_ascii_alphabetic() {
            let start = self.pos;
            while matches!(self.peek(), Some(c) if c.is_ascii_alphabetic()) {
                self.pos += 1;
            }
            self.chars[start..self.pos].iter().collect()
        } else {
            self.pos += 1;
            first.to_string()
        };
        let lettered = first.is_ascii_alphabetic();

        if let Some(mark) = accent(&name) {
            let arg = self.argument();
            out.push_str(&apply_accent(&arg, mark));
            return;
        }
        if lettered {
            // TeX swallows the spaces after a control word.
            let at_space = matches!(self.peek(), Some(' ' | '\t' | '\n'));
            if let Some(s) = symbol(&name) {
                out.push_str(s);
                if at_space {
                    self.skip_spaces();
                } else {
                    self.skip_empty_group();
                }
                return;
            }
            match name.as_str() {
                "url" | "nolinkurl" | "path" => {
                    if let Some(arg) = self.optional_group() {
                        out.push_str(&arg);
                    }
                }
                "href" => {
                    let _url = self.optional_group();
                    if let Some(text) = self.optional_group() {
                        out.push_str(&text);
                    }
                }
                "enquote" | "mkbibquote" => {
                    if let Some(arg) = self.optional_group() {
                        out.push('“');
                        out.push_str(&arg);
                        out.push('”');
                    }
                }
                "sqrt" => {
                    if let Some(arg) = self.optional_group() {
                        out.push('√');
                        out.push_str(&arg);
                    }
                }
                "mathbb" => {
                    if let Some(arg) = self.optional_group() {
                        out.push_str(&blackboard(&arg));
                    }
                }
                "frac" => {
                    let a = self.optional_group().unwrap_or_default();
                    let b = self.optional_group().unwrap_or_default();
                    out.push_str(&a);
                    out.push('/');
                    out.push_str(&b);
                }
                _ => {
                    // Formatting commands (`\emph{x}`, `\textsc{x}`) keep their
                    // argument; switches (`\em`, `\bf`) and unknown commands
                    // without one disappear.
                    if let Some(arg) = self.optional_group() {
                        out.push_str(&arg);
                    } else if at_space {
                        self.skip_spaces();
                    }
                }
            }
            return;
        }
        match name.as_str() {
            "&" | "%" | "$" | "#" | "_" | "{" | "}" => out.push_str(&name),
            " " | "," | ";" | ":" | "!" | "\\" | "\n" => out.push(' '),
            "-" | "/" | "@" => {}
            other => out.push_str(other),
        }
    }

    fn skip_spaces(&mut self) {
        while matches!(self.peek(), Some(' ' | '\t' | '\n')) {
            self.pos += 1;
        }
    }

    fn skip_empty_group(&mut self) {
        if self.peek() == Some('{') && self.peek_at(1) == Some('}') {
            self.pos += 2;
        }
    }
}

/// The combining mark an accent command puts on its argument.
fn accent(name: &str) -> Option<char> {
    Some(match name {
        "`" => '\u{300}',
        "'" => '\u{301}',
        "^" => '\u{302}',
        "~" => '\u{303}',
        "=" => '\u{304}',
        "u" => '\u{306}',
        "." => '\u{307}',
        "\"" => '\u{308}',
        "r" => '\u{30A}',
        "H" => '\u{30B}',
        "v" => '\u{30C}',
        "d" => '\u{323}',
        "c" => '\u{327}',
        "k" => '\u{328}',
        "b" => '\u{331}',
        _ => return None,
    })
}

fn apply_accent(arg: &str, mark: char) -> String {
    let mut chars = arg.chars();
    let Some(first) = chars.next() else {
        return mark.to_string();
    };
    let base = match first {
        'ı' => 'i',
        'ȷ' => 'j',
        c => c,
    };
    let mut out = String::new();
    match compose(base, mark) {
        Some(composed) => out.push(composed),
        None => {
            out.push(base);
            out.push(mark);
        }
    }
    out.extend(chars);
    out
}

/// Commands that stand for a character or a word.
fn symbol(name: &str) -> Option<&'static str> {
    Some(match name {
        "ss" => "ß",
        "SS" => "SS",
        "o" => "ø",
        "O" => "Ø",
        "ae" => "æ",
        "AE" => "Æ",
        "oe" => "œ",
        "OE" => "Œ",
        "aa" => "å",
        "AA" => "Å",
        "l" => "ł",
        "L" => "Ł",
        "i" => "ı",
        "j" => "ȷ",
        "dh" => "ð",
        "DH" => "Ð",
        "th" => "þ",
        "TH" => "Þ",
        "ng" => "ŋ",
        "NG" => "Ŋ",
        "dj" => "đ",
        "DJ" => "Đ",
        "textendash" => "–",
        "textemdash" => "—",
        "textquoteleft" => "‘",
        "textquoteright" => "’",
        "textquotedblleft" => "“",
        "textquotedblright" => "”",
        "textquotedbl" => "\"",
        "guillemotleft" | "guillemetleft" => "«",
        "guillemotright" | "guillemetright" => "»",
        "dots" | "ldots" | "textellipsis" => "…",
        "textbackslash" => "\\",
        "textasciitilde" => "~",
        "textasciicircum" => "^",
        "textunderscore" => "_",
        "textbar" => "|",
        "textless" => "<",
        "textgreater" => ">",
        "textbullet" => "•",
        "textdagger" | "dag" => "†",
        "textdaggerdbl" | "ddag" => "‡",
        "textsection" | "S" => "§",
        "textparagraph" | "P" => "¶",
        "copyright" | "textcopyright" => "©",
        "textregistered" => "®",
        "texttrademark" => "™",
        "pounds" | "textsterling" => "£",
        "euro" | "texteuro" => "€",
        "textdegree" => "°",
        "textperiodcentered" => "·",
        "textexclamdown" => "¡",
        "textquestiondown" => "¿",
        "LaTeX" => "LaTeX",
        "LaTeXe" => "LaTeX2ε",
        "TeX" => "TeX",
        "BibTeX" => "BibTeX",
        "XeTeX" => "XeTeX",
        "alpha" => "α",
        "beta" => "β",
        "gamma" => "γ",
        "delta" => "δ",
        "epsilon" => "ϵ",
        "varepsilon" => "ε",
        "zeta" => "ζ",
        "eta" => "η",
        "theta" => "θ",
        "vartheta" => "ϑ",
        "iota" => "ι",
        "kappa" => "κ",
        "lambda" => "λ",
        "mu" => "μ",
        "nu" => "ν",
        "xi" => "ξ",
        "pi" => "π",
        "rho" => "ρ",
        "sigma" => "σ",
        "tau" => "τ",
        "upsilon" => "υ",
        "phi" => "ϕ",
        "varphi" => "φ",
        "chi" => "χ",
        "psi" => "ψ",
        "omega" => "ω",
        "Gamma" => "Γ",
        "Delta" => "Δ",
        "Theta" => "Θ",
        "Lambda" => "Λ",
        "Xi" => "Ξ",
        "Pi" => "Π",
        "Sigma" => "Σ",
        "Upsilon" => "Υ",
        "Phi" => "Φ",
        "Psi" => "Ψ",
        "Omega" => "Ω",
        "times" => "×",
        "cdot" => "·",
        "pm" => "±",
        "mp" => "∓",
        "leq" | "le" => "≤",
        "geq" | "ge" => "≥",
        "neq" | "ne" => "≠",
        "approx" => "≈",
        "sim" => "∼",
        "equiv" => "≡",
        "infty" => "∞",
        "to" | "rightarrow" => "→",
        "leftarrow" | "gets" => "←",
        "Rightarrow" => "⇒",
        "leftrightarrow" => "↔",
        "partial" => "∂",
        "nabla" => "∇",
        "sum" => "∑",
        "prod" => "∏",
        "int" => "∫",
        "in" => "∈",
        "subset" => "⊂",
        "cup" => "∪",
        "cap" => "∩",
        "forall" => "∀",
        "exists" => "∃",
        "ell" => "ℓ",
        "hbar" => "ℏ",
        "circ" => "∘",
        "star" => "⋆",
        "ast" => "∗",
        "log" => "log",
        "ln" => "ln",
        "exp" => "exp",
        "sin" => "sin",
        "cos" => "cos",
        "max" => "max",
        "min" => "min",
        "quad" | "qquad" | "space" | "enspace" | "thinspace" => " ",
        "em" | "it" | "bf" | "sc" | "rm" | "sf" | "tt" | "sl" | "itshape" | "bfseries"
        | "scshape" | "normalfont" | "upshape" | "mdseries" | "rmfamily" | "sffamily"
        | "ttfamily" | "relax" | "protect" | "noopsort" | "displaystyle" => "",
        _ => return None,
    })
}

fn blackboard(arg: &str) -> String {
    arg.chars()
        .map(|c| match c {
            'R' => 'ℝ',
            'N' => 'ℕ',
            'Z' => 'ℤ',
            'Q' => 'ℚ',
            'C' => 'ℂ',
            'P' => 'ℙ',
            'E' => '𝔼',
            other => other,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accents() {
        assert_eq!(to_unicode(r#"{\"u}ber"#), "über");
        assert_eq!(to_unicode(r#"M{\"{u}}ller"#), "Müller");
        assert_eq!(to_unicode(r"Erd\H{o}s"), "Erdős");
        assert_eq!(to_unicode(r"Ca\~{n}ada"), "Cañada");
        assert_eq!(to_unicode(r"\v{C}ech"), "Čech");
        assert_eq!(to_unicode(r"Fran\c{c}ois"), "François");
        assert_eq!(to_unicode(r"\'{\i}ndice"), "índice");
        assert_eq!(to_unicode(r#"G{\"o}del"#), "Gödel");
        assert_eq!(to_unicode(r"\'Eric"), "Éric");
        assert_eq!(to_unicode(r"\u{g}"), "ğ");
        assert_eq!(
            to_unicode(r#"{\o}ystein {\ss} \AA{}ngstr\"om"#),
            "øystein ß Ångström"
        );
        assert_eq!(to_unicode(r"Pr\'{e}-print"), "Pré-print");
    }

    #[test]
    fn commands_and_punctuation() {
        assert_eq!(
            to_unicode("{BERT}: Pre-training of {Deep} Bidirectional"),
            "BERT: Pre-training of Deep Bidirectional"
        );
        assert_eq!(
            to_unicode(r"pages 1--10, and---well"),
            "pages 1–10, and—well"
        );
        assert_eq!(
            to_unicode(r"\emph{Attention} is \textbf{all}"),
            "Attention is all"
        );
        assert_eq!(
            to_unicode(r"R\&D at 50\% \textendash{} ok"),
            "R&D at 50% – ok"
        );
        assert_eq!(to_unicode(r"``quoted'' text"), "“quoted” text");
        assert_eq!(
            to_unicode(r"The $\alpha$-$\beta$ model in $\mathbb{R}^n$"),
            "The α-β model in ℝ^n"
        );
        assert_eq!(to_unicode("line\n   break\t here"), "line break here");
        assert_eq!(
            to_unicode(r"\LaTeX\ and \TeX{} rocks"),
            "LaTeX and TeX rocks"
        );
        assert_eq!(
            to_unicode(r"see \url{https://x.org/a_b}"),
            "see https://x.org/a_b"
        );
        assert_eq!(to_unicode(r"\href{https://x.org}{the site}"), "the site");
        assert_eq!(to_unicode(r"Hello~World"), "Hello World");
        assert_eq!(
            to_unicode(r"\unknowncommand{kept} \dropped text"),
            "kept text"
        );
    }

    #[test]
    fn verbatim_fields() {
        assert_eq!(
            verbatim_to_unicode(r"https://x.org/a\_b"),
            "https://x.org/a_b"
        );
        assert_eq!(verbatim_to_unicode("{10.1000/xyz}"), "10.1000/xyz");
    }

    #[test]
    fn escaping() {
        assert_eq!(
            escape("R&D 50% #1 $5 a_b {x}"),
            r"R\&D 50\% \#1 \$5 a\_b \{x\}"
        );
        assert_eq!(
            escape_title("BERT and ImageNet for GPT-3 models"),
            "{BERT} and {ImageNet} for {GPT-3} models"
        );
        assert_eq!(
            escape_title("Attention Is All You Need"),
            "Attention Is All You Need"
        );
        assert_eq!(
            to_unicode(&escape_title("R&D in ImageNet")),
            "R&D in ImageNet"
        );
        assert!(balanced("a {b} \\{"));
        assert!(!balanced("a {b"));
        assert_eq!(escape_verbatim("https://x.org/{a"), "https://x.org/%7Ba");
    }
}
