//! Whitespace 0.3: encoder, parser and interpreter.
//! Integers are i64 (overflow is an error); `/` and `%` floor like the reference.

use std::collections::HashMap;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ins {
    Push(i64),
    Dup,
    Copy(i64),
    Swap,
    Drop,
    Slide(i64),
    Add,
    Sub,
    Mul,
    Div,
    Mod,
    Store,
    Load,
    Label(i64),
    Call(i64),
    Jmp(i64),
    Jz(i64),
    Jn(i64),
    Ret,
    End,
    PutC,
    PutN,
    GetC,
    GetN,
    /// Debug marker (not Whitespace): dropped by `encode`, a no-op if run.
    Note(i64),
}

/// For each instruction `parse(encode(code))` yields, the latest `Note`
/// before it (-1 if none): maps VM error positions back to their source.
pub fn note_map(code: &[Ins]) -> Vec<i64> {
    let mut note = -1;
    let mut map = Vec::new();
    for &ins in code {
        match ins {
            Ins::Note(n) => note = n,
            Ins::Label(_) => {}
            _ => map.push(note),
        }
    }
    map
}

fn bits(s: &mut String, n: u64) {
    s.extend(
        format!("{n:b}")
            .chars()
            .map(|c| if c == '1' { '\t' } else { ' ' }),
    );
}

/// Assemble instructions (labels are positive ids) into Whitespace source.
pub fn encode(code: &[Ins]) -> String {
    use Ins::*;
    let mut s = String::new();
    for &ins in code {
        let (cmd, num, label) = match ins {
            Note(_) => continue,
            Push(n) => ("  ", Some(n), None),
            Copy(n) => (" \t ", Some(n), None),
            Slide(n) => (" \t\n", Some(n), None),
            Dup => (" \n ", None, None),
            Swap => (" \n\t", None, None),
            Drop => (" \n\n", None, None),
            Add => ("\t   ", None, None),
            Sub => ("\t  \t", None, None),
            Mul => ("\t  \n", None, None),
            Div => ("\t \t ", None, None),
            Mod => ("\t \t\t", None, None),
            Store => ("\t\t ", None, None),
            Load => ("\t\t\t", None, None),
            Label(l) => ("\n  ", None, Some(l)),
            Call(l) => ("\n \t", None, Some(l)),
            Jmp(l) => ("\n \n", None, Some(l)),
            Jz(l) => ("\n\t ", None, Some(l)),
            Jn(l) => ("\n\t\t", None, Some(l)),
            Ret => ("\n\t\n", None, None),
            End => ("\n\n\n", None, None),
            PutC => ("\t\n  ", None, None),
            PutN => ("\t\n \t", None, None),
            GetC => ("\t\n\t ", None, None),
            GetN => ("\t\n\t\t", None, None),
        };
        s.push_str(cmd);
        if let Some(n) = num {
            s.push(if n < 0 { '\t' } else { ' ' });
            bits(&mut s, n.unsigned_abs());
            s.push('\n');
        }
        if let Some(l) = label {
            assert!(l > 0, "labels are positive");
            bits(&mut s, l as u64);
            s.push('\n');
        }
    }
    s
}

struct Cursor<'a> {
    s: &'a [u8],
    p: usize,
}

impl<'a> Cursor<'a> {
    fn next(&mut self) -> Result<u8, String> {
        let c = *self.s.get(self.p).ok_or("unexpected end of program")?;
        self.p += 1;
        Ok(c)
    }

    /// Characters up to (excluding) the next line feed.
    fn field(&mut self) -> Result<&'a [u8], String> {
        let start = self.p;
        while self.next()? != b'\n' {}
        Ok(&self.s[start..self.p - 1])
    }

    fn num(&mut self) -> Result<i64, String> {
        let (sign, digits) = self.field()?.split_first().ok_or("number without sign")?;
        let mut n: i64 = 0;
        for &d in digits {
            n = n.checked_mul(2).ok_or("number too large")? + (d == b'\t') as i64;
        }
        Ok(if *sign == b'\t' { -n } else { n })
    }
}

/// Parse Whitespace source; jump/call targets become instruction indices.
pub fn parse(src: &str) -> Result<Vec<Ins>, String> {
    use Ins::*;
    const S: u8 = b' ';
    const T: u8 = b'\t';
    let s: Vec<u8> = src
        .bytes()
        .filter(|b| matches!(b, b' ' | b'\t' | b'\n'))
        .collect();
    let mut c = Cursor { s: &s, p: 0 };
    let mut code = Vec::new();
    let mut labels: HashMap<&[u8], usize> = HashMap::new();
    let mut refs = Vec::new();
    while c.p < s.len() {
        let at = c.p;
        let bad = || format!("invalid command at char {at}");
        let ins = match c.next()? {
            S => match c.next()? {
                S => Push(c.num()?),
                T => match c.next()? {
                    S => Copy(c.num()?),
                    T => return Err(bad()),
                    _ => Slide(c.num()?),
                },
                _ => match c.next()? {
                    S => Dup,
                    T => Swap,
                    _ => Drop,
                },
            },
            T => match c.next()? {
                S => match (c.next()?, c.next()?) {
                    (S, S) => Add,
                    (S, T) => Sub,
                    (S, _) => Mul,
                    (T, S) => Div,
                    (T, T) => Mod,
                    _ => return Err(bad()),
                },
                T => match c.next()? {
                    S => Store,
                    T => Load,
                    _ => return Err(bad()),
                },
                _ => match (c.next()?, c.next()?) {
                    (S, S) => PutC,
                    (S, T) => PutN,
                    (T, S) => GetC,
                    (T, T) => GetN,
                    _ => return Err(bad()),
                },
            },
            _ => match (c.next()?, c.next()?) {
                (S, S) => {
                    if labels.insert(c.field()?, code.len()).is_some() {
                        return Err(format!("label defined twice at char {at}"));
                    }
                    continue;
                }
                (T, b'\n') => Ret,
                (b'\n', b'\n') => End,
                (b'\n', _) => return Err(bad()),
                (a, b) => {
                    refs.push((code.len(), c.field()?));
                    match (a, b) {
                        (S, T) => Call(0),
                        (S, _) => Jmp(0),
                        (_, S) => Jz(0),
                        _ => Jn(0),
                    }
                }
            },
        };
        code.push(ins);
    }
    for (i, l) in refs {
        let t = *labels.get(l).ok_or("jump to undefined label")? as i64;
        code[i] = match code[i] {
            Call(_) => Call(t),
            Jmp(_) => Jmp(t),
            Jz(_) => Jz(t),
            _ => Jn(t),
        };
    }
    Ok(code)
}

fn floor_div(a: i64, b: i64) -> i64 {
    let q = a / b;
    if a % b != 0 && (a < 0) != (b < 0) {
        q - 1
    } else {
        q
    }
}

fn floor_mod(a: i64, b: i64) -> i64 {
    let r = a % b;
    if r != 0 && (r < 0) != (b < 0) {
        r + b
    } else {
        r
    }
}

/// Run parsed code. `input` returns the next byte or -1 at end of input; it
/// gets the pending output so an interactive host can show it before blocking.
pub fn run(
    code: &[Ins],
    input: &mut dyn FnMut(&mut Vec<u8>) -> i64,
    out: &mut Vec<u8>,
) -> Result<(), String> {
    Machine::default()
        .run(code, input, out, u64::MAX)
        .map(|_| ())
}

/// The state of a running program, which can be inspected between steps.
#[derive(Default)]
pub struct Machine {
    /// Index of the next instruction.
    pub pc: usize,
    pub stack: Vec<i64>,
    pub heap: Vec<i64>,
    /// Return addresses of the active calls, innermost last.
    pub calls: Vec<usize>,
}

/// What `input` returns when nothing can be read yet: a character read
/// stops the machine before it (`run` returns Ok(false)) and is retried on
/// the next `run`, so a host can wait without blocking (number reads treat
/// it as the end of the line).
pub const WAIT: i64 = i64::MIN;

impl Machine {
    /// Run at most `steps` instructions: Ok(true) once the program ended,
    /// Ok(false) if the budget ran out or input said WAIT (calling again
    /// resumes).
    pub fn run(
        &mut self,
        code: &[Ins],
        input: &mut dyn FnMut(&mut Vec<u8>) -> i64,
        out: &mut Vec<u8>,
        steps: u64,
    ) -> Result<bool, String> {
        let Machine {
            pc,
            stack: st,
            heap,
            calls,
        } = self;
        run_loop(code, input, out, steps, pc, st, heap, calls)
    }
}

#[allow(clippy::too_many_arguments)]
fn run_loop(
    code: &[Ins],
    input: &mut dyn FnMut(&mut Vec<u8>) -> i64,
    out: &mut Vec<u8>,
    steps: u64,
    pc_out: &mut usize,
    st: &mut Vec<i64>,
    heap: &mut Vec<i64>,
    calls: &mut Vec<usize>,
) -> Result<bool, String> {
    use Ins::*;
    let mut pc = *pc_out;
    let mut left = steps;
    // the program counter goes back to the caller's state on every exit
    let r = (|| {
        while pc < code.len() {
            if left == 0 {
                return Ok(false);
            }
            left -= 1;
            let ins = code[pc];
            pc += 1;
            let err = |what: &str| format!("{what} at instruction {} ({ins:?})", pc - 1);
            macro_rules! pop {
                () => {
                    st.pop().ok_or_else(|| err("stack underflow"))?
                };
            }
            macro_rules! addr {
                ($a:expr) => {
                    usize::try_from($a).map_err(|_| err("negative heap address"))?
                };
            }
            macro_rules! arith {
                ($f:expr) => {{
                    let b = pop!();
                    let a = pop!();
                    st.push($f(a, b).ok_or_else(|| err("arithmetic overflow or division by zero"))?)
                }};
            }
            match ins {
                Push(n) => st.push(n),
                Dup => st.push(*st.last().ok_or_else(|| err("stack underflow"))?),
                Copy(n) => {
                    let i = usize::try_from(n)
                        .ok()
                        .and_then(|n| st.len().checked_sub(n + 1))
                        .ok_or_else(|| err("copy out of range"))?;
                    st.push(st[i]);
                }
                Swap => {
                    let n = st.len();
                    if n < 2 {
                        return Err(err("stack underflow"));
                    }
                    st.swap(n - 1, n - 2);
                }
                Drop => {
                    pop!();
                }
                Slide(n) => {
                    let top = pop!();
                    st.truncate(st.len().saturating_sub(addr!(n)));
                    st.push(top);
                }
                Add => arith!(i64::checked_add),
                Sub => arith!(i64::checked_sub),
                Mul => arith!(i64::checked_mul),
                Div => arith!(|a, b| (b != 0).then(|| floor_div(a, b))),
                Mod => arith!(|a, b| (b != 0).then(|| floor_mod(a, b))),
                Store => {
                    let v = pop!();
                    let a = addr!(pop!());
                    if a >= heap.len() {
                        heap.resize(a + 1, 0);
                    }
                    heap[a] = v;
                }
                Load => {
                    let a = addr!(pop!());
                    st.push(heap.get(a).copied().unwrap_or(0));
                }
                Label(_) | Note(_) => {}
                Call(t) => {
                    calls.push(pc);
                    pc = t as usize;
                }
                Jmp(t) => pc = t as usize,
                Jz(t) => {
                    if pop!() == 0 {
                        pc = t as usize;
                    }
                }
                Jn(t) => {
                    if pop!() < 0 {
                        pc = t as usize;
                    }
                }
                Ret => pc = calls.pop().ok_or_else(|| err("return outside a call"))?,
                End => return Ok(true),
                PutC => {
                    let ch = u32::try_from(pop!())
                        .ok()
                        .and_then(char::from_u32)
                        .ok_or_else(|| err("invalid character"))?;
                    out.extend_from_slice(ch.encode_utf8(&mut [0; 4]).as_bytes());
                }
                PutN => out.extend_from_slice(pop!().to_string().as_bytes()),
                GetC | GetN => {
                    let a = addr!(pop!());
                    let v = if ins == GetC {
                        let v = input(out);
                        if v == WAIT {
                            // undo the read so that running again retries it
                            st.push(a as i64);
                            pc -= 1;
                            return Ok(false);
                        }
                        v
                    } else {
                        let mut line = String::new();
                        loop {
                            match input(out) {
                                -1 | 10 | WAIT => break,
                                b => line.push(b as u8 as char),
                            }
                        }
                        line.trim()
                            .parse()
                            .map_err(|_| err("invalid number input"))?
                    };
                    if a >= heap.len() {
                        heap.resize(a + 1, 0);
                    }
                    heap[a] = v;
                }
            }
        }
        Ok(true)
    })();
    *pc_out = pc;
    r
}
