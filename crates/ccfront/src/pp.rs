//! C89 preprocessor over `lex` tokens: directives, conditional inclusion and
//! macro expansion with hide sets (argument pre-expansion + rescan).

use crate::lex::{lex, Kind, Tok};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;

struct Macro {
    /// `None`: object-like.
    params: Option<Vec<Rc<str>>>,
    body: Vec<Tok>,
}

struct Cond {
    parent: bool,
    active: bool,
    taken: bool,
}

pub struct Pp<'a> {
    macros: HashMap<Rc<str>, Macro>,
    include_dirs: &'a [PathBuf],
    pub files: Vec<String>,
    conds: Vec<Cond>,
}

/// System-header replacements (see the plan's "System headers").
const PREDEFINED: [(&str, &str); 28] = [
    ("NULL", "0"),
    ("EOF", "-1"),
    ("SEEK_SET", "0"),
    ("SEEK_CUR", "1"),
    ("SEEK_END", "2"),
    ("O_RDONLY", "0"),
    ("O_WRONLY", "1"),
    ("O_RDWR", "2"),
    ("O_CREAT", "64"),
    ("O_TRUNC", "512"),
    ("O_BINARY", "0"),
    ("R_OK", "4"),
    ("W_OK", "2"),
    ("X_OK", "1"),
    ("F_OK", "0"),
    ("MAXCHAR", "127"),
    ("MINCHAR", "(-128)"),
    ("MAXSHORT", "32767"),
    ("MINSHORT", "(-32768)"),
    ("MAXINT", "2147483647"),
    ("MININT", "(-2147483647-1)"),
    ("MAXLONG", "2147483647"),
    ("MINLONG", "(-2147483647-1)"),
    ("stdin", "((FILE*)0)"),
    ("stdout", "((FILE*)1)"),
    ("stderr", "((FILE*)2)"),
    ("NORMALUNIX", "1"),
    ("LINUX", "1"),
];

fn err(t: &Tok, files: &[String], msg: impl std::fmt::Display) -> String {
    format!(
        "{}:{}: {}",
        files.get(t.file as usize).map_or("<builtin>", |s| s),
        t.line,
        msg
    )
}

impl<'a> Pp<'a> {
    pub fn new(include_dirs: &'a [PathBuf], defines: &[(&str, &str)]) -> Self {
        let mut pp = Pp {
            macros: HashMap::new(),
            include_dirs,
            files: Vec::new(),
            conds: Vec::new(),
        };
        for (n, v) in PREDEFINED.iter().chain(defines) {
            pp.macros.insert(
                (*n).into(),
                Macro {
                    params: None,
                    body: lex(v.as_bytes(), u32::MAX),
                },
            );
        }
        pp
    }

    fn file_index(&mut self, path: &Path) -> u32 {
        let s = path.to_string_lossy().into_owned();
        match self.files.iter().position(|f| *f == s) {
            Some(i) => i as u32,
            None => {
                self.files.push(s);
                self.files.len() as u32 - 1
            }
        }
    }

    fn load(&mut self, path: &Path) -> Result<Vec<Tok>, String> {
        let src = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let f = self.file_index(path);
        let mut t = lex(&src, f);
        t.reverse();
        Ok(t)
    }

    fn active(&self) -> bool {
        self.conds.last().is_none_or(|c| c.active)
    }

    /// Preprocesses `path`; the result ends with an `Eof` token.
    pub fn run(&mut self, path: &Path) -> Result<Vec<Tok>, String> {
        let mut input = self.load(path)?;
        let mut out = Vec::new();
        while let Some(t) = input.pop() {
            if t.bol && t.is("#") {
                self.directive(&t, &mut input)?;
            } else if self.active() && !(t.kind == Kind::Ident && self.expand(&t, &mut input)?) {
                // (an expanded macro went back into `input` instead)
                out.push(t);
            }
        }
        if !self.conds.is_empty() {
            return Err("unterminated #if".into());
        }
        out.push(Tok {
            kind: Kind::Eof,
            text: "".into(),
            file: 0,
            line: 0,
            bol: true,
            space: true,
            hide: Vec::new(),
        });
        Ok(out)
    }

    fn directive(&mut self, hash: &Tok, input: &mut Vec<Tok>) -> Result<(), String> {
        let mut line = Vec::new();
        while input.last().is_some_and(|t| !t.bol) {
            line.push(input.pop().unwrap());
        }
        let Some(name) = line.first() else {
            return Ok(());
        };
        let e = |m: &str| err(hash, &self.files, m);
        let active = self.active();
        match &*name.text {
            "ifdef" | "ifndef" | "if" => {
                let c = active
                    && match &*name.text {
                        "if" => self.eval(hash, &line[1..])?,
                        n => {
                            let id = line
                                .get(1)
                                .filter(|t| t.kind == Kind::Ident)
                                .ok_or_else(|| e("macro name expected"))?;
                            self.macros.contains_key(&id.text) == (n == "ifdef")
                        }
                    };
                self.conds.push(Cond {
                    parent: active,
                    active: c,
                    taken: c,
                });
            }
            "elif" => {
                let c = self.conds.last().ok_or_else(|| e("#elif without #if"))?;
                let c = c.parent && !c.taken && self.eval(hash, &line[1..])?;
                let top = self.conds.last_mut().unwrap();
                top.active = c;
                top.taken |= c;
            }
            "else" => {
                let top = self
                    .conds
                    .last_mut()
                    .ok_or_else(|| e("#else without #if"))?;
                top.active = top.parent && !top.taken;
                top.taken = true;
            }
            "endif" => {
                self.conds.pop().ok_or_else(|| e("#endif without #if"))?;
            }
            _ if !active => {}
            "define" => {
                let id = line
                    .get(1)
                    .filter(|t| t.kind == Kind::Ident)
                    .ok_or_else(|| e("macro name expected"))?;
                let m = if line.get(2).is_some_and(|t| t.is("(") && !t.space) {
                    let mut params = Vec::new();
                    let mut i = 3;
                    while !line.get(i).is_some_and(|t| t.is(")")) {
                        let p = line
                            .get(i)
                            .filter(|t| t.kind == Kind::Ident)
                            .ok_or_else(|| e("bad macro parameter list"))?;
                        params.push(p.text.clone());
                        i += 1;
                        if line.get(i).is_some_and(|t| t.is(",")) {
                            i += 1;
                        }
                    }
                    Macro {
                        params: Some(params),
                        body: line[i + 1..].to_vec(),
                    }
                } else {
                    Macro {
                        params: None,
                        body: line[2..].to_vec(),
                    }
                };
                self.macros.insert(id.text.clone(), m);
            }
            "undef" => {
                if let Some(id) = line.get(1) {
                    self.macros.remove(&id.text);
                }
            }
            "include" => {
                let arg = line.get(1).ok_or_else(|| e("#include expects a file"))?;
                if arg.is("<") {
                    return Ok(());
                }
                if arg.kind != Kind::Str {
                    return Err(e("#include expects \"file\" or <file>"));
                }
                let name = &arg.text[1..arg.text.len() - 1];
                let here = Path::new(&self.files[hash.file as usize])
                    .parent()
                    .map(Path::to_path_buf)
                    .unwrap_or_default();
                // As in C, an unfound "x" falls back to the system search: skipped like <x>.
                if let Some(path) = std::iter::once(&here)
                    .chain(self.include_dirs)
                    .map(|d| d.join(name))
                    .find(|p| p.is_file())
                {
                    let toks = self.load(&path)?;
                    input.extend(toks);
                }
            }
            "pragma" | "line" | "ident" => {}
            "error" => {
                let msg: Vec<&str> = line[1..].iter().map(|t| &*t.text).collect();
                return Err(e(&format!("#error {}", msg.join(" "))));
            }
            d => return Err(e(&format!("unknown directive #{d}"))),
        }
        Ok(())
    }

    /// `#if` / `#elif` condition.
    fn eval(&self, hash: &Tok, line: &[Tok]) -> Result<bool, String> {
        let num = |v: bool, t: &Tok| Tok {
            kind: Kind::Num,
            text: if v { "1" } else { "0" }.into(),
            ..t.clone()
        };
        let mut toks = Vec::new();
        let mut i = 0;
        while i < line.len() {
            let t = &line[i];
            if t.is("defined") {
                let paren = line.get(i + 1).is_some_and(|t| t.is("("));
                let id = line
                    .get(i + 1 + paren as usize)
                    .filter(|t| t.kind == Kind::Ident);
                let id =
                    id.ok_or_else(|| err(hash, &self.files, "defined expects a macro name"))?;
                toks.push(num(self.macros.contains_key(&id.text), t));
                i += 2 + 2 * paren as usize;
            } else {
                toks.push(t.clone());
                i += 1;
            }
        }
        let mut toks: Vec<Tok> = self
            .expand_all(toks)?
            .into_iter()
            .map(|t| {
                if t.kind == Kind::Ident {
                    num(false, &t)
                } else {
                    t
                }
            })
            .collect();
        toks.push(Tok {
            kind: Kind::Eof,
            text: "".into(),
            ..hash.clone()
        });
        crate::parse::eval_pp(&toks, &self.files).map(|v| v != 0)
    }

    fn expand_all(&self, toks: Vec<Tok>) -> Result<Vec<Tok>, String> {
        let mut st: Vec<Tok> = toks.into_iter().rev().collect();
        let mut out = Vec::new();
        while let Some(t) = st.pop() {
            if !(t.kind == Kind::Ident && self.expand(&t, &mut st)?) {
                out.push(t);
            }
        }
        Ok(out)
    }

    /// If `t` names a macro to expand here, pushes its expansion back onto
    /// `input` (a reversed token stack) for rescanning.
    fn expand(&self, t: &Tok, input: &mut Vec<Tok>) -> Result<bool, String> {
        if t.hide.contains(&t.text) {
            return Ok(false);
        }
        let dynamic = match &*t.text {
            "__LINE__" => Some((Kind::Num, t.line.to_string())),
            "__FILE__" => Some((
                Kind::Str,
                format!("{:?}", self.files.get(t.file as usize).map_or("", |s| s)),
            )),
            _ => None,
        };
        if let Some((kind, text)) = dynamic {
            input.push(Tok {
                kind,
                text: text.into(),
                bol: false,
                ..t.clone()
            });
            return Ok(true);
        }
        let Some(m) = self.macros.get(&t.text) else {
            return Ok(false);
        };
        let (body, mut hide) = match &m.params {
            None => (self.subst(m, &[], t)?, t.hide.clone()),
            Some(params) => {
                if !input.last().is_some_and(|n| n.is("(")) {
                    return Ok(false);
                }
                input.pop();
                let mut args = vec![Vec::new()];
                let mut depth = 0;
                let rparen = loop {
                    let a = input.pop().ok_or_else(|| {
                        err(
                            t,
                            &self.files,
                            format!("unterminated call of macro {}", t.text),
                        )
                    })?;
                    if a.is(")") && depth == 0 {
                        break a;
                    }
                    if a.is(",") && depth == 0 {
                        args.push(Vec::new());
                        continue;
                    }
                    depth += a.is("(") as i32 - a.is(")") as i32;
                    args.last_mut().unwrap().push(a);
                };
                if params.is_empty() && args.len() == 1 && args[0].is_empty() {
                    args.clear();
                }
                if args.len() != params.len() {
                    return Err(err(
                        t,
                        &self.files,
                        format!("macro {} expects {} arguments", t.text, params.len()),
                    ));
                }
                let hide = t
                    .hide
                    .iter()
                    .filter(|h| rparen.hide.contains(h))
                    .cloned()
                    .collect();
                (self.subst(m, &args, t)?, hide)
            }
        };
        hide.push(t.text.clone());
        for (k, mut b) in body.into_iter().enumerate().rev() {
            for h in &hide {
                if !b.hide.contains(h) {
                    b.hide.push(h.clone());
                }
            }
            b.file = t.file;
            b.line = t.line;
            b.bol = false;
            if k == 0 {
                b.space = t.space;
            }
            input.push(b);
        }
        Ok(true)
    }

    fn subst(&self, m: &Macro, args: &[Vec<Tok>], at: &Tok) -> Result<Vec<Tok>, String> {
        let param = |t: &Tok| -> Option<usize> {
            if t.kind != Kind::Ident {
                return None;
            }
            m.params.as_ref()?.iter().position(|p| *p == t.text)
        };
        let body = &m.body;
        let mut out: Vec<Tok> = Vec::new();
        let mut i = 0;
        while i < body.len() {
            let b = &body[i];
            if m.params.is_some() && b.is("#") {
                if let Some(p) = body.get(i + 1).and_then(param) {
                    out.push(stringize(&args[p], b));
                    i += 2;
                    continue;
                }
            }
            if b.is("##") && i + 1 < body.len() {
                // shortcut: an empty left operand pastes onto the previous token; Doom never uses ##.
                let rhs = match param(&body[i + 1]) {
                    Some(p) => args[p].clone(),
                    None => vec![body[i + 1].clone()],
                };
                i += 2;
                let mut rhs = rhs.into_iter();
                match (out.pop(), rhs.next()) {
                    (Some(l), Some(r)) => {
                        let text = format!("{}{}", l.text, r.text);
                        let mut p = lex(text.as_bytes(), l.file);
                        if p.len() != 1 {
                            return Err(err(
                                at,
                                &self.files,
                                format!("pasting does not give a valid token: {text}"),
                            ));
                        }
                        let p = p.pop().unwrap();
                        out.push(Tok {
                            kind: p.kind,
                            text: p.text,
                            ..l
                        });
                    }
                    (l, r) => out.extend(l.into_iter().chain(r)),
                }
                out.extend(rhs);
                continue;
            }
            match param(b) {
                Some(p) if body.get(i + 1).is_some_and(|n| n.is("##")) => {
                    out.extend(args[p].iter().cloned())
                }
                Some(p) => {
                    let mut e = self.expand_all(args[p].clone())?;
                    if let Some(f) = e.first_mut() {
                        f.space = b.space;
                    }
                    out.extend(e);
                }
                None => out.push(b.clone()),
            }
            i += 1;
        }
        Ok(out)
    }
}

fn stringize(arg: &[Tok], at: &Tok) -> Tok {
    let mut s = String::from("\"");
    for (k, t) in arg.iter().enumerate() {
        if k > 0 && t.space {
            s.push(' ');
        }
        if matches!(t.kind, Kind::Str | Kind::Char) {
            for c in t.text.chars() {
                if c == '"' || c == '\\' {
                    s.push('\\');
                }
                s.push(c);
            }
        } else {
            s.push_str(&t.text);
        }
    }
    s.push('"');
    Tok {
        kind: Kind::Str,
        text: s.into(),
        hide: Vec::new(),
        ..at.clone()
    }
}
