//! Preprocessing tokens. Source bytes are read as Latin-1 chars so string
//! literals round-trip byte for byte.

use std::rc::Rc;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Ident,
    /// pp-number, converted by the parser.
    Num,
    /// Char constant, text includes the quotes.
    Char,
    /// String literal, text includes the quotes.
    Str,
    Punct,
    /// Stray character or unterminated literal; an error only if parsed.
    Other,
    Eof,
}

#[derive(Clone, Debug)]
pub struct Tok {
    pub kind: Kind,
    pub text: Rc<str>,
    pub file: u32,
    pub line: u32,
    /// First token of a logical line (directives start with such a `#`).
    pub bol: bool,
    /// Preceded by white space (function-like `#define` detection, `#x`).
    pub space: bool,
    /// Macros this token must not be expanded by (hide set).
    pub hide: Vec<Rc<str>>,
}

impl Tok {
    pub fn is(&self, s: &str) -> bool {
        &*self.text == s && matches!(self.kind, Kind::Punct | Kind::Ident)
    }
}

/// Longest first, so the first match is the maximal munch.
const PUNCTS: [&str; 48] = [
    "...", "<<=", ">>=", "->", "++", "--", "<<", ">>", "<=", ">=", "==", "!=", "&&", "||", "*=",
    "/=", "%=", "+=", "-=", "&=", "^=", "|=", "##", "[", "]", "(", ")", "{", "}", ".", "&", "*",
    "+", "-", "~", "!", "/", "%", "<", ">", "^", "|", "?", ":", ";", "=", ",", "#",
];

pub fn lex(src: &[u8], file: u32) -> Vec<Tok> {
    // Phase 2: splice lines, remembering each char's physical line.
    let mut cs: Vec<(char, u32)> = Vec::with_capacity(src.len());
    let mut line = 1;
    let mut i = 0;
    while i < src.len() {
        let b = src[i];
        if b == b'\\' && src.get(i + 1) == Some(&b'\n') {
            i += 2;
            line += 1;
            continue;
        }
        if b == b'\\' && src.get(i + 1) == Some(&b'\r') && src.get(i + 2) == Some(&b'\n') {
            i += 3;
            line += 1;
            continue;
        }
        cs.push((b as char, line));
        if b == b'\n' {
            line += 1;
        }
        i += 1;
    }
    let c = |i: usize| cs.get(i).map_or('\0', |x| x.0);
    let mut out = Vec::new();
    let (mut bol, mut space) = (true, false);
    let mut i = 0;
    while i < cs.len() {
        let ch = c(i);
        let line = cs[i].1;
        if ch == '\n' {
            bol = true;
            space = true;
            i += 1;
            continue;
        }
        if matches!(ch, ' ' | '\t' | '\r' | '\x0b' | '\x0c') {
            space = true;
            i += 1;
            continue;
        }
        if ch == '/' && c(i + 1) == '*' {
            i += 2;
            while i < cs.len() && !(c(i) == '*' && c(i + 1) == '/') {
                i += 1;
            }
            i += 2;
            space = true;
            continue;
        }
        if ch == '/' && c(i + 1) == '/' {
            while i < cs.len() && c(i) != '\n' {
                i += 1;
            }
            space = true;
            continue;
        }
        let start = i;
        let kind = if ch.is_ascii_alphabetic() || ch == '_' {
            while c(i).is_ascii_alphanumeric() || c(i) == '_' {
                i += 1;
            }
            Kind::Ident
        } else if ch.is_ascii_digit() || (ch == '.' && c(i + 1).is_ascii_digit()) {
            i += 1;
            loop {
                let d = c(i);
                if matches!(d, 'e' | 'E' | 'p' | 'P') && matches!(c(i + 1), '+' | '-') {
                    i += 2;
                } else if d.is_ascii_alphanumeric() || d == '_' || d == '.' {
                    i += 1;
                } else {
                    break;
                }
            }
            Kind::Num
        } else if ch == '\'' || ch == '"' {
            i += 1;
            let mut closed = false;
            while i < cs.len() && c(i) != '\n' {
                let d = c(i);
                i += 1;
                if d == '\\' && i < cs.len() && c(i) != '\n' {
                    i += 1;
                } else if d == ch {
                    closed = true;
                    break;
                }
            }
            if !closed {
                Kind::Other
            } else if ch == '"' {
                Kind::Str
            } else {
                Kind::Char
            }
        } else if let Some(p) = PUNCTS
            .iter()
            .find(|p| p.chars().enumerate().all(|(k, pc)| c(i + k) == pc))
        {
            i += p.len();
            Kind::Punct
        } else {
            i += 1;
            Kind::Other
        };
        let text: String = cs[start..i.min(cs.len())].iter().map(|x| x.0).collect();
        out.push(Tok {
            kind,
            text: text.into(),
            file,
            line,
            bol,
            space,
            hide: Vec::new(),
        });
        bol = false;
        space = false;
    }
    out
}
