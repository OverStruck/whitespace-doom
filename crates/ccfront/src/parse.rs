//! C89 parser: preprocessed tokens -> `ast::Unit`, with struct layouts in
//! cells, typedef/tag scoping and integer constant evaluation.

use crate::lex::{Kind, Tok};
use std::collections::HashMap;
use wsdoom::cc::ast::*;

type R<T> = Result<T, String>;

/// Ordinary-namespace entry.
enum Name {
    Typedef(Type),
    Var(Type),
    Enum(i64),
}

#[derive(Default)]
struct Scope {
    names: HashMap<String, Name>,
    /// `None`: an enum tag.
    tags: HashMap<String, Option<usize>>,
}

const KEYWORDS: [&str; 32] = [
    "auto", "break", "case", "char", "const", "continue", "default", "do", "double", "else",
    "enum", "extern", "float", "for", "goto", "if", "int", "long", "register", "return", "short",
    "signed", "sizeof", "static", "struct", "switch", "typedef", "union", "unsigned", "void",
    "volatile", "while",
];
const TYPE_WORDS: [&str; 19] = [
    "void", "char", "short", "int", "long", "float", "double", "signed", "unsigned", "struct",
    "union", "enum", "const", "volatile", "typedef", "extern", "static", "auto", "register",
];

fn uint() -> Type {
    Type::Int {
        kind: IntKind::Int,
        signed: false,
    }
}

fn is_char(t: &Type) -> bool {
    matches!(
        t,
        Type::Int {
            kind: IntKind::Char,
            ..
        }
    )
}

/// Parameter type adjustment: arrays and functions become pointers.
fn adjust_param(t: Type) -> Type {
    match t {
        Type::Array(el, _) => Type::Ptr(el),
        f @ Type::Func { .. } => Type::Ptr(Box::new(f)),
        t => t,
    }
}

fn bits(k: IntKind) -> u32 {
    match k {
        IntKind::Char => 8,
        IntKind::Short => 16,
        IntKind::Int | IntKind::Long => 32,
        IntKind::LongLong => 64,
    }
}

/// Wraps `v` into the range of the integer type.
fn norm(v: i64, (k, signed): (IntKind, bool)) -> i64 {
    let b = bits(k);
    if b == 64 {
        return v;
    }
    let v = v & ((1i64 << b) - 1);
    if signed && v >> (b - 1) != 0 {
        v - (1i64 << b)
    } else {
        v
    }
}

fn ikind(t: &Type) -> (IntKind, bool) {
    match t {
        Type::Int { kind, signed } => (*kind, *signed),
        Type::Ptr(_) => (IntKind::Long, false),
        _ => (IntKind::Int, true),
    }
}

fn promote((k, s): (IntKind, bool)) -> (IntKind, bool) {
    match k {
        IntKind::Char | IntKind::Short => (IntKind::Int, true),
        _ => (k, s),
    }
}

/// Usual arithmetic conversions with int = long = 32 bits.
fn arith(a: (IntKind, bool), b: (IntKind, bool)) -> (IntKind, bool) {
    let (a, b) = (promote(a), promote(b));
    let has = |k: IntKind| a.0 == k || b.0 == k;
    if has(IntKind::LongLong) {
        (
            IntKind::LongLong,
            !((a.0 == IntKind::LongLong && !a.1) || (b.0 == IntKind::LongLong && !b.1)),
        )
    } else if has(IntKind::Long) {
        (IntKind::Long, a.1 && b.1)
    } else {
        (IntKind::Int, a.1 && b.1)
    }
}

fn to_type((kind, signed): (IntKind, bool)) -> Type {
    Type::Int { kind, signed }
}

/// Decodes the escapes of a char/string literal body.
pub fn unescape(s: &str) -> R<Vec<u8>> {
    let cs: Vec<char> = s.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < cs.len() {
        let c = cs[i];
        i += 1;
        if c != '\\' {
            out.push(c as u32 as u8);
            continue;
        }
        let e = *cs.get(i).ok_or("trailing backslash in literal")?;
        i += 1;
        out.push(match e {
            'n' => b'\n',
            't' => b'\t',
            'v' => 0x0b,
            'b' => 0x08,
            'r' => b'\r',
            'f' => 0x0c,
            'a' => 0x07,
            '0'..='7' => {
                let mut v = e as u32 - '0' as u32;
                for _ in 0..2 {
                    match cs.get(i) {
                        Some(d @ '0'..='7') => {
                            v = v * 8 + (*d as u32 - '0' as u32);
                            i += 1;
                        }
                        _ => break,
                    }
                }
                v as u8
            }
            'x' => {
                let mut v = 0u32;
                let start = i;
                while let Some(d) = cs.get(i).and_then(|d| d.to_digit(16)) {
                    v = v.wrapping_mul(16).wrapping_add(d);
                    i += 1;
                }
                if i == start {
                    return Err("\\x without hex digits".into());
                }
                v as u8
            }
            other => other as u32 as u8,
        });
    }
    Ok(out)
}

/// Integer or floating constant from a pp-number, typed by C89 rules with
/// int = long = 32 bits and long long = 64.
pub fn parse_num(s: &str) -> R<Expr> {
    let lower = s.to_ascii_lowercase();
    let hex = lower.starts_with("0x");
    if !hex && (lower.contains('.') || lower.contains('e')) {
        let f = lower.trim_end_matches(['f', 'l']);
        return f
            .parse::<f64>()
            .map(Expr::Float)
            .map_err(|_| format!("bad float constant `{s}`"));
    }
    let digits = lower.trim_end_matches(['u', 'l']);
    let suffix = &lower[digits.len()..];
    let (radix, ds) = if hex {
        (16, &digits[2..])
    } else if digits.len() > 1 && digits.starts_with('0') {
        (8, &digits[1..])
    } else {
        (10, digits)
    };
    let v = u64::from_str_radix(ds, radix).map_err(|_| format!("bad integer constant `{s}`"))?;
    let u = suffix.contains('u');
    let l = suffix.matches('l').count();
    use IntKind::*;
    let cands: &[(IntKind, bool)] = match (u, l) {
        (false, 0) if radix == 10 => &[
            (Int, true),
            (Long, false),
            (LongLong, true),
            (LongLong, false),
        ],
        (false, 0) => &[
            (Int, true),
            (Int, false),
            (LongLong, true),
            (LongLong, false),
        ],
        (true, 0) => &[(Int, false), (LongLong, false)],
        (false, 1) => &[
            (Long, true),
            (Long, false),
            (LongLong, true),
            (LongLong, false),
        ],
        (true, 1) => &[(Long, false), (LongLong, false)],
        (false, _) => &[(LongLong, true), (LongLong, false)],
        (true, _) => &[(LongLong, false)],
    };
    let max = |(k, s): (IntKind, bool)| {
        if s {
            (1u64 << (bits(k) - 1)) - 1
        } else {
            u64::MAX >> (64 - bits(k))
        }
    };
    let t = cands
        .iter()
        .copied()
        .find(|&c| v <= max(c))
        .unwrap_or((LongLong, false));
    Ok(Expr::Int(v as i64, to_type(t)))
}

pub struct Parser<'a> {
    t: &'a [Tok],
    p: usize,
    files: &'a [String],
    scopes: Vec<Scope>,
    unit: Unit,
}

/// Value of a `#if` expression (identifiers already replaced by 0).
pub fn eval_pp(toks: &[Tok], files: &[String]) -> R<i64> {
    let mut p = Parser::new(toks, files);
    let e = p.expr()?;
    if p.peek().kind != Kind::Eof {
        return Err(p.err("garbage after #if expression"));
    }
    p.eval(&e).map(|v| v.0).map_err(|m| p.err(&m))
}

/// Parses a preprocessed token stream ending with `Eof`.
pub fn parse(toks: &[Tok], files: &[String]) -> R<Unit> {
    let mut p = Parser::new(toks, files);
    p.unit.structs.push(StructDef {
        tag: Some("FILE".into()),
        is_union: false,
        complete: false,
        fields: Vec::new(),
        size: 0,
    });
    let char_ptr = Type::Ptr(Box::new(Type::Int {
        kind: IntKind::Char,
        signed: true,
    }));
    for (n, t) in [
        ("size_t", uint()),
        ("ssize_t", Type::int()),
        ("off_t", Type::int()),
        ("time_t", Type::int()),
        ("FILE", Type::Struct(0)),
        ("va_list", char_ptr),
    ] {
        p.unit.typedefs.push((n.into(), t.clone()));
        p.declare(n.into(), Name::Typedef(t));
    }
    while p.peek().kind != Kind::Eof {
        if p.accept(";") {
            continue;
        }
        let vars = p.declaration(true)?;
        p.unit.decls.extend(vars.into_iter().map(Decl::Var));
    }
    p.unit.files = files.to_vec();
    Ok(p.unit)
}

struct Specs {
    ty: Type,
    storage: Storage,
}

impl<'a> Parser<'a> {
    fn new(t: &'a [Tok], files: &'a [String]) -> Self {
        Parser {
            t,
            p: 0,
            files,
            scopes: vec![Scope::default()],
            unit: Unit::default(),
        }
    }

    // ---- token helpers ----

    fn peek(&self) -> &'a Tok {
        self.peek_at(0)
    }
    fn peek_at(&self, k: usize) -> &'a Tok {
        let t: &'a [Tok] = self.t;
        &t[(self.p + k).min(t.len() - 1)]
    }
    fn next(&mut self) -> &'a Tok {
        let t = self.peek();
        if self.p < self.t.len() - 1 {
            self.p += 1;
        }
        t
    }
    fn is(&self, s: &str) -> bool {
        self.peek().is(s)
    }
    fn accept(&mut self, s: &str) -> bool {
        let ok = self.is(s);
        if ok {
            self.next();
        }
        ok
    }
    fn expect(&mut self, s: &str) -> R<()> {
        if self.accept(s) {
            Ok(())
        } else {
            Err(self.err(&format!("expected `{s}`, found `{}`", self.peek().text)))
        }
    }
    fn err_at(&self, t: &Tok, msg: &str) -> String {
        format!(
            "{}:{}: {}",
            self.files.get(t.file as usize).map_or("<builtin>", |s| s),
            t.line,
            msg
        )
    }
    fn err(&self, msg: &str) -> String {
        self.err_at(self.peek(), msg)
    }
    fn pos(&self) -> Pos {
        let t = self.peek();
        Pos {
            file: t.file,
            line: t.line,
        }
    }
    fn is_name(t: &Tok) -> bool {
        t.kind == Kind::Ident && !KEYWORDS.contains(&&*t.text)
    }
    fn ident(&mut self) -> R<String> {
        if Self::is_name(self.peek()) {
            Ok(self.next().text.to_string())
        } else {
            Err(self.err(&format!(
                "expected identifier, found `{}`",
                self.peek().text
            )))
        }
    }

    // ---- scopes ----

    fn lookup(&self, n: &str) -> Option<&Name> {
        self.scopes.iter().rev().find_map(|s| s.names.get(n))
    }
    fn declare(&mut self, n: String, v: Name) {
        self.scopes.last_mut().unwrap().names.insert(n, v);
    }
    fn is_typedef(&self, t: &Tok) -> bool {
        t.kind == Kind::Ident && matches!(self.lookup(&t.text), Some(Name::Typedef(_)))
    }
    fn is_type_start(&self, t: &Tok) -> bool {
        t.kind == Kind::Ident && (TYPE_WORDS.contains(&&*t.text) || self.is_typedef(t))
    }

    // ---- declarations ----

    fn specs(&mut self) -> R<Specs> {
        let mut storage = Storage::None;
        let mut base: Option<Type> = None;
        let mut cnt: HashMap<&str, u32> = HashMap::new();
        loop {
            let t = self.peek();
            if t.kind != Kind::Ident {
                break;
            }
            match &*t.text {
                "typedef" => storage = Storage::Typedef,
                "extern" => storage = Storage::Extern,
                "static" => storage = Storage::Static,
                "auto" | "register" | "const" | "volatile" => {}
                w @ ("void" | "char" | "short" | "int" | "long" | "float" | "double" | "signed"
                | "unsigned") => *cnt.entry(w).or_default() += 1,
                "struct" | "union" => {
                    base = Some(self.struct_spec()?);
                    continue;
                }
                "enum" => {
                    base = Some(self.enum_spec()?);
                    continue;
                }
                _ if base.is_none() && cnt.is_empty() && self.is_typedef(t) => {
                    let Some(Name::Typedef(ty)) = self.lookup(&t.text) else {
                        unreachable!()
                    };
                    base = Some(ty.clone());
                }
                _ => break,
            }
            self.next();
        }
        let c = |w: &str| cnt.get(w).copied().unwrap_or(0);
        let ty = if let Some(b) = base {
            b
        } else if c("void") > 0 {
            Type::Void
        } else if c("float") + c("double") > 0 {
            Type::Float
        } else {
            let kind = match (c("char"), c("short"), c("long")) {
                (1.., _, _) => IntKind::Char,
                (_, 1.., _) => IntKind::Short,
                (_, _, 2..) => IntKind::LongLong,
                (_, _, 1) => IntKind::Long,
                _ => IntKind::Int,
            };
            // Implicit int when no specifier is given.
            Type::Int {
                kind,
                signed: c("unsigned") == 0,
            }
        };
        Ok(Specs { ty, storage })
    }

    fn new_struct(&mut self, tag: Option<String>, is_union: bool) -> usize {
        self.unit.structs.push(StructDef {
            tag: tag.clone(),
            is_union,
            complete: false,
            fields: Vec::new(),
            size: 0,
        });
        let i = self.unit.structs.len() - 1;
        if let Some(t) = tag {
            self.scopes.last_mut().unwrap().tags.insert(t, Some(i));
        }
        i
    }

    fn struct_spec(&mut self) -> R<Type> {
        let is_union = self.next().is("union");
        let tag = if Self::is_name(self.peek()) {
            Some(self.next().text.to_string())
        } else {
            None
        };
        let in_scope = tag
            .as_ref()
            .and_then(|t| self.scopes.last().unwrap().tags.get(t).copied().flatten());
        if !self.accept("{") {
            let tag = tag.ok_or_else(|| self.err("expected struct tag or body"))?;
            // `struct S;` declares S in this scope; other uses refer to any visible S.
            let visible = if self.is(";") {
                in_scope
            } else {
                self.scopes
                    .iter()
                    .rev()
                    .find_map(|s| s.tags.get(&tag).copied().flatten())
            };
            return Ok(Type::Struct(
                visible.unwrap_or_else(|| self.new_struct(Some(tag), is_union)),
            ));
        }
        let idx = match in_scope {
            Some(i) if !self.unit.structs[i].complete => i,
            _ => self.new_struct(tag.clone(), is_union),
        };
        let mut fields = Vec::new();
        let mut size = 0;
        while !self.accept("}") {
            let s = self.specs()?;
            if self.accept(";") {
                continue;
            }
            loop {
                let (name, ty, _) = self.declarator(s.ty.clone())?;
                // Bit-field widths are ignored: every scalar is one cell anyway.
                if self.accept(":") {
                    self.const_expr()?;
                }
                if let Some((name, _)) = name {
                    let sz = self.size_of(&ty).map_err(|m| self.err(&m))?;
                    fields.push(Field {
                        name,
                        ty,
                        offset: if is_union { 0 } else { size },
                    });
                    size = if is_union { size.max(sz) } else { size + sz };
                }
                if !self.accept(",") {
                    break;
                }
            }
            self.expect(";")?;
        }
        self.unit.structs[idx] = StructDef {
            tag,
            is_union,
            complete: true,
            fields,
            size,
        };
        Ok(Type::Struct(idx))
    }

    fn enum_spec(&mut self) -> R<Type> {
        self.next();
        let tag = if Self::is_name(self.peek()) {
            Some(self.next().text.to_string())
        } else {
            None
        };
        if self.accept("{") {
            let mut v = 0i64;
            while !self.accept("}") {
                let n = self.ident()?;
                if self.accept("=") {
                    v = self.const_expr()?;
                }
                self.declare(n, Name::Enum(v));
                v += 1;
                if !self.accept(",") {
                    self.expect("}")?;
                    break;
                }
            }
        }
        if let Some(t) = tag {
            self.scopes.last_mut().unwrap().tags.insert(t, None);
        }
        Ok(Type::int())
    }

    /// Parses a (possibly abstract) declarator over `base`. Returns the name,
    /// the type, and the parameter names of the function suffix closest to
    /// the name (for function definitions).
    #[allow(clippy::type_complexity)]
    fn declarator(&mut self, base: Type) -> R<(Option<(String, Pos)>, Type, Option<Vec<String>>)> {
        let mut ty = base;
        while self.accept("*") {
            while self.accept("const") || self.accept("volatile") {}
            ty = Type::Ptr(Box::new(ty));
        }
        if self.is("(") && self.nested_follows() {
            // `( inner ) suffixes`: the suffixes apply first, then the inner declarator.
            let start = self.p;
            self.skip_parens()?;
            let (ty, outer) = self.suffixes(ty)?;
            let end = self.p;
            self.p = start + 1;
            let (name, ty, inner) = self.declarator(ty)?;
            self.expect(")")?;
            self.p = end;
            return Ok((name, ty, inner.or(outer)));
        }
        let name = if Self::is_name(self.peek()) {
            let t = self.next();
            Some((
                t.text.to_string(),
                Pos {
                    file: t.file,
                    line: t.line,
                },
            ))
        } else {
            None
        };
        let (ty, names) = self.suffixes(ty)?;
        Ok((name, ty, names))
    }

    /// At `(`: does a nested declarator follow (rather than a parameter list)?
    fn nested_follows(&self) -> bool {
        let t = self.peek_at(1);
        t.is("*") || t.is("(") || t.is("[") || (Self::is_name(t) && !self.is_type_start(t))
    }

    fn skip_parens(&mut self) -> R<()> {
        let mut depth = 0;
        loop {
            let t = self.next();
            match t.kind {
                Kind::Eof => return Err(self.err_at(t, "unbalanced parentheses")),
                _ if t.is("(") => depth += 1,
                _ if t.is(")") => {
                    depth -= 1;
                    if depth == 0 {
                        return Ok(());
                    }
                }
                _ => {}
            }
        }
    }

    fn suffixes(&mut self, ty: Type) -> R<(Type, Option<Vec<String>>)> {
        if self.accept("[") {
            let n = if self.is("]") {
                None
            } else {
                Some(self.array_len()?)
            };
            self.expect("]")?;
            let (el, names) = self.suffixes(ty)?;
            return Ok((Type::Array(Box::new(el), n), names));
        }
        if self.accept("(") {
            let (params, variadic, names) = self.params()?;
            let (ret, _) = self.suffixes(ty)?;
            return Ok((
                Type::Func {
                    ret: Box::new(ret),
                    params,
                    variadic,
                },
                Some(names),
            ));
        }
        Ok((ty, None))
    }

    /// After `(`: a prototype list, `()`, or a K&R identifier list
    /// (returned as `None` types with the names).
    fn params(&mut self) -> R<(Option<Vec<Type>>, bool, Vec<String>)> {
        if self.accept(")") {
            return Ok((None, false, Vec::new()));
        }
        if self.is("void") && self.peek_at(1).is(")") {
            self.p += 2;
            return Ok((Some(Vec::new()), false, Vec::new()));
        }
        if Self::is_name(self.peek()) && !self.is_type_start(self.peek()) {
            let mut names = Vec::new();
            loop {
                names.push(self.ident()?);
                if !self.accept(",") {
                    break;
                }
            }
            self.expect(")")?;
            return Ok((None, false, names));
        }
        self.scopes.push(Scope::default());
        let r = self.param_list();
        self.scopes.pop();
        r
    }

    fn param_list(&mut self) -> R<(Option<Vec<Type>>, bool, Vec<String>)> {
        let (mut tys, mut names, mut variadic) = (Vec::new(), Vec::new(), false);
        loop {
            if self.accept("...") {
                variadic = true;
                break;
            }
            let s = self.specs()?;
            let (name, ty, _) = self.declarator(s.ty)?;
            let ty = adjust_param(ty);
            let name = name.map(|n| n.0).unwrap_or_default();
            if !name.is_empty() {
                self.declare(name.clone(), Name::Var(ty.clone()));
            }
            tys.push(ty);
            names.push(name);
            if !self.accept(",") {
                break;
            }
        }
        self.expect(")")?;
        Ok((Some(tys), variadic, names))
    }

    /// One declaration. At file scope a function definition is pushed to
    /// the unit directly; the other declared objects are returned.
    fn declaration(&mut self, global: bool) -> R<Vec<VarDecl>> {
        let s = self.specs()?;
        let mut out = Vec::new();
        if self.accept(";") {
            return Ok(out);
        }
        loop {
            let (name, ty, names) = self.declarator(s.ty.clone())?;
            let (name, pos) = name.ok_or_else(|| self.err("expected declarator name"))?;
            let knr = matches!(ty, Type::Func { params: None, .. })
                && names.as_ref().is_some_and(|n| !n.is_empty());
            if s.storage == Storage::Typedef {
                if global {
                    self.unit.typedefs.push((name.clone(), ty.clone()));
                }
                self.declare(name, Name::Typedef(ty));
            } else if global && matches!(ty, Type::Func { .. }) && (self.is("{") || knr) {
                self.func_def(name, pos, ty, s.storage, names.unwrap_or_default())?;
                return Ok(out);
            } else {
                let init = if self.accept("=") {
                    Some(self.initializer()?)
                } else {
                    None
                };
                let ty = match (ty, &init) {
                    (Type::Array(el, None), Some(i)) => {
                        let n = self.init_len(&el, i);
                        Type::Array(el, Some(n))
                    }
                    (t, _) => t,
                };
                self.declare(name.clone(), Name::Var(ty.clone()));
                out.push(VarDecl {
                    name,
                    ty,
                    storage: s.storage,
                    init,
                    pos,
                });
            }
            if !self.accept(",") {
                break;
            }
        }
        self.expect(";")?;
        Ok(out)
    }

    fn func_def(
        &mut self,
        name: String,
        pos: Pos,
        ty: Type,
        storage: Storage,
        names: Vec<String>,
    ) -> R<()> {
        let Type::Func {
            ret,
            params,
            variadic,
        } = ty
        else {
            unreachable!()
        };
        let params = match params {
            Some(p) => p,
            None => {
                // K&R (or empty `()`) definition: undeclared parameters are int.
                let mut decl: HashMap<String, Type> = HashMap::new();
                while !self.is("{") {
                    let s = self.specs()?;
                    loop {
                        let (n, t, _) = self.declarator(s.ty.clone())?;
                        if let Some((n, _)) = n {
                            decl.insert(n, adjust_param(t));
                        }
                        if !self.accept(",") {
                            break;
                        }
                    }
                    self.expect(";")?;
                }
                names
                    .iter()
                    .map(|n| decl.remove(n).unwrap_or_else(Type::int))
                    .collect()
            }
        };
        let ty = Type::Func {
            ret,
            params: Some(params.clone()),
            variadic,
        };
        self.declare(name.clone(), Name::Var(ty.clone()));
        self.scopes.push(Scope::default());
        for (n, t) in names.iter().zip(&params) {
            if !n.is_empty() {
                self.declare(n.clone(), Name::Var(t.clone()));
            }
        }
        let body = self.compound()?;
        self.scopes.pop();
        self.unit.decls.push(Decl::Func(FuncDef {
            name,
            ty,
            storage,
            params: names,
            body,
            pos,
        }));
        Ok(())
    }

    fn initializer(&mut self) -> R<Init> {
        if !self.accept("{") {
            return Ok(Init::Expr(self.assign()?));
        }
        let mut items = Vec::new();
        while !self.accept("}") {
            items.push(self.initializer()?);
            if !self.accept(",") {
                self.expect("}")?;
                break;
            }
        }
        Ok(Init::List(items))
    }

    /// Element count of `T a[] = init` (with brace elision).
    fn init_len(&self, el: &Type, init: &Init) -> usize {
        match init {
            Init::Expr(Expr::Str(s)) if is_char(el) => s.len() + 1,
            Init::List(items) => match &items[..] {
                [Init::Expr(Expr::Str(s))] if is_char(el) => s.len() + 1,
                _ => {
                    let (mut i, mut n) = (0, 0);
                    while i < items.len() {
                        i = self.consume(items, i, el).max(i + 1);
                        n += 1;
                    }
                    n
                }
            },
            Init::Expr(_) => 1,
        }
    }

    /// Index after the initializers of one object of type `ty` starting at `items[i]`.
    fn consume(&self, items: &[Init], i: usize, ty: &Type) -> usize {
        match (&items[i], ty) {
            (Init::List(_), _) => return i + 1,
            (Init::Expr(Expr::Str(_)), Type::Array(el, _)) if is_char(el) => return i + 1,
            _ => {}
        }
        let mut i = i;
        match ty {
            Type::Array(el, n) => {
                for _ in 0..n.unwrap_or(0) {
                    if i >= items.len() {
                        break;
                    }
                    i = self.consume(items, i, el);
                }
                i
            }
            Type::Struct(s) => {
                let sd = &self.unit.structs[*s];
                let n = if sd.is_union {
                    sd.fields.len().min(1)
                } else {
                    sd.fields.len()
                };
                for f in &sd.fields[..n] {
                    if i >= items.len() {
                        break;
                    }
                    i = self.consume(items, i, &f.ty);
                }
                i
            }
            _ => i + 1,
        }
    }

    // ---- statements ----

    fn compound(&mut self) -> R<Stmt> {
        let pos = self.pos();
        self.expect("{")?;
        self.scopes.push(Scope::default());
        let mut v = Vec::new();
        while !self.accept("}") {
            if self.peek().kind == Kind::Eof {
                return Err(self.err("unexpected end of file in block"));
            }
            if self.is_type_start(self.peek()) && !self.peek_at(1).is(":") {
                let pos = self.pos();
                let d = self.declaration(false)?;
                v.push(Stmt {
                    kind: if d.is_empty() {
                        StmtKind::Empty
                    } else {
                        StmtKind::Decl(d)
                    },
                    pos,
                });
            } else {
                v.push(self.stmt()?);
            }
        }
        self.scopes.pop();
        Ok(Stmt {
            kind: StmtKind::Block(v),
            pos,
        })
    }

    fn paren_expr(&mut self) -> R<Expr> {
        self.expect("(")?;
        let e = self.expr()?;
        self.expect(")")?;
        Ok(e)
    }

    fn stmt(&mut self) -> R<Stmt> {
        let pos = self.pos();
        let b = |s: Stmt| Box::new(s);
        let kind = if self.is("{") {
            return self.compound();
        } else if self.accept(";") {
            StmtKind::Empty
        } else if self.accept("if") {
            let c = self.paren_expr()?;
            let t = self.stmt()?;
            let e = if self.accept("else") {
                Some(b(self.stmt()?))
            } else {
                None
            };
            StmtKind::If(c, b(t), e)
        } else if self.accept("while") {
            let c = self.paren_expr()?;
            StmtKind::While(c, b(self.stmt()?))
        } else if self.accept("do") {
            let body = self.stmt()?;
            self.expect("while")?;
            let c = self.paren_expr()?;
            self.expect(";")?;
            StmtKind::DoWhile(b(body), c)
        } else if self.accept("for") {
            self.expect("(")?;
            let ipos = self.pos();
            let init = if self.accept(";") {
                None
            } else if self.is_type_start(self.peek()) {
                Some(b(Stmt {
                    kind: StmtKind::Decl(self.declaration(false)?),
                    pos: ipos,
                }))
            } else {
                let e = self.expr()?;
                self.expect(";")?;
                Some(b(Stmt {
                    kind: StmtKind::Expr(e),
                    pos: ipos,
                }))
            };
            let cond = if self.is(";") {
                None
            } else {
                Some(self.expr()?)
            };
            self.expect(";")?;
            let step = if self.is(")") {
                None
            } else {
                Some(self.expr()?)
            };
            self.expect(")")?;
            StmtKind::For(init, cond, step, b(self.stmt()?))
        } else if self.accept("switch") {
            let c = self.paren_expr()?;
            StmtKind::Switch(c, b(self.stmt()?))
        } else if self.accept("case") {
            let v = self.const_expr()?;
            self.expect(":")?;
            StmtKind::Case(v, b(self.stmt()?))
        } else if self.accept("default") {
            self.expect(":")?;
            StmtKind::Default(b(self.stmt()?))
        } else if self.accept("break") {
            self.expect(";")?;
            StmtKind::Break
        } else if self.accept("continue") {
            self.expect(";")?;
            StmtKind::Continue
        } else if self.accept("return") {
            let e = if self.is(";") {
                None
            } else {
                Some(self.expr()?)
            };
            self.expect(";")?;
            StmtKind::Return(e)
        } else if self.accept("goto") {
            let n = self.ident()?;
            self.expect(";")?;
            StmtKind::Goto(n)
        } else if Self::is_name(self.peek()) && self.peek_at(1).is(":") {
            let n = self.ident()?;
            self.next();
            StmtKind::Label(n, b(self.stmt()?))
        } else {
            let e = self.expr()?;
            self.expect(";")?;
            StmtKind::Expr(e)
        };
        Ok(Stmt { kind, pos })
    }

    // ---- expressions ----

    fn expr(&mut self) -> R<Expr> {
        let mut e = self.assign()?;
        while self.accept(",") {
            e = Expr::Binary(BinOp::Comma, Box::new(e), Box::new(self.assign()?));
        }
        Ok(e)
    }

    fn assign(&mut self) -> R<Expr> {
        let lhs = self.cond()?;
        let t = self.peek();
        if t.kind != Kind::Punct {
            return Ok(lhs);
        }
        use BinOp::*;
        let op = match &*t.text {
            "=" => None,
            "+=" => Some(Add),
            "-=" => Some(Sub),
            "*=" => Some(Mul),
            "/=" => Some(Div),
            "%=" => Some(Mod),
            "<<=" => Some(Shl),
            ">>=" => Some(Shr),
            "&=" => Some(BitAnd),
            "|=" => Some(BitOr),
            "^=" => Some(BitXor),
            _ => return Ok(lhs),
        };
        self.next();
        Ok(Expr::Assign(op, Box::new(lhs), Box::new(self.assign()?)))
    }

    fn cond(&mut self) -> R<Expr> {
        let c = self.binary(1)?;
        if !self.accept("?") {
            return Ok(c);
        }
        let a = self.expr()?;
        self.expect(":")?;
        let b = self.cond()?;
        Ok(Expr::Cond(Box::new(c), Box::new(a), Box::new(b)))
    }

    fn binary(&mut self, min: u8) -> R<Expr> {
        use BinOp::*;
        let mut lhs = self.unary()?;
        loop {
            let t = self.peek();
            if t.kind != Kind::Punct {
                break;
            }
            let (op, prec) = match &*t.text {
                "||" => (Or, 1),
                "&&" => (And, 2),
                "|" => (BitOr, 3),
                "^" => (BitXor, 4),
                "&" => (BitAnd, 5),
                "==" => (Eq, 6),
                "!=" => (Ne, 6),
                "<" => (Lt, 7),
                ">" => (Gt, 7),
                "<=" => (Le, 7),
                ">=" => (Ge, 7),
                "<<" => (Shl, 8),
                ">>" => (Shr, 8),
                "+" => (Add, 9),
                "-" => (Sub, 9),
                "*" => (Mul, 10),
                "/" => (Div, 10),
                "%" => (Mod, 10),
                _ => break,
            };
            if prec < min {
                break;
            }
            self.next();
            let rhs = self.binary(prec + 1)?;
            lhs = Expr::Binary(op, Box::new(lhs), Box::new(rhs));
        }
        Ok(lhs)
    }

    fn type_name(&mut self) -> R<Type> {
        let s = self.specs()?;
        let (name, ty, _) = self.declarator(s.ty)?;
        if name.is_some() {
            return Err(self.err("unexpected name in type name"));
        }
        Ok(ty)
    }

    fn unary(&mut self) -> R<Expr> {
        use UnOp::*;
        let t = self.peek();
        if t.kind == Kind::Punct {
            let op = match &*t.text {
                "++" => Some(PreInc),
                "--" => Some(PreDec),
                "&" => Some(Addr),
                "*" => Some(Deref),
                "+" => Some(Plus),
                "-" => Some(Neg),
                "~" => Some(BitNot),
                "!" => Some(Not),
                _ => None,
            };
            if let Some(op) = op {
                self.next();
                return Ok(Expr::Unary(op, Box::new(self.unary()?)));
            }
            if t.is("(") && self.is_type_start(self.peek_at(1)) {
                self.next();
                let ty = self.type_name()?;
                self.expect(")")?;
                return Ok(Expr::Cast(ty, Box::new(self.unary()?)));
            }
        }
        if self.accept("sizeof") {
            if self.is("(") && self.is_type_start(self.peek_at(1)) {
                self.next();
                let ty = self.type_name()?;
                self.expect(")")?;
                return Ok(Expr::SizeofType(ty));
            }
            return Ok(Expr::SizeofExpr(Box::new(self.unary()?)));
        }
        self.postfix()
    }

    fn postfix(&mut self) -> R<Expr> {
        let mut e = self.primary()?;
        loop {
            e = if self.accept("[") {
                let i = self.expr()?;
                self.expect("]")?;
                Expr::Index(Box::new(e), Box::new(i))
            } else if self.accept("(") {
                let mut args = Vec::new();
                if !self.accept(")") {
                    loop {
                        args.push(self.assign()?);
                        if !self.accept(",") {
                            break;
                        }
                    }
                    self.expect(")")?;
                }
                Expr::Call(Box::new(e), args)
            } else if self.accept(".") {
                Expr::Member(Box::new(e), self.ident()?)
            } else if self.accept("->") {
                Expr::Arrow(Box::new(e), self.ident()?)
            } else if self.accept("++") {
                Expr::Unary(UnOp::PostInc, Box::new(e))
            } else if self.accept("--") {
                Expr::Unary(UnOp::PostDec, Box::new(e))
            } else {
                return Ok(e);
            };
        }
    }

    fn primary(&mut self) -> R<Expr> {
        let t = self.next();
        fn lit(t: &Tok) -> &str {
            &t.text[1..t.text.len() - 1]
        }
        match t.kind {
            Kind::Ident if Self::is_name(t) => Ok(match self.lookup(&t.text) {
                Some(Name::Enum(v)) => Expr::Int(*v, Type::int()),
                _ => Expr::Ident(t.text.to_string()),
            }),
            Kind::Num => parse_num(&t.text).map_err(|m| self.err_at(t, &m)),
            Kind::Char => {
                let b = unescape(lit(t)).map_err(|m| self.err_at(t, &m))?;
                if b.is_empty() {
                    return Err(self.err_at(t, "empty character constant"));
                }
                // Plain char is signed: '\377' == -1.
                let v = if b.len() == 1 {
                    b[0] as i8 as i64
                } else {
                    b.iter()
                        .fold(0i64, |v, &c| norm(v << 8 | c as i64, (IntKind::Int, true)))
                };
                Ok(Expr::Int(v, Type::int()))
            }
            Kind::Str => {
                let mut s = unescape(lit(t)).map_err(|m| self.err_at(t, &m))?;
                while self.peek().kind == Kind::Str {
                    let t = self.next();
                    s.extend(unescape(lit(t)).map_err(|m| self.err_at(t, &m))?);
                }
                Ok(Expr::Str(s))
            }
            _ if t.is("(") => {
                let e = self.expr()?;
                self.expect(")")?;
                Ok(e)
            }
            _ => Err(self.err_at(t, &format!("expected expression, found `{}`", t.text))),
        }
    }

    // ---- constants and layout ----

    fn const_expr(&mut self) -> R<i64> {
        let e = self.cond()?;
        self.eval(&e).map(|v| v.0).map_err(|m| self.err(&m))
    }

    fn array_len(&mut self) -> R<usize> {
        let n = self.const_expr()?;
        usize::try_from(n).map_err(|_| self.err(&format!("negative array size {n}")))
    }

    fn size_of(&self, t: &Type) -> Result<usize, String> {
        Ok(match t {
            Type::Int { .. } | Type::Float | Type::Ptr(_) => 1,
            Type::Array(el, Some(n)) => n * self.size_of(el)?,
            Type::Array(_, None) => return Err("sizeof incomplete array".into()),
            Type::Struct(i) if self.unit.structs[*i].complete => self.unit.structs[*i].size,
            Type::Struct(_) => return Err("sizeof incomplete struct".into()),
            Type::Void | Type::Func { .. } => return Err("sizeof void or function".into()),
        })
    }

    fn field(&self, t: Type, f: &str) -> Result<Type, String> {
        match t {
            Type::Struct(i) => self.unit.structs[i]
                .fields
                .iter()
                .find(|x| x.name == f)
                .map(|x| x.ty.clone()),
            _ => None,
        }
        .ok_or_else(|| format!("no field `{f}`"))
    }

    /// Type of an expression, as far as `sizeof` needs it: every scalar is
    /// one cell, so only arrays and structs must come out exactly.
    fn type_of(&self, e: &Expr) -> Result<Type, String> {
        let pointee = |t: Type| match t {
            Type::Ptr(t) | Type::Array(t, _) => Ok(*t),
            _ => Err("dereference of non-pointer".to_string()),
        };
        Ok(match e {
            Expr::Ident(n) => match self.lookup(n) {
                Some(Name::Var(t)) => t.clone(),
                _ => return Err(format!("unknown identifier `{n}`")),
            },
            Expr::Str(s) => Type::Array(
                Box::new(Type::Int {
                    kind: IntKind::Char,
                    signed: true,
                }),
                Some(s.len() + 1),
            ),
            Expr::Index(a, _) | Expr::Unary(UnOp::Deref, a) => pointee(self.type_of(a)?)?,
            Expr::Member(a, f) => self.field(self.type_of(a)?, f)?,
            Expr::Arrow(a, f) => self.field(pointee(self.type_of(a)?)?, f)?,
            Expr::Cast(t, _) => t.clone(),
            Expr::Call(f, _) => match self.type_of(f) {
                Ok(Type::Func { ret, .. }) => *ret,
                Ok(Type::Ptr(t)) => match *t {
                    Type::Func { ret, .. } => *ret,
                    _ => Type::int(),
                },
                _ => Type::int(),
            },
            Expr::Assign(_, a, _) | Expr::Cond(_, a, _) => self.type_of(a)?,
            Expr::Binary(BinOp::Comma, _, b) => self.type_of(b)?,
            _ => Type::int(),
        })
    }

    /// Integer constant expression value with its C type.
    fn eval(&self, e: &Expr) -> Result<(i64, Type), String> {
        use BinOp::*;
        let int = Type::int();
        Ok(match e {
            Expr::Int(v, t) => (*v, t.clone()),
            Expr::Unary(op, a) => {
                let (v, t) = self.eval(a)?;
                let k = promote(ikind(&t));
                let r = match op {
                    UnOp::Neg => v.wrapping_neg(),
                    UnOp::Plus => v,
                    UnOp::BitNot => !v,
                    UnOp::Not => return Ok(((v == 0) as i64, int)),
                    _ => return Err("not an integer constant expression".into()),
                };
                (norm(r, k), to_type(k))
            }
            Expr::Binary(And, a, b) => ((self.eval(a)?.0 != 0 && self.eval(b)?.0 != 0) as i64, int),
            Expr::Binary(Or, a, b) => ((self.eval(a)?.0 != 0 || self.eval(b)?.0 != 0) as i64, int),
            Expr::Binary(Comma, _, b) => self.eval(b)?,
            Expr::Binary(op, a, b) => {
                let (x, tx) = self.eval(a)?;
                let (y, ty) = self.eval(b)?;
                if matches!(op, Shl | Shr) {
                    let k = promote(ikind(&tx));
                    let n = (y & 63) as u32;
                    let r = match (op, k.1) {
                        (Shl, _) => x.wrapping_shl(n),
                        (_, true) => x >> n,
                        _ => ((x as u64) >> n) as i64,
                    };
                    return Ok((norm(r, k), to_type(k)));
                }
                let k = arith(ikind(&tx), ikind(&ty));
                let (x, y) = (norm(x, k), norm(y, k));
                let (ux, uy) = (x as u64, y as u64);
                let cmp = |b: bool| Ok::<_, String>(((b as i64), Type::int()));
                let r = match op {
                    Add => x.wrapping_add(y),
                    Sub => x.wrapping_sub(y),
                    Mul => x.wrapping_mul(y),
                    Div | Mod if y == 0 => {
                        return Err("division by zero in constant expression".into())
                    }
                    Div if k.1 => x.wrapping_div(y),
                    Div => (ux / uy) as i64,
                    Mod if k.1 => x.wrapping_rem(y),
                    Mod => (ux % uy) as i64,
                    BitAnd => x & y,
                    BitOr => x | y,
                    BitXor => x ^ y,
                    Eq => return cmp(x == y),
                    Ne => return cmp(x != y),
                    Lt => return cmp(if k.1 { x < y } else { ux < uy }),
                    Gt => return cmp(if k.1 { x > y } else { ux > uy }),
                    Le => return cmp(if k.1 { x <= y } else { ux <= uy }),
                    Ge => return cmp(if k.1 { x >= y } else { ux >= uy }),
                    Shl | Shr | And | Or | Comma => unreachable!(),
                };
                (norm(r, k), to_type(k))
            }
            // shortcut: the result takes the chosen branch's type, not the common type of both.
            Expr::Cond(c, a, b) => self.eval(if self.eval(c)?.0 != 0 { a } else { b })?,
            Expr::Cast(t @ Type::Int { kind, signed }, a) => {
                let v = match &**a {
                    Expr::Float(f) => *f as i64,
                    a => self.eval(a)?.0,
                };
                (norm(v, (*kind, *signed)), t.clone())
            }
            Expr::Cast(t @ Type::Ptr(_), a) => (self.eval(a)?.0, t.clone()),
            Expr::SizeofType(t) => (self.size_of(t)? as i64, uint()),
            Expr::SizeofExpr(a) => (self.size_of(&self.type_of(a)?)? as i64, uint()),
            _ => return Err("not an integer constant expression".into()),
        })
    }
}
