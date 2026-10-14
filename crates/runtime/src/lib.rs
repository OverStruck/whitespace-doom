//! Runtime for compiled DOOM, written with the wsdoom DSL: libc subset,
//! W_* lump access over baked lumps, program entry.
//!
//! There is no file system: savegames live in memory for the session and
//! every other open fails, so DOOM runs on its built-in defaults; `access`
//! only finds "doom1.wad" (whose lumps are baked in).

use wadbake::Baked;
use wsdoom::cc::symbols::{Builtin, Symbols};
use wsdoom::dsl::{max, min, Ex, Fun, Prog, A, E, V};

const fn b(name: &'static str, arity: usize, variadic: bool) -> Builtin {
    Builtin {
        name,
        arity,
        variadic,
    }
}

/// Every function this crate defines (the compiler creates their `Fun`s).
pub const BUILTINS: &[Builtin] = &[
    b("memset", 3, false),
    b("memcpy", 3, false),
    b("strcpy", 2, false),
    b("strncpy", 3, false),
    b("strcat", 2, false),
    b("strcmp", 2, false),
    b("strncmp", 3, false),
    b("strcasecmp", 2, false),
    b("strncasecmp", 3, false),
    b("strlen", 1, false),
    b("toupper", 1, false),
    b("atoi", 1, false),
    b("abs", 1, false),
    b("malloc", 1, false),
    b("realloc", 2, false),
    b("free", 1, false),
    b("alloca", 1, false),
    b("exit", 1, false),
    b("printf", 1, true),
    b("sprintf", 2, true),
    b("fprintf", 2, true),
    b("vprintf", 2, false),
    b("vsprintf", 3, false),
    b("vfprintf", 3, false),
    b("sscanf", 2, true),
    b("fscanf", 2, true),
    b("fopen", 2, false),
    b("fclose", 1, false),
    b("fread", 4, false),
    b("fwrite", 4, false),
    b("fseek", 3, false),
    b("ftell", 1, false),
    b("feof", 1, false),
    b("open", 2, true),
    b("close", 1, false),
    b("read", 3, false),
    b("write", 3, false),
    b("lseek", 3, false),
    b("fstat", 2, false),
    b("access", 2, false),
    b("mkdir", 2, false),
    b("getenv", 1, false),
    b("W_InitMultipleFiles", 1, false),
    b("W_Reload", 0, false),
    b("W_CheckNumForName", 1, false),
    b("W_GetNumForName", 1, false),
    b("W_LumpLength", 1, false),
    b("W_ReadLump", 2, false),
    b("W_CacheLumpNum", 2, false),
    b("W_CacheLumpName", 2, false),
    b("FixedDiv2", 2, false),
    b("wipe_initMelt", 3, false),
    b("wipe_doMelt", 3, false),
    b("wipe_exitMelt", 3, false),
    b("setbuf", 2, false),
    b("getchar", 0, false),
    b("filelength", 1, false),
    b("M_ReadFile", 2, false),
];

/// Cells for malloc/alloca (bump allocated, never reused); V_Init alone
/// takes 4 screens of 320x200 through I_AllocLow.
pub const MALLOC_CELLS: i64 = 1 << 21;
/// shortcut: variadic calls with more arguments overflow this array; DOOM passes at most a handful.
pub const MAX_VARARGS: i64 = 32;
const ZONEID: i64 = 0x1d4a11;
const PU_STATIC: i64 = 1;
/// In-memory files (savegames, 6 slots): how many, cells each (g_game.c
/// SAVEGAMESIZE), distance between files (room for a zone block header),
/// longest name, first descriptor.
const NFILES: i64 = 8;
const FILE_CELLS: i64 = 0x2c000;
const STRIDE: i64 = FILE_CELLS + 8;
const NAME_CELLS: i64 = 64;
const FD0: i64 = 100;

/// The cell at address `a`.
fn m(a: impl Into<E>) -> E {
    E::Load(Box::new(a.into()))
}

fn vars<const K: usize>(p: &mut Prog) -> [V; K] {
    std::array::from_fn(|_| p.var())
}

fn cstr(p: &mut Prog, s: &str) -> i64 {
    let mut v: Vec<i64> = s.bytes().map(i64::from).collect();
    v.push(0);
    p.arr_init(&v).0
}

fn digit(c: impl Into<E> + Clone) -> E {
    c.clone().into().ge('0') & c.into().le('9')
}

fn space(c: impl Into<E> + Clone) -> E {
    c.clone().into().eq(' ') | c.clone().into().eq('\t') | c.into().eq('\n')
}

/// Uppercase the letter held in `c` (C locale `toupper`).
fn upcase(p: &mut Prog, c: V) {
    p.if_(c.ge('a') & c.le('z'), |p| p.inc(c, -32));
}

/// Call a variadic function: fixed args, variadic args, then their count.
fn call_var(f: Fun, fixed: Vec<E>, var: Vec<E>) -> E {
    let k = var.len() as i64;
    let mut a = fixed;
    a.extend(var);
    a.push(k.into());
    f.call_vec(a)
}

/// Define every builtin of `BUILTINS`, with the baked WAD as the only file.
pub fn define(p: &mut Prog, s: &Symbols, wad: &Baked) {
    let f = |n: &str| s.func(n);
    let ierror = f("I_Error");

    // ---- memory and strings ----
    let [d, src, n, i, c, e] = vars(p);
    p.def(f("memset"), &[d, c, n], |p| {
        // the raw value is stored: -1 stays -1 for ints, 0xff is 255 for bytes
        p.for_(i, 0, n, |p| p.set(m(d + i), c));
        p.ret(d);
    });
    p.def(f("memcpy"), &[d, src, n], |p| {
        p.for_(i, 0, n, |p| p.set(m(d + i), m(src + i)));
        p.ret(d);
    });
    p.def(f("strcpy"), &[d, src], |p| {
        p.set(i, 0);
        p.while_(m(src + i).ne(0), |p| {
            p.set(m(d + i), m(src + i));
            p.inc(i, 1);
        });
        p.set(m(d + i), 0);
        p.ret(d);
    });
    p.def(f("strncpy"), &[d, src, n], |p| {
        p.set(i, 0);
        p.while_(i.lt(n) & m(src + i).ne(0), |p| {
            p.set(m(d + i), m(src + i));
            p.inc(i, 1);
        });
        p.while_(i.lt(n), |p| {
            p.set(m(d + i), 0);
            p.inc(i, 1);
        });
        p.ret(d);
    });
    let strlen = f("strlen");
    p.def(strlen, &[src], |p| {
        p.set(i, 0);
        p.while_(m(src + i).ne(0), |p| p.inc(i, 1));
        p.ret(i);
    });
    p.def(f("strcat"), &[d, src], |p| {
        p.set(e, d);
        p.while_(m(e).ne(0), |p| p.inc(e, 1));
        p.set(i, 0);
        p.while_(m(src + i).ne(0), |p| {
            p.set(m(e + i), m(src + i));
            p.inc(i, 1);
        });
        p.set(m(e + i), 0);
        p.ret(d);
    });
    // strcmp family: (a, b, n, fold case); n < 0 means unlimited
    let [x, y, ca, cb] = vars(p);
    let cmp = |p: &mut Prog, fun: Fun, limited: bool, fold: bool| {
        let params: Vec<V> = if limited { vec![x, y, n] } else { vec![x, y] };
        p.def(fun, &params, |p| {
            if !limited {
                p.set(n, -1);
            }
            p.set(i, 0);
            p.while_(n.lt(0) | i.lt(n), |p| {
                p.set(ca, m(x + i));
                p.set(cb, m(y + i));
                if fold {
                    upcase(p, ca);
                    upcase(p, cb);
                }
                p.if_(ca.ne(cb) | ca.eq(0), |p| p.ret(ca - cb));
                p.inc(i, 1);
            });
            p.ret(0);
        });
    };
    cmp(p, f("strcmp"), false, false);
    cmp(p, f("strncmp"), true, false);
    cmp(p, f("strcasecmp"), false, true);
    cmp(p, f("strncasecmp"), true, true);
    p.def(f("toupper"), &[c], |p| {
        upcase(p, c);
        p.ret(c);
    });
    // DOOM calls abs() undeclared on angle_t differences (r_segs.c): the
    // unsigned bits arrive as they are and abs() reads them as an int
    p.def(f("abs"), &[c], |p| {
        p.ret(wsdoom::dsl::abs(
            (c + (1i64 << 31)) % (1i64 << 32) - (1i64 << 31),
        ))
    });
    let [neg, val] = vars(p);
    p.def(f("atoi"), &[src], |p| {
        p.while_(space(m(src)), |p| p.inc(src, 1));
        p.set(neg, m(src).eq('-'));
        p.if_(m(src).eq('-') | m(src).eq('+'), |p| p.inc(src, 1));
        p.set(val, 0);
        p.while_(digit(m(src)), |p| {
            p.set(val, val * 10 + m(src) - '0');
            p.inc(src, 1);
        });
        p.if_(neg, |p| p.set(val, -val));
        p.ret(val);
    });

    // ---- allocation ----
    let heap = p.alloc(MALLOC_CELLS);
    let brk = p.var_init(heap);
    let oom = cstr(p, "malloc: out of memory (%i cells)");
    let malloc = f("malloc");
    let [sz, r] = vars(p);
    p.def(malloc, &[sz], |p| {
        // aligned like C's malloc: savegame padding (p_saveg.c PADSAVEP)
        // depends on screens[1] and the loaded file sharing an alignment
        p.set(r, (brk + 4) / 4 * 4);
        p.if_(sz.lt(0) | (r + sz).gt(heap + MALLOC_CELLS), |p| {
            p.eval(call_var(ierror, vec![oom.into()], vec![sz.into()]))
        });
        p.set(m(r - 1), sz); // size header for realloc
        p.set(brk, r + sz);
        p.ret(r);
    });
    let [old, nsz, fresh] = vars(p);
    p.def(f("realloc"), &[old, nsz], |p| {
        p.set(fresh, malloc.call([nsz.into()]));
        p.if_(old.ne(0), |p| {
            p.for_(i, 0, wsdoom::dsl::min(m(old - 1), nsz), |p| {
                p.set(m(fresh + i), m(old + i))
            });
        });
        p.ret(fresh);
    });
    p.def(f("free"), &[sz], |_| {}); // shortcut: memory is never reclaimed; DOOM mallocs only at startup
                                     // shortcut: alloca memory is never released; DOOM only uses it during init (R_InitTextures)
    p.def(f("alloca"), &[sz], |p| p.ret(malloc.call([sz.into()])));
    p.def(f("exit"), &[sz], |p| p.exit());

    // ---- formatted output ----
    let fmt = p.func(3);
    define_format(p, fmt);
    let [stream, fp, ap] = vars(p);
    let va = |p: &mut Prog, fun: Fun, fixed: &[V], body: &dyn Fn(&mut Prog, A)| {
        let (args, cnt) = (p.arr(MAX_VARARGS), p.var());
        p.def_variadic(fun, fixed, args, cnt, |p| body(p, args));
    };
    va(p, f("printf"), &[fp], &|p, a| {
        p.ret(fmt.call([0.into(), fp.into(), a.0.into()]))
    });
    va(p, f("sprintf"), &[d, fp], &|p, a| {
        p.ret(fmt.call([d.into(), fp.into(), a.0.into()]))
    });
    va(p, f("fprintf"), &[stream, fp], &|p, a| {
        p.ret(fmt.call([0.into(), fp.into(), a.0.into()]))
    });
    // va_list = address of the first variadic argument cell
    p.def(f("vprintf"), &[fp, ap], |p| {
        p.ret(fmt.call([0.into(), fp.into(), ap.into()]))
    });
    p.def(f("vsprintf"), &[d, fp, ap], |p| {
        p.ret(fmt.call([d.into(), fp.into(), ap.into()]))
    });
    p.def(f("vfprintf"), &[stream, fp, ap], |p| {
        p.ret(fmt.call([0.into(), fp.into(), ap.into()]))
    });
    let scan = p.func(3);
    define_scan(p, scan);
    va(p, f("sscanf"), &[src, fp], &|p, a| {
        p.ret(scan.call([src.into(), fp.into(), a.0.into()]))
    });

    // ---- files: savegames live in memory for the session (Whitespace has
    // no files), nothing else exists (FILE* is an int; NULL/-1 = failure) ----
    let [a1, a2, a3, a4] = vars(p);
    va(p, f("fscanf"), &[stream, fp], &|p, _| p.ret(-1));
    p.def(f("fopen"), &[a1, a2], |p| p.ret(0));
    p.def(f("fclose"), &[a1], |p| p.ret(-1));
    p.def(f("fread"), &[a1, a2, a3, a4], |p| p.ret(0));
    p.def(f("fwrite"), &[a1, a2, a3, a4], |p| p.ret(0));
    p.def(f("fseek"), &[a1, a2, a3], |p| p.ret(-1));
    p.def(f("ftell"), &[a1], |p| p.ret(-1));
    p.def(f("feof"), &[a1], |p| p.ret(1));
    // file k: name at names + k*NAME_CELLS ("" = unused), its cells at
    // data + k*STRIDE (4-aligned, see malloc) behind room for a zone block
    // header; descriptor FD0 + k with one position per file
    let names = p.alloc(NFILES * NAME_CELLS);
    let (lens, pos) = (p.arr(NFILES), p.arr(NFILES));
    let data = (p.alloc(NFILES * STRIDE + 3) + 3) / 4 * 4 + 8;
    let (strcmp, strcpy) = (f("strcmp"), f("strcpy"));
    let find = p.func(1);
    let k = p.var(); // not `i`: strcmp may use that
    p.def(find, &[a1], |p| {
        p.for_(k, 0, NFILES, |p| {
            let name = names + k * NAME_CELLS;
            p.if_(m(name.clone()).ne(0), |p| {
                p.if_(strcmp.call([name, a1.into()]).eq(0), |p| p.ret(k))
            });
        });
        p.ret(-1);
    });
    va(p, f("open"), &[a1, a2], &|p, _| {
        p.set(a3, find.call([a1.into()]));
        // O_CREAT (64); m_misc.c always adds O_TRUNC
        p.if_((a2 / 64 % 2).eq(1), |p| {
            p.for_(i, 0, NFILES, |p| {
                p.if_(a3.lt(0) & m(names + i * NAME_CELLS).eq(0), |p| p.set(a3, i))
            });
            p.if_(a3.lt(0) | strlen.call([a1.into()]).ge(NAME_CELLS), |p| {
                p.ret(-1)
            });
            p.eval(strcpy.call([names + a3 * NAME_CELLS, a1.into()]));
            p.set(lens.at(a3), 0);
        });
        p.if_(a3.lt(0), |p| p.ret(-1));
        p.set(pos.at(a3), 0);
        p.ret(a3 + FD0);
    });
    let slot = |p: &mut Prog, fd: V| {
        p.set(fd, fd - FD0);
        p.if_(fd.lt(0) | fd.ge(NFILES), |p| p.ret(-1));
    };
    p.def(f("close"), &[a1], |p| {
        slot(p, a1);
        p.ret(0);
    });
    p.def(f("read"), &[a1, a2, a3], |p| {
        slot(p, a1);
        p.set(a3, min(a3, lens.at(a1) - pos.at(a1)));
        p.set(a4, data + a1 * STRIDE + pos.at(a1));
        p.for_(i, 0, a3, |p| p.set(m(a2 + i), m(a4 + i)));
        p.inc(pos.at(a1), a3);
        p.ret(a3);
    });
    p.def(f("write"), &[a1, a2, a3], |p| {
        slot(p, a1);
        p.if_((pos.at(a1) + a3).gt(FILE_CELLS), |p| p.ret(-1));
        p.set(a4, data + a1 * STRIDE + pos.at(a1));
        p.for_(i, 0, a3, |p| p.set(m(a4 + i), m(a2 + i)));
        p.inc(pos.at(a1), a3);
        p.set(lens.at(a1), max(lens.at(a1), pos.at(a1)));
        p.ret(a3);
    });
    p.def(f("lseek"), &[a1, a2, a3], |p| p.ret(-1));
    p.def(f("fstat"), &[a1, a2], |p| p.ret(-1));
    p.def(f("mkdir"), &[a1, a2], |p| p.ret(-1));
    let wadname = cstr(p, "doom1.wad");
    p.def(f("access"), &[a1, a2], |p| {
        p.set(a3, strlen.call([a1.into()]) - 9);
        p.if_(a3.lt(0), |p| p.ret(-1));
        p.for_(i, 0, 9, |p| {
            p.if_(m(a1 + a3 + i).ne(m(wadname + i)), |p| p.ret(-1))
        });
        p.ret(0);
    });
    let (home, dot) = (cstr(p, "HOME"), cstr(p, "."));
    p.def(f("getenv"), &[a1], |p| {
        p.if_(strcmp.call([a1.into(), home.into()]).eq(0), |p| p.ret(dot));
        p.ret(0);
    });

    p.def(f("setbuf"), &[a1, a2], |_| {});
    // only D_DoomMain's "press enter" pause for modified games uses it
    p.def(f("getchar"), &[], |p| p.ret(10));
    p.def(f("filelength"), &[a1], |p| p.ret(-1));
    // m_misc.c's own version needs struct stat. The file is handed out in
    // place behind a fake zone block header, which G_DoLoadGame's Z_Free
    // leaves alone (as for WAD lumps).
    let unreadable = cstr(p, "Couldn't read file %s");
    let mb = s.size("memblock_t");
    let field = |n: &str| s.field("memblock_t", n) - mb;
    let owner = p.var(); // Z_Free clears *user: this cell
    let wall = p.alloc(mb); // a used neighbour, so Z_Free merges nothing
    p.set_init(wall + field("user") + mb, &[owner.0]);
    p.def(f("M_ReadFile"), &[a1, a2], |p| {
        p.set(a3, find.call([a1.into()]));
        p.if_(a3.lt(0), |p| {
            p.eval(call_var(ierror, vec![unreadable.into()], vec![a1.into()]))
        });
        p.set(a4, data + a3 * STRIDE);
        p.set(m(a4 + field("size")), lens.at(a3) + mb);
        p.set(m(a4 + field("user")), owner.0);
        p.set(m(a4 + field("tag")), PU_STATIC);
        p.set(m(a4 + field("id")), ZONEID);
        p.set(m(a4 + field("next")), wall);
        p.set(m(a4 + field("prev")), wall);
        p.set(m(a2), a4);
        p.ret(lens.at(a3));
    });

    define_fixed_div(p, ierror, f("FixedDiv2"));
    define_melt(p, s);
    define_wad(p, s, wad, malloc);
}

/// m_fixed.c's FixedDiv2 without doubles: (a << 16) / b truncated toward
/// zero, with I_Error when the exact quotient leaves [-2^31, 2^31).
fn define_fixed_div(p: &mut Prog, ierror: Fun, fun: Fun) {
    let msg = cstr(p, "FixedDiv: divide by zero");
    let [a, b, n, d, q] = vars(p);
    let fail = move |p: &mut Prog| p.eval(call_var(ierror, vec![msg.into()], vec![]));
    p.def(fun, &[a, b], |p| {
        p.if_(b.eq(0), fail);
        p.set(n, wsdoom::dsl::abs(a) * 65536);
        p.set(d, wsdoom::dsl::abs(b));
        p.set(q, n / d);
        p.if_else(
            a.lt(0).ne(b.lt(0)),
            |p| {
                p.if_(n.gt(d * (1i64 << 31)), fail);
                p.ret(-q);
            },
            |p| {
                p.if_(q.ge(1i64 << 31), fail);
                p.ret(q);
            },
        );
    });
}

/// f_wipe.c's melt with one pixel per cell. The original moves 2-pixel
/// strips as `short`s over column-major copies of the start/end screens;
/// here strip i (pixels 2i, 2i+1) is copied directly from the row-major
/// screens, drawing identical pixels each tick with the same M_Random calls.
fn define_melt(p: &mut Prog, s: &Symbols) {
    // shortcut: y[] is static, sized for any width DOOM uses (SCREENWIDTH = 320)
    const MAX_WIDTH: i64 = 1024;
    let ys = p.arr(MAX_WIDTH);
    let (start, end, scr) = (
        V(s.global("wipe_scr_start")),
        V(s.global("wipe_scr_end")),
        V(s.global("wipe_scr")),
    );
    let (memcpy, rnd) = (s.func("memcpy"), s.func("M_Random"));
    let [w, h, ticks, i, r, yi, dy, done, half, k, x] = vars(p);
    p.def(s.func("wipe_initMelt"), &[w, h, ticks], |p| {
        p.call(memcpy, [scr.into(), start.into(), w * h]);
        p.set(ys.at(0), -(rnd.call([]) % 16));
        p.for_(i, 1, w, |p| {
            p.set(r, rnd.call([]) % 3 - 1);
            p.set(ys.at(i), ys.at(i - 1) + r);
            p.if_else(
                ys.at(i).gt(0),
                |p| p.set(ys.at(i), 0),
                |p| p.if_(ys.at(i).eq(-16), |p| p.set(ys.at(i), -15)),
            );
        });
        p.ret(0);
    });
    p.def(s.func("wipe_doMelt"), &[w, h, ticks], |p| {
        p.set(done, 1);
        p.set(half, w / 2);
        p.while_(ticks.gt(0), |p| {
            p.inc(ticks, -1);
            p.for_(i, 0, half, |p| {
                p.set(yi, ys.at(i));
                p.if_(yi.lt(0), |p| {
                    p.inc(ys.at(i), 1);
                    p.set(done, 0);
                    p.cont();
                });
                p.if_(yi.ge(h), |p| p.cont());
                p.set(dy, 8);
                p.if_(yi.lt(16), |p| p.set(dy, yi + 1));
                p.if_((yi + dy).ge(h), |p| p.set(dy, h - yi));
                // rows yi..yi+dy of the end screen appear
                p.for_(k, yi, yi + dy, |p| {
                    p.set(x, k * w + i * 2);
                    p.set(m(scr + x), m(end + x));
                    p.set(m(scr + x + 1), m(end + x + 1));
                });
                p.inc(yi, dy);
                p.set(ys.at(i), yi);
                // the start screen slides down to row yi
                p.for_(k, 0, h - yi, |p| {
                    p.set(x, (yi + k) * w + i * 2);
                    p.set(r, k * w + i * 2);
                    p.set(m(scr + x), m(start + r));
                    p.set(m(scr + x + 1), m(start + r + 1));
                });
                p.set(done, 0);
            });
        });
        p.ret(done);
    });
    p.def(s.func("wipe_exitMelt"), &[w, h, ticks], |p| p.ret(0));
}

/// fmt(dest, format, ap): printf formatting of the arguments at `ap`, into
/// `dest` (NUL-terminated) or to the terminal when dest is 0. Returns the
/// character count. Supports %d %i %u %x %X %p %s %c %% with - 0 flags,
/// width and precision; length modifiers are ignored.
fn define_format(p: &mut Prog, fmt: Fun) {
    let [dest, f, ap, n, c, left, zero, width, prec, val, base, sgn, s, len, k] = vars(p);
    let buf = p.arr(24);
    let null = cstr(p, "(null)");
    let put = |p: &mut Prog, ch: E| {
        let ch2 = ch.clone();
        p.if_else(
            dest.ne(0),
            move |p| {
                p.set(m(dest), ch2);
                p.inc(dest, 1);
            },
            move |p| p.putc(ch),
        );
        p.inc(n, 1);
    };
    let next = |p: &mut Prog| {
        p.inc(f, 1);
        p.set(c, m(f));
    };
    p.def(fmt, &[dest, f, ap], |p| {
        p.set(n, 0);
        p.while_(m(f).ne(0), |p| {
            p.set(c, m(f));
            p.inc(f, 1);
            p.if_(c.ne('%'), |p| {
                put(p, c.into());
                p.cont();
            });
            p.set(left, 0);
            p.set(zero, 0);
            p.set(width, 0);
            p.set(prec, -1);
            p.set(sgn, 0);
            p.set(c, m(f));
            p.while_(
                c.eq('-') | c.eq('0') | c.eq(' ') | c.eq('+') | c.eq('#'),
                |p| {
                    p.if_(c.eq('-'), |p| p.set(left, 1));
                    p.if_(c.eq('0'), |p| p.set(zero, 1));
                    next(p);
                },
            );
            p.while_(digit(c), |p| {
                p.set(width, width * 10 + c - '0');
                next(p);
            });
            p.if_(c.eq('.'), |p| {
                p.set(prec, 0);
                next(p);
                p.while_(digit(c), |p| {
                    p.set(prec, prec * 10 + c - '0');
                    next(p);
                });
            });
            p.while_(c.eq('l') | c.eq('h') | c.eq('L'), |p| next(p));
            p.if_(c.eq(0), |p| p.cont()); // lone '%' at the end
            p.inc(f, 1);
            p.if_(c.eq('%'), |p| {
                put(p, '%'.into());
                p.cont();
            });
            p.if_else(
                c.eq('s') | c.eq('c'),
                |p| {
                    p.set(zero, 0);
                    p.if_else(
                        c.eq('s'),
                        |p| {
                            p.set(s, m(ap));
                            p.if_(s.eq(0), |p| p.set(s, null));
                            p.set(len, 0);
                            p.while_((prec.lt(0) | len.lt(prec)) & m(s + len).ne(0), |p| {
                                p.inc(len, 1)
                            });
                        },
                        |p| {
                            p.set(buf.at(0), m(ap));
                            p.set(s, buf.0);
                            p.set(len, 1);
                        },
                    );
                },
                |p| {
                    p.set(val, m(ap));
                    p.set(base, 10);
                    p.if_(c.eq('x') | c.eq('X') | c.eq('p'), |p| p.set(base, 16));
                    p.if_else(
                        c.eq('d') | c.eq('i'),
                        |p| {
                            p.if_(val.lt(0), |p| {
                                p.set(sgn, '-');
                                p.set(val, -val);
                            })
                        },
                        |p| p.if_(val.lt(0), |p| p.inc(val, 1i64 << 32)), // unsigned 32-bit view
                    );
                    p.if_(left.ne(0) | prec.ge(0), |p| p.set(zero, 0));
                    p.set(s, buf.0 + 24);
                    p.set(len, 0);
                    p.while_(len.eq(0) | val.ne(0), |p| {
                        p.set(k, val % base);
                        p.if_(k.ge(10), |p| {
                            p.if_else(
                                c.eq('X'),
                                |p| p.inc(k, 'A' as i64 - '0' as i64 - 10),
                                |p| p.inc(k, 'a' as i64 - '0' as i64 - 10),
                            )
                        });
                        p.inc(s, -1);
                        p.set(m(s), k + '0');
                        p.set(val, val / base);
                        p.inc(len, 1);
                    });
                    p.while_(len.lt(prec), |p| {
                        p.inc(s, -1);
                        p.set(m(s), '0');
                        p.inc(len, 1);
                    });
                },
            );
            p.inc(ap, 1);
            p.inc(width, -len);
            p.if_(sgn.ne(0), |p| p.inc(width, -1));
            p.if_(left.eq(0) & zero.eq(0), |p| {
                p.while_(width.gt(0), |p| {
                    put(p, ' '.into());
                    p.inc(width, -1);
                })
            });
            p.if_(sgn.ne(0), |p| put(p, sgn.into()));
            p.while_(width.gt(0) & zero.ne(0), |p| {
                put(p, '0'.into());
                p.inc(width, -1);
            });
            p.for_(k, 0, len, |p| put(p, m(s + k)));
            p.while_(width.gt(0), |p| {
                put(p, ' '.into());
                p.inc(width, -1);
            });
        });
        p.if_(dest.ne(0), |p| p.set(m(dest), 0));
        p.ret(n);
    });
}

/// scan(str, format, ap): sscanf supporting whitespace, literal characters,
/// %d %i %x (optional 0x) and %s; field widths are skipped. Returns the
/// number of assignments.
fn define_scan(p: &mut Prog, scan: Fun) {
    let [sp, f, ap, cnt, c, neg, val, base, dg, got] = vars(p);
    p.def(scan, &[sp, f, ap], |p| {
        p.set(cnt, 0);
        p.while_(m(f).ne(0), |p| {
            p.set(c, m(f));
            p.inc(f, 1);
            p.if_(space(c), |p| {
                p.while_(space(m(sp)), |p| p.inc(sp, 1));
                p.cont();
            });
            p.if_(c.ne('%'), |p| {
                p.if_(m(sp).ne(c), |p| p.brk());
                p.inc(sp, 1);
                p.cont();
            });
            p.while_(digit(m(f)) | m(f).eq('l') | m(f).eq('h'), |p| p.inc(f, 1));
            p.set(c, m(f));
            p.inc(f, 1);
            p.while_(space(m(sp)), |p| p.inc(sp, 1));
            p.if_(m(sp).eq(0), |p| p.brk());
            p.if_(c.eq('s'), |p| {
                p.set(val, m(ap));
                p.while_(m(sp).ne(0) & !space(m(sp)), |p| {
                    p.set(m(val), m(sp));
                    p.inc(val, 1);
                    p.inc(sp, 1);
                });
                p.set(m(val), 0);
                p.inc(ap, 1);
                p.inc(cnt, 1);
                p.cont();
            });
            p.set(neg, m(sp).eq('-'));
            p.if_(m(sp).eq('-') | m(sp).eq('+'), |p| p.inc(sp, 1));
            p.set(base, 10);
            p.if_(c.eq('x'), |p| p.set(base, 16));
            p.if_(
                (c.eq('x') | c.eq('i')) & m(sp).eq('0') & (m(sp + 1).eq('x') | m(sp + 1).eq('X')),
                |p| {
                    p.set(base, 16);
                    p.inc(sp, 2);
                },
            );
            p.set(val, 0);
            p.set(got, 0);
            p.while_(1, |p| {
                p.set(dg, 99);
                p.if_(digit(m(sp)), |p| p.set(dg, m(sp) - '0'));
                p.if_(m(sp).ge('a') & m(sp).le('f'), |p| {
                    p.set(dg, m(sp) - 'a' + 10)
                });
                p.if_(m(sp).ge('A') & m(sp).le('F'), |p| {
                    p.set(dg, m(sp) - 'A' + 10)
                });
                p.if_(dg.ge(base), |p| p.brk());
                p.set(val, val * base + dg);
                p.set(got, 1);
                p.inc(sp, 1);
            });
            p.if_(got.eq(0), |p| p.brk());
            p.if_(neg, |p| p.set(val, -val));
            p.set(m(m(ap)), val);
            p.inc(ap, 1);
            p.inc(cnt, 1);
        });
        p.ret(cnt);
    });
}

/// Lump name as one number: up to 8 chars (7-bit), char k weighted 128^k.
fn name_key(name: &[u8]) -> i64 {
    name.iter().take(8).rev().fold(0, |acc, &c| {
        assert!(c < 128, "non-ASCII lump name");
        acc * 128 + c.to_ascii_uppercase() as i64
    })
}

/// W_* over the baked lumps. Each lump sits in the heap behind a fake zone
/// header (id ZONEID, neighbours = a used "wall" block) so DOOM's
/// Z_ChangeTag/Z_Free on cached lumps work without touching the zone.
fn define_wad(p: &mut Prog, s: &Symbols, wad: &Baked, malloc: Fun) {
    let f = |n: &str| s.func(n);
    let ierror = f("I_Error");
    let num = wad.lumps.len() as i64;
    let mb = s.size("memblock_t");
    let (mb_size, mb_user, mb_tag, mb_id, mb_next, mb_prev) = (
        s.field("memblock_t", "size"),
        s.field("memblock_t", "user"),
        s.field("memblock_t", "tag"),
        s.field("memblock_t", "id"),
        s.field("memblock_t", "next"),
        s.field("memblock_t", "prev"),
    );
    // Z_ChangeTag2 demands an owner pointer >= 0x100 for purgable tags
    let here = p.alloc(0);
    if here < 0x100 {
        p.alloc(0x100 - here);
    }
    let owner = p.var(); // Z_Free clears *user: this cell
    let wall = p.alloc(mb);
    p.set_init(wall + mb_user, &[owner.0]);

    let li = s.size("lumpinfo_t");
    let (li_name, li_size) = (s.field("lumpinfo_t", "name"), s.field("lumpinfo_t", "size"));
    let mut info = vec![0; (num * li) as usize];
    let (mut addrs, mut lens, mut keys) = (vec![], vec![], vec![]);
    for (j, l) in wad.lumps.iter().enumerate() {
        let len = l.data.len() as i64;
        let mut cells = vec![0; mb as usize];
        cells[mb_size as usize] = mb + len;
        cells[mb_user as usize] = owner.0;
        cells[mb_tag as usize] = PU_STATIC;
        cells[mb_id as usize] = ZONEID;
        cells[mb_next as usize] = wall;
        cells[mb_prev as usize] = wall;
        cells.extend(&l.data);
        addrs.push(p.arr_init(&cells).0 + mb);
        lens.push(len);
        let name = &l.name.as_bytes()[..l.name.len().min(8)];
        keys.push(name_key(name));
        let rec = j * li as usize;
        for (k, &ch) in name.iter().enumerate() {
            info[rec + li_name as usize + k] = ch.to_ascii_uppercase() as i64;
        }
        info[rec + li_size as usize] = len;
    }
    let info = p.arr_init(&info).0;
    let (addrs, lens, keys) = (p.arr_init(&addrs), p.arr_init(&lens), p.arr_init(&keys));

    let (numlumps, lumpinfo, lumpcache) = (
        V(s.global("numlumps")),
        V(s.global("lumpinfo")),
        V(s.global("lumpcache")),
    );
    let adding = cstr(p, " adding %s\n");
    let printf = f("printf");
    let [names, lump, dest, tag, name, i, c, key, mul] = vars(p);
    p.def(f("W_InitMultipleFiles"), &[names], |p| {
        // every named file "is" the baked IWAD; DOOM only ever passes doom1.wad
        p.while_(m(names).ne(0), |p| {
            p.eval(call_var(printf, vec![adding.into()], vec![m(names)]));
            p.inc(names, 1);
        });
        p.set(numlumps, num);
        p.set(lumpinfo, info);
        p.set(lumpcache, malloc.call([num.into()]));
    });
    p.def(f("W_Reload"), &[], |_| {}); // no "~" reload files without a file system

    let check = f("W_CheckNumForName");
    p.def(check, &[name], |p| {
        p.set(key, 0);
        p.set(mul, 1);
        p.for_(i, 0, 8, |p| {
            p.set(c, m(name + i));
            p.if_(c.eq(0), |p| p.brk());
            upcase(p, c);
            p.inc(key, c * mul);
            p.set(mul, mul * 128);
        });
        // scan backwards: the last lump with the name wins (PWAD overrides)
        p.set(i, num - 1);
        p.while_(i.ge(0) & keys.at(i).ne(key), |p| p.inc(i, -1));
        p.ret(i);
    });
    let notfound = cstr(p, "W_GetNumForName: %s not found!");
    let getnum = f("W_GetNumForName");
    p.def(getnum, &[name], |p| {
        p.set(i, check.call([name.into()]));
        p.if_(i.eq(-1), |p| {
            p.eval(call_var(ierror, vec![notfound.into()], vec![name.into()]))
        });
        p.ret(i);
    });
    let range = |p: &mut Prog, msg: i64| {
        p.if_(lump.lt(0) | lump.ge(num), |p| {
            p.eval(call_var(ierror, vec![msg.into()], vec![lump.into()]))
        });
    };
    let (e_len, e_read, e_cache) = (
        cstr(p, "W_LumpLength: %i >= numlumps"),
        cstr(p, "W_ReadLump: %i >= numlumps"),
        cstr(p, "W_CacheLumpNum: %i >= numlumps"),
    );
    p.def(f("W_LumpLength"), &[lump], |p| {
        range(p, e_len);
        p.ret(lens.at(lump));
    });
    p.def(f("W_ReadLump"), &[lump, dest], |p| {
        range(p, e_read);
        p.set(c, addrs.at(lump));
        p.for_(i, 0, lens.at(lump), |p| p.set(m(dest + i), m(c + i)));
    });
    let cache = f("W_CacheLumpNum");
    p.def(cache, &[lump, tag], |p| {
        range(p, e_cache);
        p.set(c, addrs.at(lump));
        // undo a previous Z_Free of this lump (the owner is the cell's address)
        p.set(m(c - mb + mb_user), owner.0);
        p.set(m(c - mb + mb_tag), tag);
        p.set(m(c - mb + mb_id), ZONEID);
        p.ret(c);
    });
    p.def(f("W_CacheLumpName"), &[name, tag], |p| {
        p.ret(cache.call([getnum.call([name.into()]), tag.into()]))
    });
}

/// Top-level code standing in for i_main.c main(): myargc = 1,
/// myargv = {"doom", NULL}, then D_DoomMain().
pub fn entry(p: &mut Prog, s: &Symbols) {
    let doom = cstr(p, "doom");
    let argv = p.arr_init(&[doom, 0]).0;
    p.set(V(s.global("myargc")), 1);
    p.set(V(s.global("myargv")), argv);
    p.call(s.func("D_DoomMain"), []);
}

#[cfg(test)]
mod tests {
    use super::*;
    use wadbake::Lump;
    use wsdoom::vm;

    struct T {
        p: Prog,
        s: Symbols,
    }

    fn setup(lumps: &[(&str, &[i64])]) -> T {
        let mut p = Prog::new();
        let mut s = Symbols::default();
        for b in BUILTINS {
            s.funcs.insert(b.name.into(), p.func(b.arity));
        }
        for g in [
            "numlumps",
            "lumpinfo",
            "lumpcache",
            "myargc",
            "myargv",
            "wipe_scr_start",
            "wipe_scr_end",
            "wipe_scr",
        ] {
            let a = p.var().0;
            s.globals.insert(g.into(), a);
        }
        for (ty, fields) in [
            ("lumpinfo_t", &["name", "handle", "position", "size"][..]),
            (
                "memblock_t",
                &["size", "user", "tag", "id", "next", "prev"][..],
            ),
        ] {
            let mut off = 0;
            for &fl in fields {
                s.fields.insert((ty.into(), fl.into()), off);
                off += if fl == "name" { 8 } else { 1 };
            }
            s.sizes.insert(ty.into(), off);
        }
        // I_Error stand-in: print "ERR " + message, stop
        let ie = p.func(1);
        s.funcs.insert("I_Error".into(), ie);
        // M_Random stand-in: a fixed byte sequence (k*37+11) % 256
        let mr = p.func(0);
        s.funcs.insert("M_Random".into(), mr);
        let wad = Baked {
            lumps: lumps
                .iter()
                .map(|(n, d)| Lump {
                    name: n.to_string(),
                    data: d.to_vec(),
                })
                .collect(),
            palettes: vec![],
        };
        define(&mut p, &s, &wad);
        let (fp, args, cnt) = (p.var(), p.arr(MAX_VARARGS), p.var());
        let vfprintf = s.func("vfprintf");
        p.def_variadic(ie, &[fp], args, cnt, |p| {
            p.puts("ERR ");
            p.eval(vfprintf.call([2.into(), fp.into(), args.0.into()]));
            p.exit();
        });
        let idx = p.var();
        p.def(mr, &[], |p| {
            p.inc(idx, 1);
            p.ret((idx * 37 + 11 - 37) % 256);
        });
        T { p, s }
    }

    impl T {
        fn f(&self, n: &str) -> Fun {
            self.s.func(n)
        }
        fn str(&mut self, s: &str) -> E {
            cstr(&mut self.p, s).into()
        }
        /// printf(fmt, args...)
        fn printf(&mut self, fmt: &str, args: Vec<E>) {
            let fmt = self.str(fmt);
            let pf = self.f("printf");
            self.p.eval(call_var(pf, vec![fmt], args));
        }
        fn run(self) -> String {
            let code = vm::parse(&vm::encode(&self.p.finish())).unwrap();
            let mut out = Vec::new();
            vm::run(&code, &mut |_: &mut Vec<u8>| -1, &mut out).unwrap();
            String::from_utf8(out).unwrap()
        }
    }

    #[test]
    fn sprintf_formats() {
        let mut t = setup(&[]);
        let buf = t.p.alloc(200);
        let fmt =
            t.str("%d|%5d|%-5d|%05d|%.2d|%2.2d|%.3d|%s|%.2s|%-4s|%3s|%c|%x|%X|%i%%|%u|%lx|%7i|%");
        let (hi, abc, x) = (t.str("hi"), t.str("abc"), t.str("x"));
        let args: Vec<E> = vec![
            (-42).into(),
            42.into(),
            42.into(),
            (-42).into(),
            7.into(),
            5.into(),
            (-7).into(),
            hi.clone(),
            abc,
            hi,
            x,
            'Z'.into(),
            255.into(),
            0xBEEF.into(),
            (-3).into(),
            (-1).into(),
            (-1).into(),
            0.into(),
        ];
        let sprintf = t.f("sprintf");
        let n = t.p.var();
        t.p.set(n, call_var(sprintf, vec![buf.into(), fmt], args));
        t.printf("%s#%d\n", vec![buf.into(), n.into()]);
        t.printf("[%s][%c]\n", vec![0.into(), 'q'.into()]);
        let want = "-42|   42|42   |-0042|07|05|-007|hi|ab|hi  |  x|Z|ff|BEEF|-3%|4294967295|ffffffff|      0|";
        assert_eq!(t.run(), format!("{want}#{}\n[(null)][q]\n", want.len()));
    }

    #[test]
    fn string_functions() {
        let mut t = setup(&[]);
        let pairs = [
            ("Hello", "hELLO"),
            ("abc", "abd"),
            ("abc", "ab"),
            ("", "a"),
            ("STAR", "star1"),
        ];
        let mut want = String::new();
        for (a, b) in pairs {
            let (ea, eb) = (t.str(a), t.str(b));
            for (fname, lim) in [
                ("strcmp", None),
                ("strcasecmp", None),
                ("strncasecmp", Some(4usize)),
                ("strncmp", Some(2)),
            ] {
                let fun = t.f(fname);
                let mut args = vec![ea.clone(), eb.clone()];
                if let Some(l) = lim {
                    args.push((l as i64).into());
                }
                t.printf("%d ", vec![fun.call_vec(args)]);
                let (la, lb) = match lim {
                    Some(l) => (&a[..a.len().min(l)], &b[..b.len().min(l)]),
                    None => (a, b),
                };
                let (la, lb) = if fname.contains("case") {
                    (la.to_ascii_uppercase(), lb.to_ascii_uppercase())
                } else {
                    (la.into(), lb.into())
                };
                want += &format!("{} ", la.cmp(&lb) as i64);
            }
        }
        // results are differences; normalise via sign in Rust instead
        let got = t.run();
        let got: Vec<i64> = got
            .split_whitespace()
            .map(|n| n.parse::<i64>().unwrap().signum())
            .collect();
        let want: Vec<i64> = want
            .split_whitespace()
            .map(|n| n.parse().unwrap())
            .collect();
        assert_eq!(got, want);

        let mut t = setup(&[]);
        let buf = t.p.alloc(40);
        let (s1, s2) = (t.str("doom"), t.str(".wad"));
        let (strcpy, strcat, strlen, strncpy, toupper, atoi, abs) = (
            t.f("strcpy"),
            t.f("strcat"),
            t.f("strlen"),
            t.f("strncpy"),
            t.f("toupper"),
            t.f("atoi"),
            t.f("abs"),
        );
        t.p.call(strcpy, [buf.into(), s1.clone()]);
        t.p.call(strcat, [buf.into(), s2]);
        t.printf("%s %d ", vec![buf.into(), strlen.call([buf.into()])]);
        t.p.call(strncpy, [buf.into(), s1, 6.into()]);
        let num = t.str("  -1234x");
        t.printf(
            "%s %c %c %c %d %d\n",
            vec![
                buf.into(),
                m(buf + 6),
                toupper.call(['q'.into()]),
                toupper.call(['['.into()]),
                atoi.call([num]),
                abs.call([(-9).into()]),
            ],
        );
        assert_eq!(t.run(), "doom.wad 8 doom a Q [ -1234 9\n");
    }

    #[test]
    fn memory_functions() {
        let mut t = setup(&[]);
        let (memset, memcpy, malloc, realloc) =
            (t.f("memset"), t.f("memcpy"), t.f("malloc"), t.f("realloc"));
        let (a, b) = (t.p.var(), t.p.var());
        t.p.set(a, malloc.call([5.into()]));
        t.p.call(memset, [a.into(), (-1).into(), 5.into()]);
        t.p.call(memset, [a.into(), 0xff.into(), 2.into()]);
        t.p.set(b, malloc.call([3.into()]));
        t.p.call(memcpy, [b.into(), a + 1, 3.into()]);
        t.p.set(a, realloc.call([a.into(), 8.into()]));
        t.p.set(m(a + 7), 70);
        t.p.set(b, realloc.call([b.into(), 2.into()]));
        t.p.set(a, realloc.call([a.into(), 9.into()]));
        let alloca = t.f("alloca");
        t.printf(
            "%d %d %d %d %d %d %d %d %d | %d %d\n",
            (0..9).map(|i| m(a + i)).chain([m(b), m(b + 1)]).collect(),
        );
        t.printf("%d\n", vec![alloca.call([4.into()]).gt(0)]);
        assert_eq!(t.run(), "255 255 -1 -1 -1 0 0 70 0 | 255 -1\n1\n");
    }

    #[test]
    fn files_and_environment() {
        let mut t = setup(&[]);
        let (wad, other, home, dw, r) = (
            t.str("./doom1.wad"),
            t.str("./doom.wad"),
            t.str("HOME"),
            t.str("DOOMWADDIR"),
            t.str("r"),
        );
        let (access, getenv, fopen, open) =
            (t.f("access"), t.f("getenv"), t.f("fopen"), t.f("open"));
        let (line, scanfmt) = (t.str("mouse_sensitivity 0x1f  x"), t.str("%79s %i"));
        let sscanf = t.f("sscanf");
        let (a, n, word) = (t.p.var(), t.p.var(), t.p.alloc(20));
        t.p.set(
            n,
            call_var(sscanf, vec![line, scanfmt], vec![word.into(), a.0.into()]),
        );
        t.printf(
            "%d %d %s %d %d %d %d|%s %d %d\n",
            vec![
                access.call([wad, 4.into()]),
                access.call([other, 4.into()]),
                getenv.call([home]),
                getenv.call([dw]),
                fopen.call([0.into(), r]),
                call_var(open, vec![0.into(), 0.into()], vec![]),
                call_var(open, vec![0.into(), 0.into()], vec![0o666.into()]),
                word.into(),
                a.into(),
                n.into(),
            ],
        );
        assert_eq!(t.run(), "0 -1 . 0 0 -1 -1|mouse_sensitivity 31 2\n");
    }

    #[test]
    fn wad_lookup_and_cache() {
        let mut t = setup(&[
            ("PLAYPAL", &[1, 2, 3]),
            ("E1M1", &[]),
            ("PLAYPAL", &[9, 8]),
            ("SKY1", &[4, 5, 6, 7]),
            ("ABCDEFGH", &[42]),
        ]);
        let (init, check, getnum, len, read, cache, cname) = (
            t.f("W_InitMultipleFiles"),
            t.f("W_CheckNumForName"),
            t.f("W_GetNumForName"),
            t.f("W_LumpLength"),
            t.f("W_ReadLump"),
            t.f("W_CacheLumpNum"),
            t.f("W_CacheLumpName"),
        );
        let wadname = t.str("./doom1.wad");
        let files = t.p.arr_init(&[0, 0]).0;
        t.p.set(m(files), wadname);
        t.p.call(init, [files.into()]);
        let names = [
            "playpal",
            "PlayPal",
            "e1m1",
            "sky1",
            "sky",
            "nothere",
            "ABCDEFGHIJ",
        ];
        let mut args = vec![];
        for n in names {
            let e = t.str(n);
            args.push(check.call([e]));
        }
        t.printf("%d %d %d %d %d %d %d\n", args);
        let numlumps = V(t.s.global("numlumps"));
        let lumpinfo = V(t.s.global("lumpinfo"));
        let li = t.s.size("lumpinfo_t");
        let size = t.s.field("lumpinfo_t", "size");
        t.printf(
            "%d %c%c %d\n",
            vec![
                numlumps.into(),
                m(lumpinfo + li * 4),
                m(lumpinfo + li * 4 + 7),
                m(lumpinfo + li * 3 + size),
            ],
        );
        let (p, buf) = (t.p.var(), t.p.alloc(4));
        t.p.set(p, cache.call([3.into(), 101.into()]));
        t.p.call(read, [2.into(), buf.into()]);
        let sky = t.str("SKY1");
        let pal = t.str("playpal");
        t.printf(
            "%d %d %d %d %d %d %d\n",
            vec![
                len.call([3.into()]),
                m(p),
                m(p + 3),
                m(buf),
                m(buf + 1),
                cname.call([sky, 1.into()]).eq(p),
                m(cname.call([pal, 1.into()])),
            ],
        );
        // fake zone header in front of a cached lump
        let mb = t.s.size("memblock_t");
        t.printf("%x %d\n", vec![m(p - mb + 3), m(m(p - mb + 4) + 1).ne(0)]);
        let missing = t.str("FOO");
        t.p.call(getnum, [missing]);
        assert_eq!(
            t.run(),
            " adding ./doom1.wad\n2 2 1 3 -1 -1 4\n5 AH 4\n4 4 7 9 8 1 9\n1d4a11 1\nERR W_GetNumForName: FOO not found!"
        );
    }

    #[test]
    fn entry_sets_args() {
        let mut p = Prog::new();
        let mut s = Symbols::default();
        for g in ["myargc", "myargv"] {
            let a = p.var().0;
            s.globals.insert(g.into(), a);
        }
        let dm = p.func(0);
        s.funcs.insert("D_DoomMain".into(), dm);
        let (argc, argv) = (V(s.global("myargc")), V(s.global("myargv")));
        let i = p.var();
        p.def(dm, &[], |p| {
            p.putn(argc);
            p.set(i, m(argv));
            p.while_(m(i).ne(0), |p| {
                p.putc(m(i));
                p.inc(i, 1);
            });
            p.putn(m(argv + 1));
        });
        entry(&mut p, &s);
        let t = T { p, s };
        assert_eq!(t.run(), "1doom0");
    }

    #[test]
    fn fixed_div2_truncates() {
        let cases: [(i64, i64); 6] = [
            (3 << 16, 2 << 16),
            (-7, 3),
            (7, -3),
            (-1 << 16, -3),
            (1 << 20, 1 << 24),
            (100, 7),
        ];
        let mut t = setup(&[]);
        let fd = t.f("FixedDiv2");
        let mut want = String::new();
        for (a, b) in cases {
            t.printf("%d ", vec![fd.call([a.into(), b.into()])]);
            want += &format!("{} ", (a as f64 / b as f64 * 65536.0) as i64);
        }
        t.p.call(fd, [(1i64 << 30).into(), 1.into()]);
        assert_eq!(t.run(), format!("{want}ERR FixedDiv: divide by zero"));
        let mut t = setup(&[]);
        let fd = t.f("FixedDiv2");
        t.p.call(fd, [5.into(), 0.into()]);
        assert_eq!(t.run(), "ERR FixedDiv: divide by zero");
    }

    #[test]
    fn file_stubs() {
        let mut t = setup(&[]);
        let (setbuf, filelength, readfile) = (t.f("setbuf"), t.f("filelength"), t.f("M_ReadFile"));
        let name = t.str("x.lmp");
        t.printf(
            "%d %d|",
            vec![
                setbuf.call([1.into(), 0.into()]),
                filelength.call([3.into()]),
            ],
        );
        t.p.call(readfile, [name, 0.into()]);
        assert_eq!(t.run(), "0 -1|ERR Couldn't read file x.lmp");
    }

    #[test]
    fn savegame_files_live_in_memory() {
        let mut t = setup(&[]);
        let (open, read, write, close, readfile) = (
            t.f("open"),
            t.f("read"),
            t.f("write"),
            t.f("close"),
            t.f("M_ReadFile"),
        );
        let (sav, other) = (t.str("doomsav0.dsg"), t.str("doomsav1.dsg"));
        let src = t.p.arr_init(&[7, -8, 300, 4]).0;
        let (fd, n, buf, got) = (t.p.var(), t.p.var(), t.p.alloc(8), t.p.var());
        let creat = 1 | 64 | 512; // O_WRONLY | O_CREAT | O_TRUNC
                                  // write 4 cells, then rewrite (truncate) with 3
        for k in [4, 3] {
            t.p.set(
                fd,
                call_var(open, vec![sav.clone(), creat.into()], vec![0o666.into()]),
            );
            t.p.eval(write.call([fd.into(), src.into(), k.into()]));
            t.p.eval(close.call([fd.into()]));
        }
        t.p.set(fd, call_var(open, vec![sav.clone(), 0.into()], vec![]));
        t.p.set(n, read.call([fd.into(), buf.into(), 8.into()]));
        t.printf(
            "%d %d %d %d %d|",
            vec![
                n.into(),
                m(buf),
                m(buf + 1),
                m(buf + 2),
                read.call([fd.into(), buf.into(), 8.into()]),
            ],
        );
        t.p.set(n, readfile.call([sav, got.0.into()]));
        t.printf(
            "%d %d %d|%d\n",
            vec![
                n.into(),
                m(got),
                m(got + 2),
                call_var(open, vec![other, 0.into()], vec![]),
            ],
        );
        assert_eq!(t.run(), "3 7 -8 300 0|3 7 300|-1\n");
    }

    /// f_wipe.c's melt, literally: 2-pixel `short`s, column-major copies.
    fn melt_reference(w: usize, h: usize, start: &[i64], end: &[i64]) -> Vec<Vec<i64>> {
        let rnd = |k: usize| ((k * 37 + 11) % 256) as i64;
        let shorts = |px: &[i64]| px.chunks(2).map(|c| (c[0], c[1])).collect::<Vec<_>>();
        let colmajor = |a: &[(i64, i64)], width: usize| {
            let mut d = a.to_vec();
            for y in 0..h {
                for x in 0..width {
                    d[x * h + y] = a[y * width + x];
                }
            }
            d
        };
        let mut scr = shorts(start);
        let (st, en) = (
            colmajor(&shorts(start), w / 2),
            colmajor(&shorts(end), w / 2),
        );
        let mut y = vec![-(rnd(0) % 16)];
        for i in 1..w {
            let mut v = y[i - 1] + rnd(i) % 3 - 1;
            if v > 0 {
                v = 0;
            } else if v == -16 {
                v = -15;
            }
            y.push(v);
        }
        let (w2, h2) = (w / 2, h as i64);
        let mut frames = vec![];
        loop {
            let mut done = true;
            for i in 0..w2 {
                if y[i] < 0 {
                    y[i] += 1;
                    done = false;
                } else if y[i] < h2 {
                    let mut dy = if y[i] < 16 { y[i] + 1 } else { 8 };
                    if y[i] + dy >= h2 {
                        dy = h2 - y[i];
                    }
                    for j in 0..dy as usize {
                        scr[(y[i] as usize + j) * w2 + i] = en[i * h + y[i] as usize + j];
                    }
                    y[i] += dy;
                    for j in 0..(h2 - y[i]) as usize {
                        scr[(y[i] as usize + j) * w2 + i] = st[i * h + j];
                    }
                    done = false;
                }
            }
            let mut f: Vec<i64> = scr.iter().flat_map(|&(a, b)| [a, b]).collect();
            f.push(done as i64);
            frames.push(f);
            if done {
                return frames;
            }
        }
    }

    #[test]
    fn melt_matches_original() {
        // tall enough that columns pass y = 16 and move 8 rows per tick
        let (w, h) = (8usize, 40usize);
        let start: Vec<i64> = (0..w * h).map(|k| (k * 7 % 251) as i64).collect();
        let end: Vec<i64> = (0..w * h).map(|k| (k * 11 % 241 + 3) as i64).collect();
        let frames = melt_reference(w, h, &start, &end);

        let mut t = setup(&[]);
        let (sa, ea, scr) = (
            t.p.arr_init(&start).0,
            t.p.arr_init(&end).0,
            t.p.alloc((w * h) as i64),
        );
        for (g, v) in [
            ("wipe_scr_start", sa),
            ("wipe_scr_end", ea),
            ("wipe_scr", scr),
        ] {
            let a = V(t.s.global(g));
            t.p.set(a, v);
        }
        let (init, step, exit) = (
            t.f("wipe_initMelt"),
            t.f("wipe_doMelt"),
            t.f("wipe_exitMelt"),
        );
        let (wi, hi) = (w as i64, h as i64);
        t.p.call(init, [wi.into(), hi.into(), 0.into()]);
        let (done, k) = (t.p.var(), t.p.var());
        t.p.while_(done.eq(0), |p| {
            p.set(done, step.call([wi.into(), hi.into(), 1.into()]));
            p.for_(k, 0, wi * hi, |p| {
                p.putn(m(scr + k));
                p.putc(',');
            });
            p.putn(done);
            p.putc('\n');
        });
        t.p.call(exit, [wi.into(), hi.into(), 0.into()]);
        let want: String = frames
            .iter()
            .map(|f| {
                f.iter()
                    .map(|v| v.to_string())
                    .collect::<Vec<_>>()
                    .join(",")
                    + "\n"
            })
            .collect();
        assert_eq!(t.run(), want);
    }
}
