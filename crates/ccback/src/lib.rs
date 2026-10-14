//! Type checking, code generation and linking: `wsdoom::cc::ast` units ->
//! Whitespace instructions in a `wsdoom::dsl::Prog`.
//!
//! Model (see src/cc/symbols.rs): one cell per scalar, values normalized to
//! their C type. Signed int `+`/`-` are not wrapped (DOOM never relies on
//! that overflow); `*`, `<<`, conversions and all unsigned arithmetic wrap;
//! `/` and `%` truncate like C. Functions that can recurse (a cycle in the
//! call graph, counting each indirect call as a call to every address-taken
//! function) keep their locals in a memory stack; all others use fixed cells.

use std::collections::{BTreeSet, HashMap};
use wsdoom::cc::ast::*;
use wsdoom::cc::symbols::{Builtin, Symbols};
use wsdoom::dsl::{abs, Ex, Fun, Prog, A, V};
use wsdoom::vm::Ins::*;

/// Debug notes: `vm::note_map` maps instructions to indexes into `notes`.
pub struct Debug {
    pub notes: Vec<String>,
}

const STACK_CELLS: i64 = 1 << 16;

type R<T> = Result<T, String>;

pub fn compile(p: &mut Prog, units: &[Unit], builtins: &[Builtin]) -> R<(Symbols, Debug)> {
    let mut c = Compiler::new(p, units);
    c.collect(builtins)?;
    c.find_recursion();
    c.allocate();
    c.helpers();
    let mut errors = vec![];
    for fi in 0..c.funcs.len() {
        if let Some((u, d)) = c.funcs[fi].def {
            if let Err(e) = c.function(fi, u, d) {
                errors.push(format!("{}: {e}", c.where_is(u, d)));
            }
        }
    }
    if !errors.is_empty() {
        return Err(errors.join("\n"));
    }
    c.dispatcher();
    c.static_data()?;
    Ok((
        c.symbols(),
        Debug {
            notes: std::mem::take(&mut c.notes),
        },
    ))
}

struct Func {
    name: String,
    fun: Fun,
    /// (unit, decl index) of the C definition.
    def: Option<(usize, usize)>,
    builtin: Option<Builtin>,
    /// Type from the definition (or the first prototype).
    ty: Option<Type>,
    recursive: bool,
    addr_taken: bool,
}

struct Glob {
    name: String,
    addr: i64,
    size: usize,
    /// Declarations that define storage (not `extern`): (unit, decl).
    defs: Vec<(usize, usize)>,
}

#[derive(Default)]
struct Scope {
    vars: HashMap<String, (usize, Type)>,
    /// Function id and this unit's own declared type of it.
    funcs: HashMap<String, (usize, Option<Type>)>,
}

#[derive(Clone, Copy)]
enum Place {
    Fixed(i64),
    Frame(i64),
}

#[derive(Clone)]
struct Local {
    ty: Type,
    place: Place,
}

enum Name {
    Var(Type, Place),
    Func(usize, Option<Type>),
}

struct Helpers {
    and: Fun,
    or: Fun,
    xor: Fun,
    cdiv: Fun,
    cmod: Fun,
    pow2: Fun,
    copy: Fun,
}

struct Compiler<'a> {
    p: &'a mut Prog,
    units: &'a [Unit],
    funcs: Vec<Func>,
    func_ids: HashMap<String, usize>,
    globs: Vec<Glob>,
    glob_ids: HashMap<String, usize>,
    scopes: Vec<Scope>,
    sp: V,
    tmp: V,
    help: Option<Helpers>,
    dispatch: Fun,
    notes: Vec<String>,
    strings: Vec<(i64, Vec<u8>)>,
    /// Static locals with initializers: (addr, unit, type, init).
    late: Vec<(i64, usize, Type, Init)>,
    // per-function state
    u: usize,
    fname: String,
    locals: Vec<HashMap<String, Local>>,
    framed: bool,
    frame_size: i64,
    size_cell: i64,
    ret_ty: Type,
    breaks: Vec<i64>,
    conts: Vec<i64>,
    labels: HashMap<String, i64>,
    cases: Vec<Vec<(Option<i64>, i64)>>,
}

fn key(u: usize, name: &str, is_static: bool) -> String {
    if is_static {
        format!("{u}:{name}")
    } else {
        name.to_string()
    }
}

/// (bits, signed) of an integer type; pointers count as unsigned 32.
fn int_info(t: &Type) -> Option<(u32, bool)> {
    match t {
        Type::Int { kind, signed } => Some((
            match kind {
                IntKind::Char => 8,
                IntKind::Short => 16,
                IntKind::Int | IntKind::Long => 32,
                IntKind::LongLong => 64,
            },
            *signed,
        )),
        Type::Ptr(_) => Some((32, false)),
        _ => None,
    }
}

fn is_int(t: &Type) -> bool {
    matches!(t, Type::Int { .. })
}

fn is_ptr(t: &Type) -> bool {
    matches!(t, Type::Ptr(_) | Type::Array(..))
}

fn is_aggregate(t: &Type) -> bool {
    matches!(t, Type::Array(..) | Type::Struct(_))
}

fn uint() -> Type {
    Type::Int {
        kind: IntKind::Int,
        signed: false,
    }
}

fn char_t() -> Type {
    Type::Int {
        kind: IntKind::Char,
        signed: true,
    }
}

fn decay(t: Type) -> Type {
    match t {
        Type::Array(e, _) => Type::Ptr(e),
        f @ Type::Func { .. } => Type::Ptr(Box::new(f)),
        t => t,
    }
}

fn pointee(t: &Type) -> Option<Type> {
    match t {
        Type::Ptr(e) | Type::Array(e, _) => Some((**e).clone()),
        _ => None,
    }
}

fn promote(t: &Type) -> Type {
    match t {
        Type::Int {
            kind: IntKind::Char | IntKind::Short,
            ..
        } => Type::int(),
        t => t.clone(),
    }
}

/// Usual arithmetic conversions.
fn uac(a: &Type, b: &Type) -> Type {
    let (ia, ib) = (
        int_info(&promote(a)).unwrap_or((32, true)),
        int_info(&promote(b)).unwrap_or((32, true)),
    );
    if ia.0 == 64 || ib.0 == 64 {
        Type::Int {
            kind: IntKind::LongLong,
            signed: true,
        }
    } else if !ia.1 || !ib.1 {
        uint()
    } else {
        Type::int()
    }
}

/// Wrap a compile-time value to an integer type.
fn wrap_const(v: i64, t: &Type) -> i64 {
    match int_info(t) {
        Some((64, _)) | None => v,
        Some((b, false)) => v.rem_euclid(1 << b),
        Some((b, true)) => (v + (1 << (b - 1))).rem_euclid(1 << b) - (1 << (b - 1)),
    }
}

fn func_parts(t: &Type) -> Option<(Type, Option<Vec<Type>>, bool)> {
    match t {
        Type::Func {
            ret,
            params,
            variadic,
        } => Some(((**ret).clone(), params.clone(), *variadic)),
        Type::Ptr(f) => func_parts(f),
        _ => None,
    }
}

impl<'a> Compiler<'a> {
    fn new(p: &'a mut Prog, units: &'a [Unit]) -> Self {
        let sp = p.var();
        let tmp = p.var();
        let dispatch = p.func(0);
        Compiler {
            p,
            units,
            funcs: vec![],
            func_ids: HashMap::new(),
            globs: vec![],
            glob_ids: HashMap::new(),
            scopes: (0..units.len()).map(|_| Scope::default()).collect(),
            sp,
            tmp,
            help: None,
            dispatch,
            notes: vec![],
            strings: vec![],
            late: vec![],
            u: 0,
            fname: String::new(),
            locals: vec![],
            framed: false,
            frame_size: 0,
            size_cell: 0,
            ret_ty: Type::Void,
            breaks: vec![],
            conts: vec![],
            labels: HashMap::new(),
            cases: vec![],
        }
    }

    fn where_is(&self, u: usize, d: usize) -> String {
        let unit = &self.units[u];
        let (pos, name) = match &unit.decls[d] {
            Decl::Func(f) => (f.pos, &f.name),
            Decl::Var(v) => (v.pos, &v.name),
        };
        format!(
            "{}:{} ({name})",
            unit.files.get(pos.file as usize).map_or("?", |s| s),
            pos.line
        )
    }

    fn size_of(&self, u: usize, t: &Type) -> usize {
        match t {
            Type::Array(e, n) => self.size_of(u, e) * n.unwrap_or(0),
            Type::Struct(i) => self.units[u].structs[*i].size,
            _ => 1,
        }
    }

    fn field(&self, t: &Type, name: &str) -> R<(usize, Type)> {
        if let Type::Struct(i) = t {
            let s = &self.units[self.u].structs[*i];
            return s
                .fields
                .iter()
                .find(|f| f.name == name)
                .map(|f| (f.offset, f.ty.clone()))
                .ok_or_else(|| {
                    format!(
                        "no field `{name}` in struct {}",
                        s.tag.as_deref().unwrap_or("?")
                    )
                });
        }
        Err(format!("`.{name}` on a non-struct"))
    }

    // ------------------------------------------------------------ pass 1

    fn func_id(&mut self, k: String, name: &str) -> usize {
        if let Some(&i) = self.func_ids.get(&k) {
            return i;
        }
        let fun = self.p.func(0);
        self.funcs.push(Func {
            name: name.to_string(),
            fun,
            def: None,
            builtin: None,
            ty: None,
            recursive: false,
            addr_taken: false,
        });
        self.func_ids.insert(k, self.funcs.len() - 1);
        self.funcs.len() - 1
    }

    fn collect(&mut self, builtins: &[Builtin]) -> R<()> {
        for b in builtins {
            let i = self.func_id(b.name.to_string(), b.name);
            self.funcs[i].builtin = Some(*b);
        }
        for (u, unit) in self.units.iter().enumerate() {
            for (d, decl) in unit.decls.iter().enumerate() {
                match decl {
                    Decl::Func(f) => {
                        let i =
                            self.func_id(key(u, &f.name, f.storage == Storage::Static), &f.name);
                        self.scopes[u]
                            .funcs
                            .insert(f.name.clone(), (i, Some(f.ty.clone())));
                        if self.funcs[i].builtin.is_none() {
                            if self.funcs[i].def.is_some() {
                                return Err(format!(
                                    "{}: `{}` defined twice",
                                    self.where_is(u, d),
                                    f.name
                                ));
                            }
                            self.funcs[i].def = Some((u, d));
                            self.funcs[i].ty = Some(f.ty.clone());
                        }
                    }
                    Decl::Var(v) if matches!(v.ty, Type::Func { .. }) => {
                        let i =
                            self.func_id(key(u, &v.name, v.storage == Storage::Static), &v.name);
                        self.scopes[u]
                            .funcs
                            .insert(v.name.clone(), (i, Some(v.ty.clone())));
                        if self.funcs[i].ty.is_none() {
                            self.funcs[i].ty = Some(v.ty.clone());
                        }
                    }
                    Decl::Var(v) => {
                        let k = key(u, &v.name, v.storage == Storage::Static);
                        let i = match self.glob_ids.get(&k) {
                            Some(&i) => i,
                            None => {
                                self.globs.push(Glob {
                                    name: v.name.clone(),
                                    addr: 0,
                                    size: 0,
                                    defs: vec![],
                                });
                                self.glob_ids.insert(k, self.globs.len() - 1);
                                self.globs.len() - 1
                            }
                        };
                        let size = self.size_of(u, &v.ty);
                        let g = &mut self.globs[i];
                        g.size = g.size.max(size);
                        if v.storage != Storage::Extern {
                            g.defs.push((u, d));
                        }
                        self.scopes[u]
                            .vars
                            .insert(v.name.clone(), (i, v.ty.clone()));
                    }
                }
            }
        }
        // now the arity of every function is known
        for f in &mut self.funcs {
            let arity = match (&f.builtin, &f.ty) {
                (Some(b), _) => b.arity,
                (None, Some(t)) => func_parts(t)
                    .and_then(|(_, ps, _)| ps)
                    .map_or(0, |ps| ps.len()),
                _ => 0,
            };
            f.fun = self.p.func(arity);
        }
        Ok(())
    }

    // ------------------------------------------------------------ recursion

    fn find_recursion(&mut self) {
        let n = self.funcs.len();
        let mut edges: Vec<BTreeSet<usize>> = vec![BTreeSet::new(); n];
        let mut indirect = vec![false; n];
        let mut taken = BTreeSet::new();
        for (fi, edge) in edges.iter_mut().enumerate() {
            if let Some((u, d)) = self.funcs[fi].def {
                if let Decl::Func(f) = &self.units[u].decls[d] {
                    let mut s = Scan {
                        scope: &self.scopes[u],
                        ids: &self.func_ids,
                        calls: BTreeSet::new(),
                        taken: &mut taken,
                        indirect: false,
                    };
                    s.stmt(&f.body);
                    *edge = s.calls;
                    indirect[fi] = s.indirect;
                }
            }
        }
        for (u, unit) in self.units.iter().enumerate() {
            for decl in &unit.decls {
                if let Decl::Var(VarDecl {
                    init: Some(init), ..
                }) = decl
                {
                    let mut s = Scan {
                        scope: &self.scopes[u],
                        ids: &self.func_ids,
                        calls: BTreeSet::new(),
                        taken: &mut taken,
                        indirect: false,
                    };
                    s.init(init);
                }
            }
        }
        for &t in &taken {
            self.funcs[t].addr_taken = true;
        }
        for fi in 0..n {
            if indirect[fi] {
                edges[fi].extend(taken.iter().copied());
            }
        }
        let rec = cyclic(&edges);
        for (f, r) in self.funcs.iter_mut().zip(rec) {
            f.recursive = r;
        }
    }

    // ------------------------------------------------------------ storage

    fn allocate(&mut self) {
        // z_zone.c treats owner pointers below 0x100 as tags and m_misc.c
        // treats defaults below 0xfff as numbers: keep C objects above both
        self.p.alloc(0x1000);
        // globals sit in definition order, as a C compiler lays out a unit:
        // r_things.c relies on sprnames[] being followed by states[0] (a 0).
        // Globals only ever declared `extern` (defined in the platform files
        // we replace, e.g. mb_used) still get zeroed storage, like commons.
        let mut order: Vec<usize> = (0..self.globs.len()).collect();
        order.sort_by_key(|&i| {
            self.globs[i]
                .defs
                .first()
                .copied()
                .unwrap_or((usize::MAX, 0))
        });
        for i in order {
            let g = &mut self.globs[i];
            g.addr = self.p.alloc(g.size.max(1) as i64);
        }
        let stack = self.p.alloc(STACK_CELLS);
        // top-level code: the C stack grows down from the end of its block
        self.p.set(self.sp, stack + STACK_CELLS);
    }

    fn helpers(&mut self) {
        let p = &mut *self.p;
        let two32 = 1i64 << 32;
        let bitwise = |p: &mut Prog, op: u8| {
            let f = p.func(2);
            let (a, b, r, m, i) = (p.var(), p.var(), p.var(), p.var(), p.var());
            p.def(f, &[a, b], |p| {
                p.set(a, a % two32);
                p.set(b, b % two32);
                p.set(r, 0);
                p.set(m, 1);
                p.for_(i, 0, 32, |p| {
                    let bit = match op {
                        0 => (a % 2) * (b % 2),
                        1 => (a % 2) + (b % 2) - (a % 2) * (b % 2),
                        _ => ((a % 2) - (b % 2)) * ((a % 2) - (b % 2)),
                    };
                    p.inc(r, bit * m);
                    p.set(a, a / 2);
                    p.set(b, b / 2);
                    p.set(m, m * 2);
                });
                p.ret(r);
            });
            f
        };
        let and = bitwise(p, 0);
        let or = bitwise(p, 1);
        let xor = bitwise(p, 2);
        let cdiv = p.func(2);
        let (a, b) = (p.var(), p.var());
        p.def(cdiv, &[a, b], |p| {
            p.if_else(
                a.lt(0).ne(b.lt(0)),
                |p| p.ret(-(abs(a) / abs(b))),
                |p| p.ret(abs(a) / abs(b)),
            );
        });
        let cmod = p.func(2);
        let (a2, b2) = (p.var(), p.var());
        p.def(cmod, &[a2, b2], |p| {
            p.ret(a2 - b2 * cdiv.call([a2.into(), b2.into()]))
        });
        let pow2 = p.func(1);
        let table = p.arr_init(&(0..63).map(|i| 1i64 << i).collect::<Vec<_>>());
        let n = p.var();
        p.def(pow2, &[n], |p| p.ret(table.at(n)));
        let copy = p.func(3);
        let (dst, src, len, k) = (p.var(), p.var(), p.var(), p.var());
        p.def(copy, &[dst, src, len], |p| {
            p.for_(k, 0, len, |p| p.set(A(0).at(dst + k), A(0).at(src + k)));
            p.ret(dst);
        });
        self.help = Some(Helpers {
            and,
            or,
            xor,
            cdiv,
            cmod,
            pow2,
            copy,
        });
    }

    fn help(&self) -> &Helpers {
        self.help.as_ref().unwrap()
    }

    fn string(&mut self, s: &[u8]) -> i64 {
        let addr = self.p.alloc(s.len() as i64 + 1);
        self.strings.push((addr, s.to_vec()));
        addr
    }

    fn label(&mut self) -> i64 {
        self.p.new_label()
    }

    fn i(&mut self, ins: wsdoom::vm::Ins) {
        self.p.raw(ins);
    }

    // ------------------------------------------------------------ functions

    fn function(&mut self, fi: usize, u: usize, d: usize) -> R<()> {
        let f = match &self.units[u].decls[d] {
            Decl::Func(f) => f,
            _ => unreachable!(),
        };
        let (ret, params, _) = func_parts(&f.ty).ok_or("function without a function type")?;
        let params = params.unwrap_or_default();
        if params.iter().any(is_aggregate) || matches!(ret, Type::Struct(_)) {
            return Err("struct parameters or results are not supported".into());
        }
        self.u = u;
        self.fname = f.name.clone();
        self.ret_ty = ret;
        self.locals = vec![HashMap::new()];
        self.framed = self.funcs[fi].recursive;
        self.frame_size = 0;
        self.size_cell = self.p.alloc(1);
        self.labels.clear();
        let fun = self.funcs[fi].fun;
        let main = self.p.begin_raw(fun);
        self.note(f.pos);
        let places: Vec<Place> = f
            .params
            .iter()
            .zip(&params)
            .map(|(n, t)| self.new_local(n, t.clone()))
            .collect();
        if self.framed {
            // sp -= frame size (patched into size_cell once known)
            self.i(Push(self.sp.0));
            self.i(Push(self.sp.0));
            self.i(Load);
            self.i(Push(self.size_cell));
            self.i(Load);
            self.i(Sub);
            self.i(Store);
        }
        for place in places.iter().rev() {
            self.place_addr(*place);
            self.i(Swap);
            self.i(Store);
        }
        let r = self.stmt(&f.body);
        self.i(Push(0));
        self.epilogue();
        self.p.end_raw(main);
        r?;
        self.p.set_init(self.size_cell, &[self.frame_size]);
        Ok(())
    }

    fn epilogue(&mut self) {
        if self.framed {
            self.i(Push(self.sp.0));
            self.i(Push(self.sp.0));
            self.i(Load);
            self.i(Push(self.size_cell));
            self.i(Load);
            self.i(Add);
            self.i(Store);
        }
        self.i(Ret);
    }

    fn new_local(&mut self, name: &str, ty: Type) -> Place {
        let size = self.size_of(self.u, &ty).max(1) as i64;
        let place = if self.framed {
            let off = self.frame_size;
            self.frame_size += size;
            Place::Frame(off)
        } else {
            Place::Fixed(self.p.alloc(size))
        };
        self.locals
            .last_mut()
            .unwrap()
            .insert(name.to_string(), Local { ty, place });
        place
    }

    fn place_addr(&mut self, place: Place) {
        match place {
            Place::Fixed(a) => self.i(Push(a)),
            Place::Frame(off) => {
                self.i(Push(self.sp.0));
                self.i(Load);
                if off != 0 {
                    self.i(Push(off));
                    self.i(Add);
                }
            }
        }
    }

    fn lookup(&self, name: &str) -> Option<Name> {
        for scope in self.locals.iter().rev() {
            if let Some(l) = scope.get(name) {
                return Some(Name::Var(l.ty.clone(), l.place));
            }
        }
        let s = &self.scopes[self.u];
        if let Some((g, ty)) = s.vars.get(name) {
            return Some(Name::Var(ty.clone(), Place::Fixed(self.globs[*g].addr)));
        }
        if let Some((f, ty)) = s.funcs.get(name) {
            return Some(Name::Func(*f, ty.clone()));
        }
        self.func_ids.get(name).map(|&f| Name::Func(f, None))
    }

    fn note(&mut self, pos: Pos) {
        let file = self.units[self.u]
            .files
            .get(pos.file as usize)
            .map_or("?", |s| s.as_str());
        self.notes
            .push(format!("{file}:{} in {}", pos.line, self.fname));
        self.i(Note(self.notes.len() as i64 - 1));
    }

    // ------------------------------------------------------------ statements

    fn stmt(&mut self, s: &Stmt) -> R<()> {
        if !matches!(s.kind, StmtKind::Block(_) | StmtKind::Empty) {
            self.note(s.pos);
        }
        match &s.kind {
            StmtKind::Expr(e) => self.expr_stmt(e)?,
            StmtKind::Decl(ds) => {
                for d in ds {
                    self.local_decl(d)?;
                }
            }
            StmtKind::Block(ss) => {
                self.locals.push(HashMap::new());
                let r = ss.iter().try_for_each(|s| self.stmt(s));
                self.locals.pop();
                r?;
            }
            StmtKind::If(c, a, b) => {
                let (other, end) = (self.label(), self.label());
                self.cond(c, other, false)?;
                self.stmt(a)?;
                match b {
                    Some(b) => {
                        self.i(Jmp(end));
                        self.i(Label(other));
                        self.stmt(b)?;
                        self.i(Label(end));
                    }
                    None => self.i(Label(other)),
                }
            }
            StmtKind::While(c, b) => {
                let (top, test, end) = (self.label(), self.label(), self.label());
                self.i(Jmp(test));
                self.i(Label(top));
                self.loop_body(b, end, test)?;
                self.i(Label(test));
                self.cond(c, top, true)?;
                self.i(Label(end));
            }
            StmtKind::DoWhile(b, c) => {
                let (top, test, end) = (self.label(), self.label(), self.label());
                self.i(Label(top));
                self.loop_body(b, end, test)?;
                self.i(Label(test));
                self.cond(c, top, true)?;
                self.i(Label(end));
            }
            StmtKind::For(init, c, next, b) => {
                self.locals.push(HashMap::new());
                if let Some(init) = init {
                    self.stmt(init)?;
                }
                let (top, step, test, end) =
                    (self.label(), self.label(), self.label(), self.label());
                self.i(Jmp(test));
                self.i(Label(top));
                self.loop_body(b, end, step)?;
                self.i(Label(step));
                if let Some(n) = next {
                    self.expr_stmt(n)?;
                }
                self.i(Label(test));
                match c {
                    Some(c) => self.cond(c, top, true)?,
                    None => self.i(Jmp(top)),
                }
                self.i(Label(end));
                self.locals.pop();
            }
            StmtKind::Switch(e, b) => {
                let t = self.gen(e)?;
                self.convert(&t, &promote(&t))?;
                let tmp = self.tmp.0;
                self.i(Push(tmp));
                self.i(Swap);
                self.i(Store);
                let (dispatch, end) = (self.label(), self.label());
                self.i(Jmp(dispatch));
                self.cases.push(vec![]);
                self.breaks.push(end);
                let r = self.stmt(b);
                self.breaks.pop();
                let cases = self.cases.pop().unwrap();
                r?;
                self.i(Jmp(end));
                self.i(Label(dispatch));
                let mut default = end;
                for (v, l) in cases {
                    match v {
                        Some(v) => {
                            self.i(Push(tmp));
                            self.i(Load);
                            self.i(Push(v));
                            self.i(Sub);
                            self.i(Jz(l));
                        }
                        None => default = l,
                    }
                }
                self.i(Jmp(default));
                self.i(Label(end));
            }
            StmtKind::Case(v, b) => {
                let l = self.label();
                self.cases
                    .last_mut()
                    .ok_or("case outside switch")?
                    .push((Some(*v), l));
                self.i(Label(l));
                self.stmt(b)?;
            }
            StmtKind::Default(b) => {
                let l = self.label();
                self.cases
                    .last_mut()
                    .ok_or("default outside switch")?
                    .push((None, l));
                self.i(Label(l));
                self.stmt(b)?;
            }
            StmtKind::Break => {
                let l = *self.breaks.last().ok_or("break outside loop")?;
                self.i(Jmp(l));
            }
            StmtKind::Continue => {
                let l = *self.conts.last().ok_or("continue outside loop")?;
                self.i(Jmp(l));
            }
            StmtKind::Return(e) => {
                match e {
                    Some(e) => {
                        let t = self.gen(e)?;
                        let rt = self.ret_ty.clone();
                        if rt != Type::Void {
                            self.convert(&t, &rt)?;
                        }
                    }
                    None => self.i(Push(0)),
                }
                self.epilogue();
            }
            StmtKind::Goto(l) => {
                let l = self.goto_label(l);
                self.i(Jmp(l));
            }
            StmtKind::Label(l, b) => {
                let l = self.goto_label(l);
                self.i(Label(l));
                self.stmt(b)?;
            }
            StmtKind::Empty => {}
        }
        Ok(())
    }

    fn loop_body(&mut self, b: &Stmt, brk: i64, cont: i64) -> R<()> {
        self.breaks.push(brk);
        self.conts.push(cont);
        let r = self.stmt(b);
        self.breaks.pop();
        self.conts.pop();
        r
    }

    fn goto_label(&mut self, name: &str) -> i64 {
        if let Some(&l) = self.labels.get(name) {
            return l;
        }
        let l = self.label();
        self.labels.insert(name.to_string(), l);
        l
    }

    fn expr_stmt(&mut self, e: &Expr) -> R<()> {
        match e {
            Expr::Assign(op, l, r) => {
                self.assign(*op, l, r, false)?;
            }
            Expr::Unary(op @ (UnOp::PreInc | UnOp::PreDec | UnOp::PostInc | UnOp::PostDec), a) => {
                self.incdec(*op, a, false)?;
            }
            Expr::Binary(BinOp::Comma, a, b) => {
                self.expr_stmt(a)?;
                self.expr_stmt(b)?;
            }
            Expr::Cast(Type::Void, a) => self.expr_stmt(a)?,
            _ => {
                self.gen(e)?;
                self.i(Drop);
            }
        }
        Ok(())
    }

    fn local_decl(&mut self, d: &VarDecl) -> R<()> {
        match d.storage {
            Storage::Typedef => Ok(()),
            _ if matches!(d.ty, Type::Func { .. }) => Ok(()), // local prototype
            Storage::Extern => {
                let g = self.scopes[self.u]
                    .vars
                    .get(&d.name)
                    .map(|v| v.0)
                    .or_else(|| self.glob_ids.get(&d.name).copied())
                    .ok_or_else(|| format!("undefined `{}`", d.name))?;
                let place = Place::Fixed(self.globs[g].addr);
                self.locals.last_mut().unwrap().insert(
                    d.name.clone(),
                    Local {
                        ty: d.ty.clone(),
                        place,
                    },
                );
                Ok(())
            }
            Storage::Static => {
                let addr = self.p.alloc(self.size_of(self.u, &d.ty).max(1) as i64);
                if let Some(init) = &d.init {
                    self.late.push((addr, self.u, d.ty.clone(), init.clone()));
                }
                self.locals.last_mut().unwrap().insert(
                    d.name.clone(),
                    Local {
                        ty: d.ty.clone(),
                        place: Place::Fixed(addr),
                    },
                );
                Ok(())
            }
            Storage::None => {
                let place = self.new_local(&d.name, d.ty.clone());
                match &d.init {
                    None => Ok(()),
                    Some(init) => self.init_local(place, &d.ty, init),
                }
            }
        }
    }

    fn init_local(&mut self, place: Place, ty: &Type, init: &Init) -> R<()> {
        match (ty, init) {
            (Type::Struct(_), Init::Expr(e)) if !matches!(e, Expr::Str(_)) => {
                let size = self.size_of(self.u, ty) as i64;
                self.place_addr(place);
                self.gen(e)?;
                self.i(Push(size));
                self.i(Call(self.help().copy.label()));
                self.i(Drop);
                Ok(())
            }
            (t, Init::Expr(e)) if !is_aggregate(t) => {
                self.place_addr(place);
                let et = self.gen(e)?;
                self.convert(&et, t)?;
                self.i(Store);
                Ok(())
            }
            _ => {
                // aggregate: zero it, then store each initialized cell
                let size = self.size_of(self.u, ty);
                for k in 0..size {
                    self.place_addr(place);
                    self.i(Push(k as i64));
                    self.i(Add);
                    self.i(Push(0));
                    self.i(Store);
                }
                let mut cells = vec![];
                let items = match init {
                    Init::List(l) => l.clone(),
                    e => vec![e.clone()],
                };
                let mut pos = 0;
                self.flatten(ty, &items, &mut pos, 0, &mut cells)?;
                for (off, t, e) in cells {
                    self.place_addr(place);
                    self.i(Push(off as i64));
                    self.i(Add);
                    let et = self.gen(&e)?;
                    self.convert(&et, &t)?;
                    self.i(Store);
                }
                Ok(())
            }
        }
    }

    /// Initializer traversal with brace elision: (cell offset, scalar type,
    /// expression) for every initialized scalar of an object of type `ty`.
    fn flatten(
        &self,
        ty: &Type,
        items: &[Init],
        pos: &mut usize,
        off: usize,
        out: &mut Vec<(usize, Type, Expr)>,
    ) -> R<()> {
        match ty {
            Type::Array(elem, n) => {
                if let (
                    Type::Int {
                        kind: IntKind::Char,
                        ..
                    },
                    Some(Init::Expr(Expr::Str(s))),
                ) = (&**elem, items.get(*pos))
                {
                    let n = n.unwrap_or(s.len() + 1);
                    for (k, &b) in s.iter().take(n).enumerate() {
                        out.push((off + k, (**elem).clone(), Expr::Int(b as i64, Type::int())));
                    }
                    *pos += 1;
                    return Ok(());
                }
                let esize = self.size_of(self.u, elem);
                let mut k = 0;
                while n.is_none_or(|n| k < n) && *pos < items.len() {
                    self.member(elem, items, pos, off + k * esize, out)?;
                    k += 1;
                }
            }
            Type::Struct(i) => {
                let s = &self.units[self.u].structs[*i];
                let fields: Vec<Field> = if s.is_union {
                    s.fields.iter().take(1).cloned().collect()
                } else {
                    s.fields.clone()
                };
                for f in fields {
                    if *pos >= items.len() {
                        break;
                    }
                    self.member(&f.ty, items, pos, off + f.offset, out)?;
                }
            }
            t => match items.get(*pos) {
                Some(Init::Expr(e)) => {
                    out.push((off, t.clone(), e.clone()));
                    *pos += 1;
                }
                Some(Init::List(l)) => {
                    let mut p2 = 0;
                    self.flatten(t, l, &mut p2, off, out)?;
                    *pos += 1;
                }
                None => {}
            },
        }
        Ok(())
    }

    fn member(
        &self,
        ty: &Type,
        items: &[Init],
        pos: &mut usize,
        off: usize,
        out: &mut Vec<(usize, Type, Expr)>,
    ) -> R<()> {
        match &items[*pos] {
            Init::List(sub) => {
                let mut p2 = 0;
                self.flatten(ty, sub, &mut p2, off, out)?;
                *pos += 1;
                Ok(())
            }
            // a scalar or string here fills an aggregate member element by element
            Init::Expr(_) => self.flatten(ty, items, pos, off, out),
        }
    }

    // ------------------------------------------------------------ typing

    fn type_of(&mut self, e: &Expr) -> R<Type> {
        Ok(match e {
            Expr::Int(_, t) => t.clone(),
            Expr::Float(_) => Type::Float,
            Expr::Str(s) => Type::Array(Box::new(char_t()), Some(s.len() + 1)),
            Expr::Ident(n) => match self.lookup(n) {
                Some(Name::Var(t, _)) => t,
                Some(Name::Func(f, t)) => {
                    t.or_else(|| self.funcs[f].ty.clone())
                        .unwrap_or(Type::Func {
                            ret: Box::new(Type::int()),
                            params: None,
                            variadic: false,
                        })
                }
                None => return Err(format!("undeclared `{n}`")),
            },
            Expr::Unary(op, a) => {
                let t = decay(self.type_of(a)?);
                match op {
                    UnOp::Neg | UnOp::Plus | UnOp::BitNot => promote(&t),
                    UnOp::Not => Type::int(),
                    UnOp::Deref => pointee(&t).ok_or("dereferencing a non-pointer")?,
                    UnOp::Addr => Type::Ptr(Box::new(self.type_of(a)?)),
                    _ => self.type_of(a)?,
                }
            }
            Expr::Binary(op, a, b) => {
                let (ta, tb) = (decay(self.type_of(a)?), decay(self.type_of(b)?));
                match op {
                    BinOp::Add | BinOp::Sub => {
                        if is_ptr(&ta) && is_ptr(&tb) {
                            Type::int()
                        } else if is_ptr(&ta) {
                            ta
                        } else if is_ptr(&tb) {
                            tb
                        } else {
                            uac(&ta, &tb)
                        }
                    }
                    BinOp::Shl | BinOp::Shr => promote(&ta),
                    BinOp::Mul
                    | BinOp::Div
                    | BinOp::Mod
                    | BinOp::BitAnd
                    | BinOp::BitOr
                    | BinOp::BitXor => uac(&ta, &tb),
                    BinOp::Comma => tb,
                    _ => Type::int(),
                }
            }
            Expr::Assign(_, l, _) => self.type_of(l)?,
            Expr::Cond(_, a, b) => {
                let (ta, tb) = (decay(self.type_of(a)?), decay(self.type_of(b)?));
                if is_ptr(&ta) {
                    ta
                } else if is_ptr(&tb) {
                    tb
                } else if ta == Type::Void {
                    Type::Void
                } else {
                    uac(&ta, &tb)
                }
            }
            Expr::Call(f, _) => {
                let ft = self.type_of(f)?;
                func_parts(&ft).map(|(r, _, _)| r).unwrap_or_else(Type::int)
            }
            Expr::Index(a, b) => {
                let (ta, tb) = (decay(self.type_of(a)?), decay(self.type_of(b)?));
                pointee(&ta)
                    .or_else(|| pointee(&tb))
                    .ok_or("indexing a non-pointer")?
            }
            Expr::Member(a, f) => {
                let t = self.type_of(a)?;
                self.field(&t, f)?.1
            }
            Expr::Arrow(a, f) => {
                let t = pointee(&decay(self.type_of(a)?)).ok_or("-> on a non-pointer")?;
                self.field(&t, f)?.1
            }
            Expr::Cast(t, _) => t.clone(),
            Expr::SizeofType(_) | Expr::SizeofExpr(_) => uint(),
        })
    }

    // ------------------------------------------------------------ conversions

    /// `a & k` for the value `a` on the stack and a constant mask: floor
    /// modulo by powers of two gives two's complement bits of any value.
    fn and_const(&mut self, k: i64) {
        if k < 0 {
            // a & k = a - (a & ~k)
            self.i(Dup);
            self.and_const(!k);
            self.i(Sub);
            return;
        }
        match runs(k).as_slice() {
            [] => {
                self.i(Drop);
                self.i(Push(0));
            }
            &[(0, h)] => {
                self.i(Push(1 << h));
                self.i(Mod);
            }
            &[(l, h)] => {
                self.i(Dup);
                self.i(Push(1 << h));
                self.i(Mod);
                self.i(Swap);
                self.i(Push(1 << l));
                self.i(Mod);
                self.i(Sub);
            }
            rs => {
                self.i(Push(0));
                for &(l, h) in rs {
                    self.i(Copy(1));
                    self.i(Push(1 << h));
                    self.i(Mod);
                    if l > 0 {
                        self.i(Copy(2));
                        self.i(Push(1 << l));
                        self.i(Mod);
                        self.i(Sub);
                    }
                    self.i(Add);
                }
                self.i(Slide(1));
            }
        }
    }

    fn wrap(&mut self, bits: u32, signed: bool) {
        if bits >= 64 {
            return;
        }
        let m = 1i64 << bits;
        if signed {
            let h = m / 2;
            self.i(Push(h));
            self.i(Add);
            self.i(Push(m));
            self.i(Mod);
            self.i(Push(h));
            self.i(Sub);
        } else {
            self.i(Push(m));
            self.i(Mod);
        }
    }

    /// Convert the value on top of the stack from `from` to `to`.
    fn convert(&mut self, from: &Type, to: &Type) -> R<()> {
        let from = decay(from.clone());
        if matches!(to, Type::Float) || matches!(from, Type::Float) {
            return Err("floating point is not supported".into());
        }
        if let (Some((fb, fs)), Type::Int { .. }) = (int_info(&from), to) {
            let (tb, ts) = int_info(to).unwrap();
            let fits = (fs == ts && fb <= tb) || (!fs && ts && fb < tb);
            if !fits {
                self.wrap(tb, ts);
            }
        }
        Ok(())
    }

    /// Normalize an arithmetic result of (promoted) type `t` after `op`.
    fn normalize(&mut self, t: &Type, op: BinOp) {
        if let Some((bits, signed)) = int_info(t) {
            if !signed || matches!(op, BinOp::Mul | BinOp::Shl) {
                self.wrap(bits, signed);
            }
        }
    }

    // ------------------------------------------------------------ expressions

    /// Emit code leaving the value of `e`; returns its (decayed) type.
    fn gen(&mut self, e: &Expr) -> R<Type> {
        // floating point only appears in constants (am_map.c scales):
        // fold them, truncating like the conversion to fixed_t that follows
        if has_float(e) {
            if let Some(v) = konst_f(e) {
                self.i(Push(v.trunc() as i64));
                return Ok(Type::int());
            }
        }
        match e {
            Expr::Int(v, t) => {
                self.i(Push(*v));
                Ok(t.clone())
            }
            Expr::Float(_) => Err("floating point is not supported".into()),
            Expr::Str(s) => {
                let a = self.string(s);
                self.i(Push(a));
                Ok(Type::Ptr(Box::new(char_t())))
            }
            Expr::Ident(n) => match self.lookup(n) {
                Some(Name::Var(t, place)) => {
                    self.place_addr(place);
                    Ok(self.load(t))
                }
                Some(Name::Func(f, t)) => {
                    self.i(Push(self.funcs[f].fun.label()));
                    let t = t
                        .or_else(|| self.funcs[f].ty.clone())
                        .unwrap_or(Type::Func {
                            ret: Box::new(Type::int()),
                            params: None,
                            variadic: false,
                        });
                    Ok(decay(t))
                }
                None => Err(format!("undeclared `{n}`")),
            },
            Expr::Unary(op, a) => self.unary(*op, a),
            Expr::Binary(op, a, b) => self.binary(*op, a, b),
            Expr::Assign(op, l, r) => self.assign(*op, l, r, true),
            Expr::Cond(c, a, b) => {
                let t = self.type_of(e)?;
                let (other, end) = (self.label(), self.label());
                self.cond(c, other, false)?;
                let ta = self.gen(a)?;
                if t != Type::Void {
                    self.convert(&ta, &t)?;
                }
                self.i(Jmp(end));
                self.i(Label(other));
                let tb = self.gen(b)?;
                if t != Type::Void {
                    self.convert(&tb, &t)?;
                }
                self.i(Label(end));
                Ok(t)
            }
            Expr::Call(f, args) => self.call(f, args),
            Expr::Index(..) | Expr::Member(..) | Expr::Arrow(..) => {
                let t = self.gen_addr(e)?;
                Ok(self.load(t))
            }
            Expr::Cast(t, a) => {
                let from = self.gen(a)?;
                if *t != Type::Void {
                    self.convert(&from, t)?;
                }
                Ok(decay(t.clone()))
            }
            Expr::SizeofType(t) => {
                let s = self.size_of(self.u, t) as i64;
                self.i(Push(s));
                Ok(uint())
            }
            Expr::SizeofExpr(a) => {
                let t = self.type_of(a)?;
                let s = self.size_of(self.u, &t) as i64;
                self.i(Push(s));
                Ok(uint())
            }
        }
    }

    /// With an object's address on the stack, produce its value: arrays and
    /// structs are represented by their address.
    fn load(&mut self, t: Type) -> Type {
        match t {
            Type::Array(..) | Type::Func { .. } => decay(t),
            Type::Struct(_) => t,
            t => {
                self.i(Load);
                t
            }
        }
    }

    /// Emit the address of an lvalue; returns the object's type.
    fn gen_addr(&mut self, e: &Expr) -> R<Type> {
        match e {
            Expr::Ident(n) => match self.lookup(n) {
                Some(Name::Var(t, place)) => {
                    self.place_addr(place);
                    Ok(t)
                }
                Some(Name::Func(f, t)) => {
                    self.i(Push(self.funcs[f].fun.label()));
                    Ok(t.or_else(|| self.funcs[f].ty.clone())
                        .unwrap_or(Type::int()))
                }
                None => Err(format!("undeclared `{n}`")),
            },
            Expr::Unary(UnOp::Deref, a) => {
                let t = self.gen(a)?;
                pointee(&t).ok_or_else(|| "dereferencing a non-pointer".to_string())
            }
            Expr::Index(a, b) => {
                let (ta, tb) = (decay(self.type_of(a)?), decay(self.type_of(b)?));
                let (p, i) = if is_ptr(&ta) { (a, b) } else { (b, a) };
                let elem =
                    pointee(&if is_ptr(&ta) { ta } else { tb }).ok_or("indexing a non-pointer")?;
                self.gen(p)?;
                let it = self.gen(i)?;
                self.convert(&it, &Type::int())?;
                self.scale(&elem);
                self.i(Add);
                Ok(elem)
            }
            Expr::Member(a, f) => {
                let t = self.gen_addr(a)?;
                let (off, ft) = self.field(&t, f)?;
                self.offset(off);
                Ok(ft)
            }
            Expr::Arrow(a, f) => {
                let t = self.gen(a)?;
                let st = pointee(&t).ok_or("-> on a non-pointer")?;
                let (off, ft) = self.field(&st, f)?;
                self.offset(off);
                Ok(ft)
            }
            Expr::Str(s) => {
                let a = self.string(s);
                self.i(Push(a));
                Ok(Type::Array(Box::new(char_t()), Some(s.len() + 1)))
            }
            Expr::Cast(t, a) => {
                self.gen_addr(a)?;
                Ok(t.clone())
            }
            _ => Err(format!("not an lvalue: {e:?}")),
        }
    }

    fn offset(&mut self, off: usize) {
        if off != 0 {
            self.i(Push(off as i64));
            self.i(Add);
        }
    }

    /// Multiply the index on top of the stack by the size of `elem`.
    fn scale(&mut self, elem: &Type) {
        let s = self.size_of(self.u, elem) as i64;
        if s != 1 {
            self.i(Push(s));
            self.i(Mul);
        }
    }

    fn unary(&mut self, op: UnOp, a: &Expr) -> R<Type> {
        match op {
            UnOp::Neg | UnOp::BitNot => {
                let t = self.gen(a)?;
                let pt = promote(&t);
                self.convert(&t, &pt)?;
                self.i(Push(-1));
                self.i(Mul);
                if op == UnOp::BitNot {
                    self.i(Push(1));
                    self.i(Sub);
                }
                self.normalize(&pt, BinOp::Sub);
                Ok(pt)
            }
            UnOp::Plus => {
                let t = self.gen(a)?;
                Ok(promote(&t))
            }
            UnOp::Not => self.cond_value(&Expr::Unary(UnOp::Not, Box::new(a.clone()))),
            UnOp::Deref => {
                // `*(int *)chars` (sprite name compares in r_things.c): DOOM
                // means the little-endian bytes, which here are 4 cells
                if let Expr::Cast(Type::Ptr(target), inner) = a {
                    let src = decay(self.type_of(inner)?);
                    if let (
                        Some((bits @ (16 | 32), signed)),
                        Some(Type::Int {
                            kind: IntKind::Char,
                            ..
                        }),
                    ) = (int_info(target), pointee(&src))
                    {
                        if is_int(target) {
                            self.gen(inner)?;
                            self.i(Push(0));
                            for k in (0..bits as i64 / 8).rev() {
                                self.i(Push(256));
                                self.i(Mul);
                                self.i(Copy(1));
                                self.offset(k as usize);
                                self.i(Load);
                                self.i(Push(256));
                                self.i(Mod);
                                self.i(Add);
                            }
                            self.i(Swap);
                            self.i(Drop);
                            self.wrap(bits, signed);
                            return Ok((**target).clone());
                        }
                    }
                }
                let t = self.gen(a)?;
                let pt = pointee(&t).ok_or("dereferencing a non-pointer")?;
                Ok(self.load(pt))
            }
            UnOp::Addr => match a {
                Expr::Ident(n) if matches!(self.lookup(n), Some(Name::Func(..))) => self.gen(a),
                _ => {
                    let t = self.gen_addr(a)?;
                    Ok(Type::Ptr(Box::new(t)))
                }
            },
            UnOp::PreInc | UnOp::PreDec | UnOp::PostInc | UnOp::PostDec => self.incdec(op, a, true),
        }
    }

    fn binary(&mut self, op: BinOp, a: &Expr, b: &Expr) -> R<Type> {
        match op {
            BinOp::And
            | BinOp::Or
            | BinOp::Lt
            | BinOp::Gt
            | BinOp::Le
            | BinOp::Ge
            | BinOp::Eq
            | BinOp::Ne => {
                return self.cond_value(&Expr::Binary(
                    op,
                    Box::new(a.clone()),
                    Box::new(b.clone()),
                ));
            }
            BinOp::Comma => {
                self.expr_stmt(a)?;
                return self.gen(b);
            }
            _ => {}
        }
        let (ta, tb) = (decay(self.type_of(a)?), decay(self.type_of(b)?));
        if matches!(op, BinOp::Add | BinOp::Sub) && (is_ptr(&ta) || is_ptr(&tb)) {
            if is_ptr(&ta) && is_ptr(&tb) {
                self.gen(a)?;
                self.gen(b)?;
                self.i(Sub);
                let s = self.size_of(self.u, &pointee(&ta).unwrap()) as i64;
                if s != 1 {
                    self.i(Push(s));
                    self.i(Div);
                }
                return Ok(Type::int());
            }
            let (pt, p, n) = if is_ptr(&ta) { (ta, a, b) } else { (tb, b, a) };
            let elem = pointee(&pt).unwrap();
            if is_ptr(&self.type_of(a).map(decay)?) {
                self.gen(p)?;
                let nt = self.gen(n)?;
                self.convert(&nt, &Type::int())?;
                self.scale(&elem);
            } else {
                let nt = self.gen(n)?;
                self.convert(&nt, &Type::int())?;
                self.scale(&elem);
                self.gen(p)?;
            }
            self.i(if op == BinOp::Add { Add } else { Sub });
            return Ok(pt);
        }
        let shift = matches!(op, BinOp::Shl | BinOp::Shr);
        let t = if shift { promote(&ta) } else { uac(&ta, &tb) };
        self.gen(a)?;
        self.convert(&ta, &t)?;
        self.arith(op, &t, &tb, b)?;
        Ok(t)
    }

    /// With the left operand (already of type `t`) on the stack, apply
    /// `op` with right operand `b` (of type `tb`).
    fn arith(&mut self, op: BinOp, t: &Type, tb: &Type, b: &Expr) -> R<()> {
        let signed = int_info(t).is_none_or(|i| i.1);
        let bits = int_info(t).map_or(32, |i| i.0);
        let konst = match b {
            Expr::Int(v, _) => Some(*v),
            // folded masks such as r_draw.c's `&(63*64)` or `&~0xff`
            Expr::Binary(..) | Expr::Unary(UnOp::Neg | UnOp::BitNot, _) | Expr::Cast(..)
                if !has_float(b) =>
            {
                self.konst(b).ok()
            }
            _ => None,
        };
        match op {
            BinOp::Shl | BinOp::Shr => {
                match konst {
                    Some(n) if (0..63).contains(&n) => self.i(Push(1i64 << n)),
                    _ => {
                        let bt = self.gen(b)?;
                        self.convert(&bt, &Type::int())?;
                        self.i(Call(self.help().pow2.label()));
                    }
                }
                self.i(if op == BinOp::Shl { Mul } else { Div });
                if op == BinOp::Shl {
                    self.normalize(t, BinOp::Shl);
                }
            }
            BinOp::BitAnd | BinOp::BitOr | BinOp::BitXor
                if konst.is_some_and(|k| runs(k).len() <= 4) =>
            {
                let k = konst.unwrap();
                match op {
                    BinOp::BitAnd => self.and_const(k),
                    // a | k = a + k - (a & k);  a ^ k = a + k - 2 (a & k)
                    _ => {
                        self.i(Dup);
                        self.and_const(k);
                        if op == BinOp::BitXor {
                            self.i(Push(2));
                            self.i(Mul);
                        }
                        self.i(Push(k));
                        self.i(Swap);
                        self.i(Sub);
                        self.i(Add);
                        if k < 0 {
                            self.wrap(bits, signed);
                        }
                    }
                }
            }
            BinOp::BitAnd | BinOp::BitOr | BinOp::BitXor => {
                let bt = self.gen(b)?;
                self.convert(&bt, t)?;
                let f = match op {
                    BinOp::BitAnd => self.help().and,
                    BinOp::BitOr => self.help().or,
                    _ => self.help().xor,
                };
                self.i(Call(f.label()));
                if bits == 32 && signed {
                    self.wrap(32, true);
                }
            }
            BinOp::Div | BinOp::Mod if signed => {
                let bt = self.gen(b)?;
                self.convert(&bt, t)?;
                let f = if op == BinOp::Div {
                    self.help().cdiv
                } else {
                    self.help().cmod
                };
                self.i(Call(f.label()));
                if op == BinOp::Div {
                    self.wrap(bits, true);
                }
            }
            _ => {
                let bt = self.gen(b)?;
                let _ = tb;
                self.convert(&bt, t)?;
                self.i(match op {
                    BinOp::Add => Add,
                    BinOp::Sub => Sub,
                    BinOp::Mul => Mul,
                    BinOp::Div => Div,
                    BinOp::Mod => Mod,
                    _ => return Err(format!("unexpected operator {op:?}")),
                });
                self.normalize(t, op);
            }
        }
        Ok(())
    }

    fn assign(&mut self, op: Option<BinOp>, l: &Expr, r: &Expr, want: bool) -> R<Type> {
        let lt = self.type_of(l)?;
        if let Type::Struct(_) = lt {
            let size = self.size_of(self.u, &lt) as i64;
            self.gen_addr(l)?;
            self.gen(r)?;
            self.i(Push(size));
            self.i(Call(self.help().copy.label()));
            if !want {
                self.i(Drop);
            }
            return Ok(lt);
        }
        match op {
            None => {
                if want {
                    let rt = self.gen(r)?;
                    self.convert(&rt, &lt)?;
                    self.i(Dup);
                    self.gen_addr(l)?;
                    self.i(Swap);
                    self.i(Store);
                } else {
                    self.gen_addr(l)?;
                    let rt = self.gen(r)?;
                    self.convert(&rt, &lt)?;
                    self.i(Store);
                }
            }
            Some(op) => {
                self.gen_addr(l)?;
                self.i(Dup);
                self.i(Load);
                let rt = decay(self.type_of(r)?);
                if is_ptr(&lt) && matches!(op, BinOp::Add | BinOp::Sub) {
                    let nt = self.gen(r)?;
                    self.convert(&nt, &Type::int())?;
                    self.scale(&pointee(&lt).unwrap());
                    self.i(if op == BinOp::Add { Add } else { Sub });
                } else {
                    let t = if matches!(op, BinOp::Shl | BinOp::Shr) {
                        promote(&lt)
                    } else {
                        uac(&lt, &rt)
                    };
                    self.convert(&lt, &t)?;
                    self.arith(op, &t, &rt, r)?;
                    self.convert(&t, &lt)?;
                }
                self.store_keep(want);
            }
        }
        Ok(lt)
    }

    /// `[addr value]` -> store; if `want`, leave the value.
    fn store_keep(&mut self, want: bool) {
        if want {
            self.i(Dup);
            self.i(Push(self.tmp.0));
            self.i(Swap);
            self.i(Store);
            self.i(Store);
            self.i(Push(self.tmp.0));
            self.i(Load);
        } else {
            self.i(Store);
        }
    }

    fn incdec(&mut self, op: UnOp, a: &Expr, want: bool) -> R<Type> {
        let t = self.type_of(a)?;
        let step = match pointee(&decay(t.clone())) {
            Some(e) if is_ptr(&t) => self.size_of(self.u, &e) as i64,
            _ => 1,
        };
        let inc = matches!(op, UnOp::PreInc | UnOp::PostInc);
        let post = matches!(op, UnOp::PostInc | UnOp::PostDec);
        self.gen_addr(a)?;
        self.i(Dup);
        self.i(Load);
        if want && post {
            self.i(Dup);
            self.i(Push(self.tmp.0));
            self.i(Swap);
            self.i(Store);
        }
        self.i(Push(if inc { step } else { -step }));
        self.i(Add);
        if let Some((bits, signed)) = int_info(&t) {
            if is_ptr(&t) {
            } else if !signed || bits < 32 {
                self.wrap(bits, signed);
            }
        }
        if want && post {
            self.i(Store);
            self.i(Push(self.tmp.0));
            self.i(Load);
        } else {
            self.store_keep(want);
        }
        Ok(decay(t))
    }

    fn call(&mut self, f: &Expr, args: &[Expr]) -> R<Type> {
        // memcpy(shorts, ..., 2*n) (r_segs.c): a byte count written as
        // element size times n is n cells here
        if let (Expr::Ident(n), [dst, src, size]) = (f, args) {
            if matches!(n.as_str(), "memcpy" | "memmove" | "memset") {
                let bytes = match pointee(&decay(self.type_of(dst)?)) {
                    Some(Type::Int {
                        kind: IntKind::Short,
                        ..
                    }) => 2,
                    Some(
                        Type::Int {
                            kind: IntKind::Int | IntKind::Long,
                            ..
                        }
                        | Type::Ptr(_),
                    ) => 4,
                    _ => 0,
                };
                let count = match size {
                    Expr::Binary(BinOp::Mul, k, e) | Expr::Binary(BinOp::Mul, e, k) if matches!(**k, Expr::Int(v, _) if v == bytes) => {
                        Some(e)
                    }
                    _ => None,
                };
                if let Some(count) = count {
                    return self.call(f, &[dst.clone(), src.clone(), (**count).clone()]);
                }
            }
        }
        // direct call?
        let direct = match f {
            Expr::Ident(n) => match self.lookup(n) {
                Some(Name::Func(fi, t)) => Some((fi, t)),
                _ => None,
            },
            _ => None,
        };
        let ft = match &direct {
            Some((fi, t)) => t.clone().or_else(|| self.funcs[*fi].ty.clone()),
            None => Some(self.type_of(f)?),
        };
        let (ret, params, mut variadic) =
            ft.as_ref()
                .and_then(func_parts)
                .unwrap_or((Type::int(), None, false));
        let mut nfixed = params.as_ref().map_or(args.len(), |p| p.len());
        if let Some((fi, _)) = &direct {
            let func = &self.funcs[*fi];
            if func.def.is_none() && func.builtin.is_none() {
                return Err(format!("call to undefined function `{}`", func.name));
            }
            if let Some(b) = func.builtin {
                variadic = b.variadic;
                nfixed = b.arity;
            }
        }
        if args.len() < nfixed || (!variadic && args.len() != nfixed && params.is_some()) {
            return Err(format!(
                "wrong number of arguments ({}, expected {nfixed}) in call",
                args.len()
            ));
        }
        for (k, a) in args.iter().enumerate() {
            let at = self.gen(a)?;
            if let Type::Struct(_) = at {
                return Err("struct arguments are not supported".into());
            }
            match params.as_ref().and_then(|p| p.get(k)) {
                Some(pt) => self.convert(&at, pt)?,
                None => self.convert(&at, &promote(&at))?,
            }
        }
        if variadic {
            self.i(Push((args.len() - nfixed) as i64));
        }
        match direct {
            Some((fi, _)) => self.i(Call(self.funcs[fi].fun.label())),
            None => {
                self.gen(f)?;
                self.i(Call(self.dispatch.label()));
            }
        }
        if let Type::Struct(_) = ret {
            return Err("struct results are not supported".into());
        }
        Ok(ret)
    }

    // ------------------------------------------------------------ conditions

    fn cond_value(&mut self, e: &Expr) -> R<Type> {
        let (no, end) = (self.label(), self.label());
        self.cond(e, no, false)?;
        self.i(Push(1));
        self.i(Jmp(end));
        self.i(Label(no));
        self.i(Push(0));
        self.i(Label(end));
        Ok(Type::int())
    }

    /// Jump to `target` when the truth of `e` equals `when`.
    fn cond(&mut self, e: &Expr, target: i64, when: bool) -> R<()> {
        match e {
            Expr::Unary(UnOp::Not, a) => self.cond(a, target, !when),
            Expr::Binary(BinOp::And, a, b) if !when => {
                self.cond(a, target, false)?;
                self.cond(b, target, false)
            }
            Expr::Binary(BinOp::Or, a, b) if when => {
                self.cond(a, target, true)?;
                self.cond(b, target, true)
            }
            Expr::Binary(op @ (BinOp::And | BinOp::Or), a, b) => {
                let skip = self.label();
                self.cond(a, skip, *op == BinOp::Or)?;
                self.cond(b, target, when)?;
                self.i(Label(skip));
                Ok(())
            }
            Expr::Binary(
                op @ (BinOp::Lt | BinOp::Gt | BinOp::Le | BinOp::Ge | BinOp::Eq | BinOp::Ne),
                a,
                b,
            ) => {
                let (ta, tb) = (decay(self.type_of(a)?), decay(self.type_of(b)?));
                let t = if is_ptr(&ta) || is_ptr(&tb) {
                    uint()
                } else {
                    uac(&ta, &tb)
                };
                // a<b and a>=b test a-b; a>b and a<=b test b-a (operand order is unspecified in C)
                let (x, tx, y, ty) = match op {
                    BinOp::Gt | BinOp::Le => (b, tb, a, ta),
                    _ => (a, ta, b, tb),
                };
                self.gen(x)?;
                self.convert(&tx, &t)?;
                if !matches!(**y, Expr::Int(0, _)) {
                    self.gen(y)?;
                    self.convert(&ty, &t)?;
                    self.i(Sub);
                }
                let (zero, negate) = match op {
                    BinOp::Lt | BinOp::Gt => (false, false),
                    BinOp::Le | BinOp::Ge => (false, true),
                    BinOp::Eq => (true, false),
                    _ => (true, true),
                };
                self.test(zero, target, when != negate);
                Ok(())
            }
            Expr::Int(v, _) => {
                if (*v != 0) == when {
                    self.i(Jmp(target));
                }
                Ok(())
            }
            _ => {
                self.gen(e)?;
                self.test(true, target, !when);
                Ok(())
            }
        }
    }

    fn test(&mut self, zero: bool, target: i64, when: bool) {
        let j = |l| if zero { Jz(l) } else { Jn(l) };
        if when {
            self.i(j(target));
        } else {
            let skip = self.label();
            self.i(j(skip));
            self.i(Jmp(target));
            self.i(Label(skip));
        }
    }

    // ------------------------------------------------------------ linking

    fn dispatcher(&mut self) {
        let mut targets: Vec<(i64, i64)> = self
            .funcs
            .iter()
            .filter(|f| f.addr_taken && (f.def.is_some() || f.builtin.is_some()))
            .map(|f| (f.fun.label(), f.fun.label()))
            .collect();
        targets.sort();
        let main = self.p.begin_raw(self.dispatch);
        let bad = self.label();
        self.tree(&targets, bad);
        self.i(Label(bad));
        for ch in b"bad function pointer\n" {
            self.i(Push(*ch as i64));
            self.i(PutC);
        }
        self.i(End);
        self.p.end_raw(main);
    }

    fn tree(&mut self, t: &[(i64, i64)], bad: i64) {
        if t.is_empty() {
            self.i(Jmp(bad));
            return;
        }
        if t.len() == 1 {
            let ok = self.label();
            self.i(Dup);
            self.i(Push(t[0].0));
            self.i(Sub);
            self.i(Jz(ok));
            self.i(Jmp(bad));
            self.i(Label(ok));
            self.i(Drop);
            self.i(Jmp(t[0].1));
            return;
        }
        let mid = t.len() / 2;
        let left = self.label();
        self.i(Dup);
        self.i(Push(t[mid].0));
        self.i(Sub);
        self.i(Jn(left));
        self.tree(&t[mid..], bad);
        self.i(Label(left));
        self.tree(&t[..mid], bad);
    }

    fn static_data(&mut self) -> R<()> {
        let mut inits = vec![];
        for g in &self.globs {
            for &(u, d) in &g.defs {
                if let Decl::Var(VarDecl {
                    init: Some(init),
                    ty,
                    ..
                }) = &self.units[u].decls[d]
                {
                    inits.push((g.addr, u, ty.clone(), init.clone()));
                }
            }
        }
        inits.append(&mut self.late);
        for (addr, u, ty, init) in inits {
            self.u = u;
            let items = match &init {
                Init::List(l) => l.clone(),
                e => vec![e.clone()],
            };
            let mut cells = vec![];
            let mut pos = 0;
            self.flatten(&ty, &items, &mut pos, 0, &mut cells)?;
            let mut out = vec![0; self.size_of(u, &ty).max(1)];
            for (off, t, e) in cells {
                let v = self
                    .konst(&e)
                    .map_err(|err| format!("initializer of global at {addr}: {err}"))?;
                if off < out.len() {
                    out[off] = wrap_const(v, &t);
                }
            }
            self.p.set_init(addr, &out);
        }
        for (addr, s) in std::mem::take(&mut self.strings) {
            let mut cells: Vec<i64> = s.iter().map(|&b| b as i8 as i64).collect();
            cells.push(0);
            self.p.set_init(addr, &cells);
        }
        Ok(())
    }

    /// Value of a constant expression (integers and address constants).
    fn konst(&mut self, e: &Expr) -> R<i64> {
        if has_float(e) {
            return konst_f(e)
                .map(|v| v.trunc() as i64)
                .ok_or_else(|| format!("not constant: {e:?}"));
        }
        Ok(match e {
            Expr::Int(v, _) => *v,
            Expr::Str(s) => self.string(s),
            Expr::Ident(n) => match self.lookup(n) {
                Some(Name::Func(f, _)) => self.funcs[f].fun.label(),
                Some(Name::Var(Type::Array(..), Place::Fixed(a))) => a,
                _ => return Err(format!("`{n}` is not constant")),
            },
            Expr::Unary(UnOp::Addr, a) => self.konst_addr(a)?,
            // `seq[0]` of a 2-D array decays to an address constant
            Expr::Index(..) | Expr::Member(..) if matches!(self.type_of(e)?, Type::Array(..)) => {
                self.konst_addr(e)?
            }
            Expr::Unary(op, a) => {
                let v = self.konst(a)?;
                match op {
                    UnOp::Neg => -v,
                    UnOp::Plus => v,
                    UnOp::Not => (v == 0) as i64,
                    UnOp::BitNot => !v,
                    _ => return Err("not constant".into()),
                }
            }
            Expr::Binary(op, a, b) => {
                let (x, y) = (self.konst(a)?, self.konst(b)?);
                match op {
                    BinOp::Add => x + y,
                    BinOp::Sub => x - y,
                    BinOp::Mul => x * y,
                    BinOp::Div => x.checked_div(y).ok_or("division by zero")?,
                    BinOp::Mod => x.checked_rem(y).ok_or("division by zero")?,
                    BinOp::Shl => x << y,
                    BinOp::Shr => x >> y,
                    BinOp::BitAnd => x & y,
                    BinOp::BitOr => x | y,
                    BinOp::BitXor => x ^ y,
                    BinOp::Lt => (x < y) as i64,
                    BinOp::Gt => (x > y) as i64,
                    BinOp::Le => (x <= y) as i64,
                    BinOp::Ge => (x >= y) as i64,
                    BinOp::Eq => (x == y) as i64,
                    BinOp::Ne => (x != y) as i64,
                    BinOp::And => (x != 0 && y != 0) as i64,
                    BinOp::Or => (x != 0 || y != 0) as i64,
                    BinOp::Comma => y,
                }
            }
            Expr::Cond(c, a, b) => {
                if self.konst(c)? != 0 {
                    self.konst(a)?
                } else {
                    self.konst(b)?
                }
            }
            Expr::Cast(t, a) => wrap_const(self.konst(a)?, t),
            Expr::SizeofType(t) => self.size_of(self.u, t) as i64,
            Expr::SizeofExpr(a) => {
                let t = self.type_of(a)?;
                self.size_of(self.u, &t) as i64
            }
            _ => return Err(format!("not constant: {e:?}")),
        })
    }

    fn konst_addr(&mut self, e: &Expr) -> R<i64> {
        Ok(match e {
            Expr::Ident(n) => match self.lookup(n) {
                Some(Name::Var(_, Place::Fixed(a))) => a,
                Some(Name::Func(f, _)) => self.funcs[f].fun.label(),
                _ => return Err(format!("address of `{n}` is not constant")),
            },
            Expr::Member(a, f) => {
                let t = self.type_of(a)?;
                self.konst_addr(a)? + self.field(&t, f)?.0 as i64
            }
            Expr::Index(a, i) => {
                let t = decay(self.type_of(a)?);
                let elem = pointee(&t).ok_or("indexing a non-pointer")?;
                let base = match &**a {
                    Expr::Ident(_) => self.konst_addr(a)?,
                    other => self.konst(other)?,
                };
                base + self.konst(i)? * self.size_of(self.u, &elem) as i64
            }
            _ => return Err("address is not constant".into()),
        })
    }

    fn symbols(&self) -> Symbols {
        let mut s = Symbols::default();
        let mut seen: HashMap<&str, usize> = HashMap::new();
        for g in &self.globs {
            *seen.entry(&g.name).or_default() += 1;
        }
        for g in &self.globs {
            if seen[g.name.as_str()] == 1 {
                s.globals.insert(g.name.clone(), g.addr);
            }
        }
        for (u, unit) in self.units.iter().enumerate() {
            let add = |name: &str, t: &Type, s: &mut Symbols| {
                s.sizes.insert(name.to_string(), self.size_of(u, t) as i64);
                if let Type::Struct(i) = t {
                    for f in &unit.structs[*i].fields {
                        s.fields
                            .insert((name.to_string(), f.name.clone()), f.offset as i64);
                    }
                }
            };
            for (i, st) in unit.structs.iter().enumerate() {
                if let (Some(tag), true) = (&st.tag, st.complete) {
                    add(tag, &Type::Struct(i), &mut s);
                }
            }
            for (name, t) in &unit.typedefs {
                if !matches!(t, Type::Struct(i) if !unit.structs[*i].complete) {
                    add(name, t, &mut s);
                }
            }
        }
        for f in &self.funcs {
            if f.def.is_some() || f.builtin.is_some() {
                s.funcs.entry(f.name.clone()).or_insert(f.fun);
            }
        }
        s
    }
}

// ---------------------------------------------------------------- analysis

fn has_float(e: &Expr) -> bool {
    match e {
        Expr::Float(_) => true,
        Expr::Unary(_, a) | Expr::Cast(_, a) => has_float(a),
        Expr::Binary(_, a, b) => has_float(a) || has_float(b),
        _ => false,
    }
}

/// Runs of set bits (low bit, high bit exclusive) of a 32-bit mask, or of
/// its complement when negative; masks beyond 32 bits count as many runs.
fn runs(k: i64) -> Vec<(u32, u32)> {
    let mut m = if k < 0 { !k } else { k } as u64;
    if m >> 32 != 0 {
        return vec![(0, 0); 64];
    }
    let mut out = vec![];
    while m != 0 {
        let l = m.trailing_zeros();
        let h = l + (m >> l).trailing_ones();
        out.push((l, h));
        m &= u64::MAX << h;
    }
    out
}

/// Value of a constant arithmetic expression involving floats.
fn konst_f(e: &Expr) -> Option<f64> {
    match e {
        Expr::Float(v) => Some(*v),
        Expr::Int(v, _) => Some(*v as f64),
        Expr::Unary(UnOp::Neg, a) => konst_f(a).map(|v| -v),
        Expr::Unary(UnOp::Plus, a) => konst_f(a),
        Expr::Binary(op, a, b) => {
            let (x, y) = (konst_f(a)?, konst_f(b)?);
            let (i, j) = (x as i64, y as i64);
            match op {
                BinOp::Add => Some(x + y),
                BinOp::Sub => Some(x - y),
                BinOp::Mul => Some(x * y),
                BinOp::Div if has_float(a) || has_float(b) => Some(x / y),
                // integer subexpressions such as FRACUNIT = (1<<FRACBITS)
                BinOp::Div => i.checked_div(j).map(|v| v as f64),
                BinOp::Mod => i.checked_rem(j).map(|v| v as f64),
                BinOp::Shl => Some((i << j) as f64),
                BinOp::Shr => Some((i >> j) as f64),
                _ => None,
            }
        }
        Expr::Cast(t, a) => konst_f(a).map(|v| if int_info(t).is_some() { v.trunc() } else { v }),
        _ => None,
    }
}

/// Which nodes lie on a cycle (Tarjan's strongly connected components).
fn cyclic(edges: &[BTreeSet<usize>]) -> Vec<bool> {
    struct T<'e> {
        e: &'e [BTreeSet<usize>],
        index: Vec<Option<usize>>,
        low: Vec<usize>,
        on: Vec<bool>,
        stack: Vec<usize>,
        next: usize,
        rec: Vec<bool>,
    }
    fn visit(v: usize, t: &mut T) {
        t.index[v] = Some(t.next);
        t.low[v] = t.next;
        t.next += 1;
        t.stack.push(v);
        t.on[v] = true;
        for w in t.e[v].clone() {
            match t.index[w] {
                None => {
                    visit(w, t);
                    t.low[v] = t.low[v].min(t.low[w]);
                }
                Some(iw) if t.on[w] => t.low[v] = t.low[v].min(iw),
                _ => {}
            }
        }
        if Some(t.low[v]) == t.index[v] {
            let mut comp = vec![];
            loop {
                let w = t.stack.pop().unwrap();
                t.on[w] = false;
                comp.push(w);
                if w == v {
                    break;
                }
            }
            let cyc = comp.len() > 1 || t.e[v].contains(&v);
            for w in comp {
                t.rec[w] = cyc;
            }
        }
    }
    let n = edges.len();
    let mut t = T {
        e: edges,
        index: vec![None; n],
        low: vec![0; n],
        on: vec![false; n],
        stack: vec![],
        next: 0,
        rec: vec![false; n],
    };
    for v in 0..n {
        if t.index[v].is_none() {
            visit(v, &mut t);
        }
    }
    t.rec
}

struct Scan<'s> {
    scope: &'s Scope,
    ids: &'s HashMap<String, usize>,
    calls: BTreeSet<usize>,
    taken: &'s mut BTreeSet<usize>,
    indirect: bool,
}

impl Scan<'_> {
    fn func(&self, n: &str) -> Option<usize> {
        if self.scope.vars.contains_key(n) {
            return None;
        }
        self.scope
            .funcs
            .get(n)
            .map(|f| f.0)
            .or_else(|| self.ids.get(n).copied())
    }

    fn stmt(&mut self, s: &Stmt) {
        match &s.kind {
            StmtKind::Expr(x) => self.expr(x),
            StmtKind::Decl(ds) => {
                for d in ds {
                    if let Some(i) = &d.init {
                        self.init(i);
                    }
                }
            }
            StmtKind::Block(ss) => ss.iter().for_each(|s| self.stmt(s)),
            StmtKind::If(c, a, b) => {
                self.expr(c);
                self.stmt(a);
                if let Some(b) = b {
                    self.stmt(b);
                }
            }
            StmtKind::While(c, b) | StmtKind::DoWhile(b, c) | StmtKind::Switch(c, b) => {
                self.expr(c);
                self.stmt(b);
            }
            StmtKind::For(i, c, n, b) => {
                if let Some(i) = i {
                    self.stmt(i);
                }
                for x in [c, n].into_iter().flatten() {
                    self.expr(x);
                }
                self.stmt(b);
            }
            StmtKind::Case(_, b) | StmtKind::Default(b) | StmtKind::Label(_, b) => self.stmt(b),
            StmtKind::Return(Some(x)) => self.expr(x),
            _ => {}
        }
    }

    fn init(&mut self, i: &Init) {
        match i {
            Init::Expr(x) => self.expr(x),
            Init::List(l) => l.iter().for_each(|i| self.init(i)),
        }
    }

    fn expr(&mut self, x: &Expr) {
        match x {
            Expr::Ident(n) => {
                if let Some(f) = self.func(n) {
                    self.taken.insert(f);
                }
            }
            Expr::Call(f, args) => {
                match &**f {
                    Expr::Ident(n) if self.func(n).is_some() => {
                        let f = self.func(n).unwrap();
                        self.calls.insert(f);
                    }
                    other => {
                        self.indirect = true;
                        self.expr(other);
                    }
                }
                args.iter().for_each(|a| self.expr(a));
            }
            Expr::Unary(_, a)
            | Expr::Member(a, _)
            | Expr::Arrow(a, _)
            | Expr::Cast(_, a)
            | Expr::SizeofExpr(a) => self.expr(a),
            Expr::Binary(_, a, b) | Expr::Assign(_, a, b) | Expr::Index(a, b) => {
                self.expr(a);
                self.expr(b);
            }
            Expr::Cond(a, b, c) => {
                self.expr(a);
                self.expr(b);
                self.expr(c);
            }
            _ => {}
        }
    }
}
