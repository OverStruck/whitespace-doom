//! Structured Whitespace generator: write programs as Rust expressions and
//! closures, get Whitespace instructions out. Every variable lives at a fixed
//! heap address (functions must not recurse), comparisons and `& | !` yield
//! 0/1 with short-circuit evaluation, `/` and `%` floor.

use crate::vm::Ins;
use std::ops;

#[derive(Clone, Debug)]
pub enum E {
    Num(i64),
    Load(Box<E>),
    Bin(Ins, Box<E>, Box<E>),
    Cmp(Cmp, Box<E>, Box<E>),
    And(Box<E>, Box<E>),
    Or(Box<E>, Box<E>),
    Not(Box<E>),
    Call(i64, Vec<E>),
    Abs(Box<E>),
    Min(Box<E>, Box<E>),
    Max(Box<E>, Box<E>),
    Getc,
}

#[derive(Clone, Copy, Debug)]
pub enum Cmp {
    Lt,
    Le,
    Gt,
    Ge,
    Eq,
    Ne,
}

/// Scalar variable: a heap address.
#[derive(Clone, Copy, Debug)]
pub struct V(pub i64);

/// Array: base heap address.
#[derive(Clone, Copy, Debug)]
pub struct A(pub i64);

/// Subroutine: label and arity.
#[derive(Clone, Copy, Debug)]
pub struct Fun(i64, usize);

impl From<i64> for E {
    fn from(n: i64) -> E {
        E::Num(n)
    }
}

impl From<char> for E {
    fn from(c: char) -> E {
        E::Num(c as i64)
    }
}

impl From<V> for E {
    fn from(v: V) -> E {
        E::Load(Box::new(E::Num(v.0)))
    }
}

impl A {
    pub fn at(self, i: impl Into<E>) -> E {
        E::Load(Box::new(E::Num(self.0) + i))
    }
}

impl Fun {
    pub fn call<const K: usize>(self, args: [E; K]) -> E {
        assert_eq!(K, self.1, "wrong number of arguments");
        E::Call(self.0, args.into())
    }

    /// Call with a runtime-sized argument list (C calls, variadic builtins).
    pub fn call_vec(self, args: Vec<E>) -> E {
        E::Call(self.0, args)
    }

    /// The label, which is also the function's pointer value.
    pub fn label(self) -> i64 {
        self.0
    }

    pub fn arity(self) -> usize {
        self.1
    }
}

impl E {
    fn bin(op: Ins, a: E, b: E) -> E {
        match (op, &a, &b) {
            (Ins::Add, E::Num(x), E::Num(y)) => E::Num(x + y),
            (Ins::Sub, E::Num(x), E::Num(y)) => E::Num(x - y),
            (Ins::Mul, E::Num(x), E::Num(y)) => E::Num(x * y),
            _ => E::Bin(op, Box::new(a), Box::new(b)),
        }
    }
}

macro_rules! binop {
    ($tr:ident, $f:ident, $op:expr) => {
        impl<T: Into<E>> ops::$tr<T> for E {
            type Output = E;
            fn $f(self, r: T) -> E {
                E::bin($op, self, r.into())
            }
        }
        impl<T: Into<E>> ops::$tr<T> for V {
            type Output = E;
            fn $f(self, r: T) -> E {
                E::bin($op, self.into(), r.into())
            }
        }
        impl ops::$tr<E> for i64 {
            type Output = E;
            fn $f(self, r: E) -> E {
                E::bin($op, E::Num(self), r)
            }
        }
        impl ops::$tr<V> for i64 {
            type Output = E;
            fn $f(self, r: V) -> E {
                E::bin($op, E::Num(self), r.into())
            }
        }
    };
}
binop!(Add, add, Ins::Add);
binop!(Sub, sub, Ins::Sub);
binop!(Mul, mul, Ins::Mul);
binop!(Div, div, Ins::Div);
binop!(Rem, rem, Ins::Mod);

impl ops::Neg for E {
    type Output = E;
    fn neg(self) -> E {
        0 - self
    }
}

impl ops::Neg for V {
    type Output = E;
    fn neg(self) -> E {
        0 - self
    }
}

impl<T: Into<E>> ops::BitAnd<T> for E {
    type Output = E;
    fn bitand(self, r: T) -> E {
        E::And(Box::new(self), Box::new(r.into()))
    }
}

impl<T: Into<E>> ops::BitOr<T> for E {
    type Output = E;
    fn bitor(self, r: T) -> E {
        E::Or(Box::new(self), Box::new(r.into()))
    }
}

impl ops::Not for E {
    type Output = E;
    fn not(self) -> E {
        E::Not(Box::new(self))
    }
}

impl ops::Not for V {
    type Output = E;
    fn not(self) -> E {
        E::Not(Box::new(self.into()))
    }
}

/// Comparisons (methods, since Rust's `<` must return bool).
pub trait Ex: Into<E> + Sized {
    fn lt(self, r: impl Into<E>) -> E {
        cmp(Cmp::Lt, self, r)
    }
    fn le(self, r: impl Into<E>) -> E {
        cmp(Cmp::Le, self, r)
    }
    fn gt(self, r: impl Into<E>) -> E {
        cmp(Cmp::Gt, self, r)
    }
    fn ge(self, r: impl Into<E>) -> E {
        cmp(Cmp::Ge, self, r)
    }
    fn eq(self, r: impl Into<E>) -> E {
        cmp(Cmp::Eq, self, r)
    }
    fn ne(self, r: impl Into<E>) -> E {
        cmp(Cmp::Ne, self, r)
    }
}
impl Ex for E {}
impl Ex for V {}

fn cmp(c: Cmp, a: impl Into<E>, b: impl Into<E>) -> E {
    E::Cmp(c, Box::new(a.into()), Box::new(b.into()))
}

pub fn abs(a: impl Into<E>) -> E {
    E::Abs(Box::new(a.into()))
}

pub fn min(a: impl Into<E>, b: impl Into<E>) -> E {
    E::Min(Box::new(a.into()), Box::new(b.into()))
}

pub fn max(a: impl Into<E>, b: impl Into<E>) -> E {
    E::Max(Box::new(a.into()), Box::new(b.into()))
}

/// Read one input character (-1 at end of input on most interpreters).
pub fn getc() -> E {
    E::Getc
}

/// Program under construction.
pub struct Prog {
    code: Vec<Ins>,
    funcs: Vec<Ins>,
    init: Vec<(i64, i64)>,
    heap: i64,
    labels: i64,
    loops: Vec<(i64, i64)>,
    scratch: i64,
}

impl Default for Prog {
    fn default() -> Self {
        Self::new()
    }
}

impl Prog {
    pub fn new() -> Prog {
        let mut p = Prog {
            code: vec![],
            funcs: vec![],
            init: vec![],
            heap: 0,
            labels: 0,
            loops: vec![],
            scratch: 0,
        };
        p.scratch = p.var().0;
        p
    }

    fn emit(&mut self, i: Ins) {
        self.code.push(i);
    }

    fn label(&mut self) -> i64 {
        self.labels += 1;
        self.labels
    }

    /// Emit one instruction into the current function (code generators).
    pub fn raw(&mut self, i: Ins) {
        self.emit(i);
    }

    pub fn new_label(&mut self) -> i64 {
        self.label()
    }

    /// Evaluate `e`, leaving its value on the stack.
    pub fn push(&mut self, e: impl Into<E>) {
        self.expr(e.into());
    }

    /// Reserve `n` heap cells, returning the first address.
    pub fn alloc(&mut self, n: i64) -> i64 {
        self.arr(n).0
    }

    /// Initial contents for cells starting at `addr` (already allocated).
    pub fn set_init(&mut self, addr: i64, vals: &[i64]) {
        self.init
            .extend(vals.iter().enumerate().map(|(i, &v)| (addr + i as i64, v)));
    }

    pub fn var(&mut self) -> V {
        self.heap += 1;
        V(self.heap - 1)
    }

    pub fn var_init(&mut self, v: i64) -> V {
        let x = self.var();
        self.init.push((x.0, v));
        x
    }

    pub fn arr(&mut self, len: i64) -> A {
        let a = A(self.heap);
        self.heap += len;
        a
    }

    pub fn arr_init(&mut self, vals: &[i64]) -> A {
        let a = self.arr(vals.len() as i64);
        self.init
            .extend(vals.iter().enumerate().map(|(i, &v)| (a.0 + i as i64, v)));
        a
    }

    pub fn func(&mut self, arity: usize) -> Fun {
        Fun(self.label(), arity)
    }

    /// Define `f`; arguments are stored into `params` on entry. Falling off
    /// the end returns 0.
    pub fn def(&mut self, f: Fun, params: &[V], body: impl FnOnce(&mut Prog)) {
        assert_eq!(params.len(), f.1, "wrong number of parameters");
        let main = std::mem::take(&mut self.code);
        self.emit(Ins::Label(f.0));
        for p in params.iter().rev() {
            self.emit(Ins::Push(p.0));
            self.emit(Ins::Swap);
            self.emit(Ins::Store);
        }
        body(self);
        self.emit(Ins::Push(0));
        self.emit(Ins::Ret);
        let f = std::mem::replace(&mut self.code, main);
        self.funcs.extend(f);
    }

    /// Define `f` with a body that handles its own arguments and returns
    /// (code generators); only the entry label is emitted here.
    pub fn def_raw(&mut self, f: Fun, body: impl FnOnce(&mut Prog)) {
        let main = self.begin_raw(f);
        body(self);
        self.end_raw(main);
    }

    /// Start emitting the body of `f` (entry label only); returns the code
    /// being interrupted, to hand back to `end_raw`.
    pub fn begin_raw(&mut self, f: Fun) -> Vec<Ins> {
        let main = std::mem::take(&mut self.code);
        self.emit(Ins::Label(f.0));
        main
    }

    pub fn end_raw(&mut self, main: Vec<Ins>) {
        let f = std::mem::replace(&mut self.code, main);
        self.funcs.extend(f);
    }

    /// Define a variadic `f`: on entry the variadic arguments are stored in
    /// `args[0..count]` (in call order) and the fixed ones in `fixed`.
    pub fn def_variadic(
        &mut self,
        f: Fun,
        fixed: &[V],
        args: A,
        count: V,
        body: impl FnOnce(&mut Prog),
    ) {
        assert_eq!(fixed.len(), f.1, "wrong number of parameters");
        let i = self.var();
        self.def_raw(f, |p| {
            p.emit(Ins::Push(count.0));
            p.emit(Ins::Swap);
            p.emit(Ins::Store);
            p.set(i, count);
            p.while_(i.gt(0), |p| {
                p.inc(i, -1);
                p.push(args.0 + i);
                p.emit(Ins::Swap);
                p.emit(Ins::Store);
            });
            for v in fixed.iter().rev() {
                p.emit(Ins::Push(v.0));
                p.emit(Ins::Swap);
                p.emit(Ins::Store);
            }
            body(p);
            p.emit(Ins::Push(0));
            p.emit(Ins::Ret);
        });
    }

    fn lvalue(e: E) -> E {
        match e {
            E::Load(addr) => *addr,
            e => panic!("not assignable: {e:?}"),
        }
    }

    pub fn set(&mut self, target: impl Into<E>, val: impl Into<E>) {
        let addr = Self::lvalue(target.into());
        self.expr(addr);
        self.expr(val.into());
        self.emit(Ins::Store);
    }

    /// `target += delta`
    pub fn inc(&mut self, target: impl Into<E>, delta: impl Into<E>) {
        let addr = Self::lvalue(target.into());
        self.expr(addr);
        self.emit(Ins::Dup);
        self.emit(Ins::Load);
        self.expr(delta.into());
        self.emit(Ins::Add);
        self.emit(Ins::Store);
    }

    pub fn if_(&mut self, c: impl Into<E>, then: impl FnOnce(&mut Prog)) {
        let end = self.label();
        self.branch(c.into(), end, false);
        then(self);
        self.emit(Ins::Label(end));
    }

    pub fn if_else(
        &mut self,
        c: impl Into<E>,
        then: impl FnOnce(&mut Prog),
        els: impl FnOnce(&mut Prog),
    ) {
        let (other, end) = (self.label(), self.label());
        self.branch(c.into(), other, false);
        then(self);
        self.emit(Ins::Jmp(end));
        self.emit(Ins::Label(other));
        els(self);
        self.emit(Ins::Label(end));
    }

    pub fn while_(&mut self, c: impl Into<E>, body: impl FnOnce(&mut Prog)) {
        let (top, test, end) = (self.label(), self.label(), self.label());
        self.emit(Ins::Jmp(test));
        self.emit(Ins::Label(top));
        self.loops.push((test, end));
        body(self);
        self.loops.pop();
        self.emit(Ins::Label(test));
        self.branch(c.into(), top, true);
        self.emit(Ins::Label(end));
    }

    /// `for v in from..to` (bound evaluated once).
    pub fn for_(
        &mut self,
        v: V,
        from: impl Into<E>,
        to: impl Into<E>,
        body: impl FnOnce(&mut Prog),
    ) {
        let to = match to.into() {
            E::Num(n) => E::Num(n),
            e => {
                let t = self.var();
                self.set(t, e);
                t.into()
            }
        };
        self.set(v, from);
        let (top, next, test, end) = (self.label(), self.label(), self.label(), self.label());
        self.emit(Ins::Jmp(test));
        self.emit(Ins::Label(top));
        self.loops.push((next, end));
        body(self);
        self.loops.pop();
        self.emit(Ins::Label(next));
        self.inc(v, 1);
        self.emit(Ins::Label(test));
        self.branch(v.lt(to), top, true);
        self.emit(Ins::Label(end));
    }

    pub fn brk(&mut self) {
        let l = self.loops.last().expect("break outside a loop").1;
        self.emit(Ins::Jmp(l));
    }

    pub fn cont(&mut self) {
        let l = self.loops.last().expect("continue outside a loop").0;
        self.emit(Ins::Jmp(l));
    }

    pub fn ret(&mut self, e: impl Into<E>) {
        self.expr(e.into());
        self.emit(Ins::Ret);
    }

    /// Call `f` as a statement, discarding its result.
    pub fn call<const K: usize>(&mut self, f: Fun, args: [E; K]) {
        self.eval(f.call(args));
    }

    /// Evaluate `e` for its side effects.
    pub fn eval(&mut self, e: impl Into<E>) {
        self.expr(e.into());
        self.emit(Ins::Drop);
    }

    pub fn putc(&mut self, c: impl Into<E>) {
        self.expr(c.into());
        self.emit(Ins::PutC);
    }

    pub fn putn(&mut self, n: impl Into<E>) {
        self.expr(n.into());
        self.emit(Ins::PutN);
    }

    pub fn puts(&mut self, s: &str) {
        for ch in s.chars() {
            self.putc(ch);
        }
    }

    pub fn exit(&mut self) {
        self.emit(Ins::End);
    }

    fn expr(&mut self, e: E) {
        use Ins::*;
        match e {
            E::Num(n) => self.emit(Push(n)),
            E::Load(a) => {
                self.expr(*a);
                self.emit(Load);
            }
            E::Bin(op, a, b) => {
                self.expr(*a);
                self.expr(*b);
                self.emit(op);
            }
            E::Call(l, args) => {
                for a in args {
                    self.expr(a);
                }
                self.emit(Call(l));
            }
            E::Abs(a) => {
                let (neg, end) = (self.label(), self.label());
                self.expr(*a);
                self.emit(Dup);
                self.emit(Jn(neg));
                self.emit(Jmp(end));
                self.emit(Label(neg));
                self.emit(Push(-1));
                self.emit(Mul);
                self.emit(Label(end));
            }
            E::Min(a, b) => self.pick(*a, *b, true),
            E::Max(a, b) => self.pick(*a, *b, false),
            E::Getc => {
                self.emit(Push(self.scratch));
                self.emit(GetC);
                self.emit(Push(self.scratch));
                self.emit(Load);
            }
            cond => {
                let (no, end) = (self.label(), self.label());
                self.branch(cond, no, false);
                self.emit(Push(1));
                self.emit(Jmp(end));
                self.emit(Label(no));
                self.emit(Push(0));
                self.emit(Label(end));
            }
        }
    }

    /// min/max of [a b]: keep a when a-b < 0 (min) or b-a < 0 (max), else b.
    fn pick(&mut self, a: E, b: E, is_min: bool) {
        use Ins::*;
        let keep = self.label();
        self.expr(a);
        self.expr(b);
        let (x, y) = if is_min { (1, 1) } else { (0, 2) };
        self.emit(Copy(x));
        self.emit(Copy(y));
        self.emit(Sub);
        self.emit(Jn(keep));
        self.emit(Swap);
        self.emit(Label(keep));
        self.emit(Drop);
    }

    /// Jump to `target` when the truth of `e` equals `when`, else fall through.
    fn branch(&mut self, e: E, target: i64, when: bool) {
        match e {
            E::Num(n) => {
                if (n != 0) == when {
                    self.emit(Ins::Jmp(target));
                }
            }
            E::Not(a) => self.branch(*a, target, !when),
            E::And(a, b) if !when => {
                self.branch(*a, target, false);
                self.branch(*b, target, false);
            }
            E::Or(a, b) if when => {
                self.branch(*a, target, true);
                self.branch(*b, target, true);
            }
            E::And(a, b) | E::Or(a, b) => {
                let skip = self.label();
                self.branch(*a, skip, !when);
                self.branch(*b, target, when);
                self.emit(Ins::Label(skip));
            }
            E::Cmp(c, a, b) => {
                use Cmp::*;
                // reduce to a sign/zero test of a-b
                let (a, b, zero, negate) = match c {
                    Lt => (a, b, false, false),
                    Gt => (b, a, false, false),
                    Le => (b, a, false, true),
                    Ge => (a, b, false, true),
                    Eq => (a, b, true, false),
                    Ne => (a, b, true, true),
                };
                self.expr(*a);
                if !matches!(*b, E::Num(0)) {
                    self.expr(*b);
                    self.emit(Ins::Sub);
                }
                self.test(zero, target, when != negate);
            }
            e => {
                self.expr(e);
                self.test(true, target, !when);
            }
        }
    }

    fn test(&mut self, zero: bool, target: i64, when: bool) {
        let jump = |l| if zero { Ins::Jz(l) } else { Ins::Jn(l) };
        if when {
            self.emit(jump(target));
        } else {
            let skip = self.label();
            self.emit(jump(skip));
            self.emit(Ins::Jmp(target));
            self.emit(Ins::Label(skip));
        }
    }

    /// Code storing the initial heap: short runs as plain stores, long runs
    /// packed several values per pushed number and unpacked by a loop.
    fn init_code(&mut self) -> Vec<Ins> {
        use Ins::*;
        let cells: Vec<(i64, i64)> = std::mem::take(&mut self.init)
            .into_iter()
            .collect::<std::collections::BTreeMap<_, _>>() // later writes win
            .into_iter()
            .collect();
        let (ptr, cnt) = (self.var().0, self.var().0);
        let mut out = vec![];
        // value width class of a cell: 8-bit, 16-bit, 32-bit, wider
        let class = |v: i64| match v {
            0..=255 => 0,
            -32768..=32767 => 1,
            -2147483648..=2147483647 => 2,
            _ => 3,
        };
        let mut i = 0;
        while i < cells.len() {
            // a run of consecutive addresses whose values share a width class
            let mut j = i + 1;
            while j < cells.len()
                && cells[j].0 == cells[j - 1].0 + 1
                && class(cells[j].1) == class(cells[i].1)
            {
                j += 1;
            }
            let run = &cells[i..j];
            i = j;
            let lo = run.iter().map(|c| c.1).min().unwrap();
            let hi = run.iter().map(|c| c.1).max().unwrap();
            // (bits per value, values per number, offset making values non-negative)
            let (bits, per, off) = if lo >= 0 && hi < 256 {
                (8, 7, 0)
            } else if lo >= -32768 && hi < 32768 {
                (16, 3, 32768)
            } else if lo >= -(1 << 31) && hi < (1 << 31) {
                (32, 1, 1 << 31)
            } else {
                (0, 1, 0)
            };
            let words = run.len() / per;
            let mut packed = if bits > 0 && words >= 2 {
                words * per
            } else {
                0
            };
            if packed > 0 {
                // keep plain stores when they are shorter (sparse data, tiny runs)
                let b = 1i64 << bits;
                let data: usize = run[..packed]
                    .chunks(per)
                    .map(|w| push_len(w.iter().rev().fold(0, |acc, &(_, v)| acc * b + v + off)))
                    .sum();
                let plain: usize = run[..packed]
                    .iter()
                    .filter(|c| c.1 != 0)
                    .map(|&(a, v)| push_len(a) + push_len(v) + 3)
                    .sum();
                if data + 60 * per + 150 >= plain {
                    packed = 0;
                }
            }
            if packed > 0 {
                let b = 1i64 << bits;
                for w in run[..packed].chunks(per).rev() {
                    out.push(Push(
                        w.iter().rev().fold(0, |acc, &(_, v)| acc * b + v + off),
                    ));
                }
                let (top, done) = (self.label(), self.label());
                out.extend([
                    Push(ptr),
                    Push(run[0].0),
                    Store,
                    Push(cnt),
                    Push(words as i64),
                    Store,
                    Label(top),
                ]);
                for _ in 0..per {
                    out.extend([Push(ptr), Load, Copy(1), Push(b), Mod]);
                    if off != 0 {
                        out.extend([Push(off), Sub]);
                    }
                    out.extend([
                        Store,
                        Push(b),
                        Div,
                        Push(ptr),
                        Push(ptr),
                        Load,
                        Push(1),
                        Add,
                        Store,
                    ]);
                }
                out.extend([
                    Drop,
                    Push(cnt),
                    Push(cnt),
                    Load,
                    Push(1),
                    Sub,
                    Store,
                    Push(cnt),
                    Load,
                    Jz(done),
                    Jmp(top),
                    Label(done),
                ]);
            }
            for &(a, v) in &run[packed..] {
                if v != 0 {
                    out.extend([Push(a), Push(v), Store]);
                }
            }
        }
        out
    }

    /// Final instruction list: heap setup, main code, end, subroutines.
    pub fn finish(mut self) -> Vec<Ins> {
        use Ins::*;
        let init = self.init_code();
        let (top, done) = (self.label(), self.label());
        // zero every cell first so loads never touch unset heap on any interpreter
        let mut out = vec![
            Push(self.heap),
            Label(top),
            Push(1),
            Sub,
            Dup,
            Push(0),
            Store,
            Dup,
            Jz(done),
            Jmp(top),
            Label(done),
            Drop,
        ];
        out.extend(init);
        out.append(&mut self.code);
        out.push(End);
        out.append(&mut self.funcs);
        out
    }
}

/// Characters in the Whitespace encoding of `Push(n)`.
fn push_len(n: i64) -> usize {
    4 + (64 - n.unsigned_abs().leading_zeros()).max(1) as usize
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vm;

    /// Compile, round-trip through Whitespace text, run.
    fn run(p: Prog, input: &[u8]) -> String {
        let code = vm::parse(&vm::encode(&p.finish())).unwrap();
        let mut it = input.iter();
        let mut out = Vec::new();
        vm::run(
            &code,
            &mut |_: &mut Vec<u8>| it.next().map_or(-1, |&b| b as i64),
            &mut out,
        )
        .unwrap();
        String::from_utf8(out).unwrap()
    }

    #[test]
    fn arithmetic_comparisons_and_logic_match_rust() {
        let mut p = Prog::new();
        let (a, b, once) = (p.var(), p.var(), p.var());
        let mut want = String::new();
        for (x, y) in [(7, 2), (-7, 2), (7, -2), (-7, -2), (0, 3), (5, 5), (-3, 0)] {
            p.set(a, x);
            p.set(b, y);
            let mut vals: Vec<(E, i64)> = vec![
                (a + b, x + y),
                (a - b, x - y),
                (a * b, x * y),
                (-a, -x),
                (min(a, b), x.min(y)),
                (max(a, b), x.max(y)),
                (abs(a), x.abs()),
            ];
            if y != 0 {
                let q = (x as f64 / y as f64).floor() as i64;
                vals.extend([(a / b, q), (a % b, x - y * q)]);
            }
            let conds: Vec<(E, bool)> = vec![
                (a.lt(b), x < y),
                (a.le(b), x <= y),
                (a.gt(b), x > y),
                (a.ge(b), x >= y),
                (a.eq(b), x == y),
                (a.ne(b), x != y),
                (a.gt(0) & b.gt(0), x > 0 && y > 0),
                (a.gt(0) | b.gt(0), x > 0 || y > 0),
                (!(a.lt(b) | a.eq(0)), !(x < y || x == 0)),
                (a.lt(b) & (b.ne(0) | a.ge(0)), x < y && (y != 0 || x >= 0)),
            ];
            for (e, v) in vals {
                p.putn(e);
                p.putc(' ');
                want += &format!("{v} ");
            }
            // each condition as a value, as an if/else test and as a loop test
            for (c, v) in conds {
                p.putn(c.clone());
                p.if_else(c.clone(), |p| p.putc('T'), |p| p.putc('F'));
                p.set(once, 1);
                p.while_(c & once.eq(1), |p| {
                    p.putc('W');
                    p.set(once, 0);
                });
                p.putc(' ');
                want += if v { "1TW " } else { "0F " };
            }
            // a plain number as a condition: nonzero is true
            p.if_else(a + 0, |p| p.putc('T'), |p| p.putc('F'));
            want += if x != 0 { "T" } else { "F" };
        }
        assert_eq!(run(p, b""), want);
    }

    #[test]
    fn input_wait_pauses_and_retries_the_read() {
        let mut p = Prog::new();
        let c = p.var();
        p.putc('>');
        p.set(c, getc());
        p.putc(c);
        let code = vm::parse(&vm::encode(&p.finish())).unwrap();
        let mut m = vm::Machine::default();
        let (mut out, mut calls) = (Vec::new(), 0);
        let mut input = |_: &mut Vec<u8>| {
            calls += 1;
            if calls == 1 {
                vm::WAIT
            } else {
                'x' as i64
            }
        };
        assert_eq!(m.run(&code, &mut input, &mut out, u64::MAX), Ok(false));
        assert_eq!(out, b">");
        assert_eq!(m.run(&code, &mut input, &mut out, u64::MAX), Ok(true));
        assert_eq!(out, b">x");
    }

    #[test]
    fn initial_heap_survives_packing() {
        // byte runs, 16-bit headers, 32-bit and wide values, zeros, gaps
        let mut seed = 7u64;
        let mut next = || {
            seed = seed
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            (seed >> 33) as i64
        };
        let mut p = Prog::new();
        let mut want = vec![];
        for block in 0..40 {
            let len = 1 + next() % 300;
            let vals: Vec<i64> = (0..len)
                .map(|k| match (block + k / 50) % 5 {
                    0 => next() % 256,
                    1 => next() % 65536 - 32768,
                    2 => next() - (1 << 30),
                    3 => (next() << 20) - (1 << 50),
                    _ => 0,
                })
                .collect();
            let a = p.arr_init(&vals);
            p.arr(next() % 3);
            want.push((a, vals));
        }
        let out = p.var();
        for (a, vals) in &want {
            for (k, v) in vals.iter().enumerate() {
                // flag any cell that differs from what was stored
                p.if_(a.at(k as i64).ne(*v), |p| p.set(out, 1));
            }
        }
        p.putn(out);
        assert_eq!(run(p, b""), "0");
    }

    #[test]
    fn loops_functions_arrays_and_io() {
        let mut p = Prog::new();
        let isprime = p.func(1);
        let (n, d) = (p.var(), p.var());
        p.def(isprime, &[n], |p| {
            p.if_(n.lt(2), |p| p.ret(0));
            p.for_(d, 2, n, |p| {
                p.if_((d * d).gt(n), |p| p.brk());
                p.if_((n % d).eq(0), |p| p.ret(0));
            });
            p.ret(1);
        });
        let digits = p.func(3);
        let (x, y, z) = (p.var(), p.var(), p.var());
        p.def(digits, &[x, y, z], |p| p.ret(x * 100 + y * 10 + z));

        let (i, sum, c) = (p.var(), p.var(), p.var());
        p.for_(i, 0, 50, |p| {
            p.if_(isprime.call([i.into()]).eq(0), |p| p.cont());
            p.inc(sum, i);
        });
        p.putn(sum);
        p.putc(' ');
        p.putn(digits.call([
            1.into(),
            digits.call([0.into(), 0.into(), 2.into()]),
            3.into(),
        ]));
        p.putc(' ');
        let sq = p.arr(10);
        p.for_(i, 0, 10, |p| p.set(sq.at(i), i * i));
        p.inc(sq.at(3), 1);
        p.putn(sq.at(3) + sq.at(9));
        p.putc(' ');
        let k = p.var_init(-5);
        let t = p.arr_init(&[4, 0, 6]);
        p.putn(k + t.at(0) + t.at(1) + t.at(2));
        p.putc(' ');
        p.set(c, getc());
        p.while_(c.ge(0) & c.ne('\n'), |p| {
            p.if_(c.ge('a') & c.le('z'), |p| p.inc(c, -32));
            p.putc(c);
            p.set(c, getc());
        });
        p.puts("▀");
        assert_eq!(run(p, b"hi!\nignored"), "328 123 91 5 HI!▀");
    }
}
