//! The I_* platform layer for compiled DOOM, in the wsdoom DSL: terminal
//! video, keyboard and clock over the runner byte protocol, net/sound stubs.
//! Replaces linuxdoom's i_system.c, i_video.c, i_sound.c and i_net.c.
//!
//! Input protocol: a byte >= 0x80 reports that byte-0x80 tics elapsed, any
//! other byte is a key. If the first unit read is a key (a plain interpreter
//! that sends no time bytes) every key advances the clock 4 tics instead.

use wsdoom::cc::symbols::{Builtin, Symbols};
use wsdoom::dsl::{getc, max, min, Ex, Prog, E, V};
use wsdoom::vm::Ins;

const fn b(name: &'static str, arity: usize) -> Builtin {
    Builtin {
        name,
        arity,
        variadic: false,
    }
}

/// Every function declared in i_system.h, i_video.h, i_sound.h and i_net.h.
pub const BUILTINS: &[Builtin] = &[
    // i_system.h
    b("I_Init", 0),
    b("I_ZoneBase", 1),
    b("I_GetTime", 0),
    b("I_StartFrame", 0),
    b("I_StartTic", 0),
    b("I_BaseTiccmd", 0),
    b("I_Quit", 0),
    b("I_AllocLow", 1),
    b("I_Tactile", 3),
    Builtin {
        name: "I_Error",
        arity: 1,
        variadic: true,
    },
    // i_video.h
    b("I_InitGraphics", 0),
    b("I_ShutdownGraphics", 0),
    b("I_SetPalette", 1),
    b("I_UpdateNoBlit", 0),
    b("I_FinishUpdate", 0),
    b("I_WaitVBL", 1),
    b("I_ReadScreen", 1),
    b("I_BeginRead", 0),
    b("I_EndRead", 0),
    // i_sound.h
    b("I_InitSound", 0),
    b("I_UpdateSound", 0),
    b("I_SubmitSound", 0),
    b("I_ShutdownSound", 0),
    b("I_SetChannels", 0),
    b("I_GetSfxLumpNum", 1),
    b("I_StartSound", 5),
    b("I_StopSound", 1),
    b("I_SoundIsPlaying", 1),
    b("I_UpdateSoundParams", 4),
    b("I_InitMusic", 0),
    b("I_ShutdownMusic", 0),
    b("I_SetMusicVolume", 1),
    b("I_PauseSong", 1),
    b("I_ResumeSong", 1),
    b("I_RegisterSong", 1),
    b("I_PlaySong", 2),
    b("I_StopSong", 1),
    b("I_UnRegisterSong", 1),
    // i_net.h
    b("I_InitNetwork", 0),
    b("I_NetCmd", 0),
];

/// Builtins with a real body; every other one does nothing and returns 0.
const IMPLEMENTED: &[&str] = &[
    "I_ZoneBase",
    "I_GetTime",
    "I_StartFrame",
    "I_StartTic",
    "I_BaseTiccmd",
    "I_Quit",
    "I_AllocLow",
    "I_Error",
    "I_InitGraphics",
    "I_SetPalette",
    "I_FinishUpdate",
    "I_ReadScreen",
    "I_StartSound",
    "I_RegisterSong",
    "I_GetSfxLumpNum",
    "I_InitNetwork",
];

// doomdef.h
const KEY_RIGHTARROW: i64 = 0xae;
const KEY_LEFTARROW: i64 = 0xac;
const KEY_UPARROW: i64 = 0xad;
const KEY_DOWNARROW: i64 = 0xaf;
const KEY_ESCAPE: i64 = 27;
const KEY_ENTER: i64 = 13;
const KEY_TAB: i64 = 9;
const KEY_F1: i64 = 0x80 + 0x3b;
const KEY_F6: i64 = 0x80 + 0x40;
const KEY_F11: i64 = 0x80 + 0x57;
const KEY_F12: i64 = 0x80 + 0x58;
const KEY_BACKSPACE: i64 = 127;
const KEY_RSHIFT: i64 = 0x80 + 0x36;
const KEY_RCTRL: i64 = 0x80 + 0x1d;
// d_event.h evtype_t
const EV_KEYDOWN: i64 = 0;
const EV_KEYUP: i64 = 1;
const PU_CACHE: i64 = 101; // z_zone.h
const DOOMCOM_ID: i64 = 0x12345678; // d_net.h
const SCREENWIDTH: i64 = 320;
const SCREENHEIGHT: i64 = 200;

const ZONE_CELLS: i64 = 6_000_000;
const TICS_PER_KEY: i64 = 4;
const HOLD_FIRST: i64 = 12;
const HOLD_REPEAT: i64 = 6;
const DSR_WAIT: i64 = 35;
/// I_GetTime calls without new time before it blocks for it: busy-wait
/// loops; a frame makes about 7 calls plus one per tic it runs (d_net.c).
const SPIN_LIMIT: i64 = 32;
const QLEN: i64 = 64;
const NO_BYTE: i64 = -1000;
const ERR_BUF: i64 = 1024;
const ERR_ARGS: i64 = 16;

fn mem(addr: impl Into<E>) -> E {
    E::Load(Box::new(addr.into()))
}

fn cstr(p: &mut Prog, s: &str) -> i64 {
    let cells: Vec<i64> = s.bytes().map(i64::from).chain([0]).collect();
    p.arr_init(&cells).0
}

/// Define every builtin in `BUILTINS`. Needs C globals `screens`, `doomcom`,
/// `netgame`, `gammatable`, `usegamma`, types `event_t`, `ticcmd_t`,
/// `doomcom_t`, `sfxinfo_t` and functions `malloc`, `sprintf`,
/// `D_PostEvent`, `W_CacheLumpName`.
pub fn define(p: &mut Prog, s: &Symbols, wad: &wadbake::Baked) {
    assert!(!wad.palettes.is_empty(), "no palettes baked");
    for bi in BUILTINS.iter().filter(|bi| !IMPLEMENTED.contains(&bi.name)) {
        let params: Vec<V> = (0..bi.arity).map(|_| p.var()).collect();
        p.def(s.func(bi.name), &params, |_| {});
    }

    // ---- sound: handles the original returned, nothing played
    let sp: Vec<V> = (0..5).map(|_| p.var()).collect();
    p.def(s.func("I_StartSound"), &sp, |p| p.ret(sp[0]));
    let song = p.var();
    p.def(s.func("I_RegisterSong"), &[song], |p| p.ret(1));
    // the original I_InitSound loads every sound's data; mark a sound as
    // loaded when s_sound.c first asks for its lump, or it prints
    // "16bit and not pre-cached - wtf?" each time the sound starts
    let sfx = p.var();
    p.def(s.func("I_GetSfxLumpNum"), &[sfx], |p| {
        p.set(mem(sfx + s.field("sfxinfo_t", "data")), 1);
        p.ret(0);
    });

    // ---- memory
    let zone = p.alloc(ZONE_CELLS);
    let size = p.var();
    p.def(s.func("I_ZoneBase"), &[size], |p| {
        p.set(mem(size), ZONE_CELLS);
        p.ret(zone);
    });
    let (len, ptr, i) = (p.var(), p.var(), p.var());
    let malloc = s.func("malloc");
    p.def(s.func("I_AllocLow"), &[len], |p| {
        p.set(ptr, malloc.call([len.into()]));
        p.for_(i, 0, len, |p| p.set(mem(ptr + i), 0));
        p.ret(ptr);
    });
    let cmd = p.alloc(s.size("ticcmd_t"));
    p.def(s.func("I_BaseTiccmd"), &[], |p| p.ret(cmd));

    // ---- terminal restore, quit, error
    let gfx = p.var();
    let restore = p.func(0);
    p.def(restore, &[], |p| {
        p.puts("\x1b[0m\x1b[?25h");
        p.if_(gfx.ne(0), |p| p.puts("\r\n"));
    });
    let quit = s.func("I_Quit");
    p.def(quit, &[], |p| {
        p.call(restore, []);
        p.exit();
    });
    let (efmt, eargs, ecount, ei) = (p.var(), p.arr(ERR_ARGS), p.var(), p.var());
    let ebuf = p.alloc(ERR_BUF);
    let sprintf = s.func("sprintf");
    p.def_variadic(s.func("I_Error"), &[efmt], eargs, ecount, |p| {
        p.call(restore, []);
        p.puts("Error: ");
        // sprintf(ebuf, fmt, args...): forward the variadic arguments as received
        p.push(ebuf);
        p.push(efmt);
        p.for_(ei, 0, ecount, |p| p.push(eargs.at(ei)));
        p.push(ecount);
        p.raw(Ins::Call(sprintf.label()));
        p.raw(Ins::Drop);
        p.set(ei, ebuf);
        p.while_(mem(ei).ne(0), |p| {
            p.putc((mem(ei) + 256) % 256);
            p.inc(ei, 1);
        });
        p.putc('\n');
        p.exit();
    });

    // ---- input: clock, byte reader, key decoder
    let clock = p.var();
    let last = p.var_init(-1); // value I_GetTime returned last
    let spins = p.var(); // I_GetTime calls since the time it returned last changed
    let mode = p.var(); // 0 undecided, 1 time bytes, 2 turn-based
    let pend = p.var_init(NO_BYTE); // pushed-back byte
    let queue = p.arr(QLEN);
    let (qhead, qtail) = (p.var(), p.var());
    let (got, rows, cols) = (p.var(), p.var(), p.var());
    // a window host (the browser page) answers the size query with 0;0: it
    // gets raw frames and palettes and sends real presses and releases
    let raw = p.var();
    let post = p.func(2); // post(event type, key) to D_PostEvent
    let arrows = p.arr_init(&[KEY_UPARROW, KEY_DOWNARROW, KEY_RIGHTARROW, KEY_LEFTARROW]);
    let mut tilde = [0; 25]; // ESC [ n ~ function keys
    for n in 0..5 {
        tilde[11 + n] = KEY_F1 + n as i64;
        tilde[17 + n] = KEY_F6 + n as i64;
    }
    tilde[23] = KEY_F11;
    tilde[24] = KEY_F12;
    let tilde = p.arr_init(&tilde);

    let readb = p.func(0);
    let t = p.var();
    p.def(readb, &[], |p| {
        p.if_(pend.ne(NO_BYTE), |p| {
            p.set(t, pend);
            p.set(pend, NO_BYTE);
            p.ret(t);
        });
        p.ret(getc());
    });

    let key = p.func(1);
    let k = p.var();
    p.def(key, &[k], |p| {
        p.if_((qtail - qhead).lt(QLEN), |p| {
            p.set(queue.at(qtail % QLEN), k);
            p.inc(qtail, 1);
        });
        p.if_(mode.eq(0), |p| p.set(mode, 2));
        p.if_(mode.eq(2), |p| p.inc(clock, TICS_PER_KEY));
    });

    // Read one unit: a time report, a key (or escape sequence) or a DSR reply.
    let pump = p.func(0);
    let (c, n, a, semi) = (p.var(), p.var(), p.var(), p.var());
    let abort = |p: &mut Prog, x: V| {
        p.if_(x.lt(0) | x.ge(128), |p| {
            p.set(pend, x);
            p.ret(0);
        })
    };
    p.def(pump, &[], |p| {
        p.set(c, readb.call([]));
        // shortcut: end of input counts as one tic so the game keeps running
        p.if_(c.lt(0), |p| {
            p.inc(clock, 1);
            p.ret(0);
        });
        // from a window host: 256 + key pressed, 512 + key released
        p.if_(c.ge(256), |p| {
            p.if_else(
                c.ge(512),
                |p| p.call(post, [EV_KEYUP.into(), c - 512]),
                |p| p.call(post, [EV_KEYDOWN.into(), c - 256]),
            );
            p.ret(0);
        });
        p.if_(c.ge(128), |p| {
            p.if_(mode.ne(2), |p| {
                p.set(mode, 1);
                p.inc(clock, c - 128);
            });
            p.ret(0);
        });
        p.if_(c.eq(3), |p| p.call(quit, []));
        p.if_(c.eq(27), |p| {
            p.set(c, readb.call([]));
            p.if_(c.ne('[') & c.ne('O'), |p| {
                // lone ESC: c (time, end of input or another key) comes next
                p.set(pend, c);
                p.if_(c.ge(128) & mode.eq(0), |p| p.set(mode, 1));
                p.call(key, [KEY_ESCAPE.into()]);
                p.ret(0);
            });
            p.set(n, readb.call([]));
            abort(p, n);
            p.if_(c.eq('O'), |p| {
                p.if_(n.ge('A') & n.le('D'), |p| p.call(key, [arrows.at(n - 'A')]));
                p.if_(n.ge('P') & n.le('S'), |p| p.call(key, [KEY_F1 + (n - 'P')]));
                p.ret(0);
            });
            p.if_(n.eq('['), |p| {
                // linux console F1..F5: ESC [ [ A..E
                p.set(n, readb.call([]));
                abort(p, n);
                p.if_(n.ge('A') & n.le('E'), |p| p.call(key, [KEY_F1 + (n - 'A')]));
                p.ret(0);
            });
            // CSI: parameters "a;c" (or just "c"), then a final byte in n
            p.set(a, 0);
            p.set(c, 0);
            p.set(semi, 0);
            p.while_((n.ge('0') & n.le('9')) | n.eq(';'), |p| {
                p.if_else(
                    n.eq(';'),
                    |p| {
                        p.set(a, c);
                        p.set(c, 0);
                        p.set(semi, 1);
                    },
                    |p| p.set(c, c * 10 + n - '0'),
                );
                p.set(n, readb.call([]));
            });
            abort(p, n);
            p.if_(semi.eq(0), |p| p.set(a, c));
            p.if_(n.ge('A') & n.le('D'), |p| p.call(key, [arrows.at(n - 'A')]));
            p.if_(n.ge('P') & n.le('S') & semi.eq(0), |p| {
                p.call(key, [KEY_F1 + (n - 'P')])
            });
            p.if_(n.eq('~') & a.lt(25), |p| {
                p.if_(tilde.at(a).ne(0), |p| p.call(key, [tilde.at(a)]));
            });
            p.if_(n.eq('R') & semi.ne(0), |p| {
                p.set(rows, a);
                p.set(cols, c);
                p.set(got, 1);
            });
            p.ret(0);
        });
        p.if_(c.ge('A') & c.le('Z'), |p| p.inc(c, 32));
        p.if_(c.eq(13) | c.eq(10), |p| {
            p.call(key, [KEY_ENTER.into()]);
            p.ret(0);
        });
        p.if_(c.eq(9), |p| {
            p.call(key, [KEY_TAB.into()]);
            p.ret(0);
        });
        p.if_(c.eq(127) | c.eq(8), |p| {
            p.call(key, [KEY_BACKSPACE.into()]);
            p.ret(0);
        });
        p.if_(c.eq('z'), |p| {
            p.call(key, [KEY_RCTRL.into()]);
            p.ret(0);
        });
        p.if_(c.ge(32) & c.lt(127), |p| p.call(key, [c.into()]));
    });

    // Reading blocks until a tic passes, so it happens once per frame (in
    // I_StartFrame, which D_DoomLoop calls before TryRunTics) and in
    // busy-wait loops; NetUpdate's many calls in between see the known time.
    p.def(s.func("I_StartFrame"), &[], |p| {
        p.while_(clock.le(last), |p| p.call(pump, []))
    });
    p.def(s.func("I_GetTime"), &[], |p| {
        p.if_else(
            clock.gt(last),
            |p| p.set(spins, 0),
            |p| {
                p.inc(spins, 1);
                p.if_(spins.ge(SPIN_LIMIT), |p| {
                    p.while_(clock.le(last), |p| p.call(pump, []));
                    p.set(spins, 0);
                });
            },
        );
        p.set(last, clock);
        p.ret(clock);
    });

    // ---- events
    let ev = p.alloc(s.size("event_t"));
    let (etype, ekey) = (p.var(), p.var());
    let postevent = s.func("D_PostEvent");
    p.def(post, &[etype, ekey], |p| {
        p.set(mem(ev + s.field("event_t", "type")), etype);
        p.set(mem(ev + s.field("event_t", "data1")), ekey);
        p.set(mem(ev + s.field("event_t", "data2")), 0);
        p.set(mem(ev + s.field("event_t", "data3")), 0);
        p.call(postevent, [ev.into()]);
    });
    let down = p.arr_init(&[-1; 256]); // tic of first press, -1 when up
    let rep = p.arr(256); // tic of latest press
    let (ndown, shift, sk, si) = (p.var(), p.var(), p.var(), p.var());
    p.def(s.func("I_StartTic"), &[], |p| {
        p.if_(shift.eq(0), |p| {
            p.set(shift, 1);
            p.call(post, [EV_KEYDOWN.into(), KEY_RSHIFT.into()]); // always run
        });
        p.while_(qhead.lt(qtail), |p| {
            p.set(sk, queue.at(qhead % QLEN));
            p.inc(qhead, 1);
            p.if_(down.at(sk).lt(0), |p| {
                p.set(down.at(sk), clock);
                p.inc(ndown, 1);
            });
            p.set(rep.at(sk), clock);
            p.call(post, [EV_KEYDOWN.into(), sk.into()]);
        });
        // terminals send no releases: synthesize them
        p.if_(ndown.gt(0), |p| {
            p.for_(si, 0, 256, |p| {
                let held = down.at(si).ge(0);
                let due = clock.ge(max(down.at(si) + HOLD_FIRST, rep.at(si) + HOLD_REPEAT));
                p.if_(held & due, |p| {
                    p.set(down.at(si), -1);
                    p.inc(ndown, -1);
                    p.call(post, [EV_KEYUP.into(), si.into()]);
                });
            });
        });
    });

    // ---- video
    let (w, h, t0, gi) = (p.var(), p.var(), p.var(), p.var());
    let xmap = p.arr(SCREENWIDTH); // output column -> source column
    let yoff = p.arr(SCREENHEIGHT); // output pixel row -> source row offset
    let playpal = cstr(p, "PLAYPAL");
    let cache = s.func("W_CacheLumpName");
    let setpal = s.func("I_SetPalette");
    p.def(s.func("I_InitGraphics"), &[], |p| {
        p.puts("\x1b[?25l\x1b[2J\x1b[999;999H\x1b[6n");
        p.set(t0, clock);
        p.while_(got.eq(0) & (clock - t0).lt(DSR_WAIT), |p| p.call(pump, []));
        p.if_(got.ne(0) & rows.eq(0) & cols.eq(0), |p| {
            p.set(raw, 1);
            p.set(shift, 1); // Shift is a real key there: no always-run
            p.set(gfx, 1);
            p.call(setpal, [cache.call([playpal.into(), PU_CACHE.into()])]);
            p.ret(0);
        });
        p.if_(got.eq(0) | rows.lt(1) | cols.lt(1), |p| {
            p.set(rows, 24);
            p.set(cols, 80);
        });
        // half-block cells are square pixels; show 320x200 at DOOM's 4:3
        p.set(h, min(rows * 2, SCREENHEIGHT));
        p.set(w, h * 4 / 3);
        p.if_(w.gt(cols), |p| {
            p.set(w, cols);
            p.set(h, max(w * 3 / 8 * 2, 2));
        });
        p.for_(gi, 0, w, |p| {
            p.set(xmap.at(gi), (2 * gi + 1) * SCREENWIDTH / (w * 2))
        });
        p.for_(gi, 0, h, |p| {
            p.set(
                yoff.at(gi),
                (2 * gi + 1) * SCREENHEIGHT / (h * 2) * SCREENWIDTH,
            )
        });
        p.set(gfx, 1);
    });

    let pals: Vec<i64> = wad.palettes.iter().flatten().copied().collect();
    let pal_base = p.arr_init(&pals).0;
    let pal = p.var_init(pal_base);
    let (pp, pn) = (p.var(), p.var());
    let (gamma, usegamma) = (s.global("gammatable"), s.global("usegamma"));
    p.def(setpal, &[pp], |p| {
        p.if_(raw.ne(0), |p| {
            // the RGB values i_video.c uploads: through the gamma table
            p.puts("\x1bP");
            p.for_(gi, 0, 768, |p| {
                p.putc(mem(gamma + mem(usegamma) * 256 + mem(pp + gi)))
            });
            p.ret(0);
        });
        p.set(
            pn,
            (pp - cache.call([playpal.into(), PU_CACHE.into()])) / 768,
        );
        p.if_(pn.lt(0) | pn.ge(wad.palettes.len() as i64), |p| {
            p.set(pn, 0)
        });
        p.set(pal, pal_base + pn * 256);
    });

    let screens = s.global("screens");
    let (scr, top, bot, r, x, fg, bg, ct, cb) = (
        p.var(),
        p.var(),
        p.var(),
        p.var(),
        p.var(),
        p.var(),
        p.var(),
        p.var(),
        p.var(),
    );
    p.def(s.func("I_FinishUpdate"), &[], |p| {
        p.set(scr, mem(screens));
        p.if_(raw.ne(0), |p| {
            // one character per pixel: its palette index
            p.puts("\x1bF");
            p.for_(r, 0, SCREENWIDTH * SCREENHEIGHT, |p| p.putc(mem(scr + r)));
            p.ret(0);
        });
        p.puts("\x1b[H");
        p.for_(r, 0, h / 2, |p| {
            p.set(top, scr + yoff.at(2 * r));
            p.set(bot, scr + yoff.at(2 * r + 1));
            p.set(fg, -1);
            p.set(bg, -1);
            p.for_(x, 0, w, |p| {
                p.set(ct, mem(pal + mem(top + xmap.at(x))));
                p.set(cb, mem(pal + mem(bot + xmap.at(x))));
                p.if_(ct.ne(fg) | cb.ne(bg), |p| {
                    p.puts("\x1b[");
                    p.if_(ct.ne(fg), |p| {
                        p.puts("38;5;");
                        p.putn(ct);
                        p.if_(cb.ne(bg), |p| p.putc(';'));
                    });
                    p.if_(cb.ne(bg), |p| {
                        p.puts("48;5;");
                        p.putn(cb);
                    });
                    p.putc('m');
                    p.set(fg, ct);
                    p.set(bg, cb);
                });
                p.putc('▀');
            });
            p.puts("\x1b[0m");
            p.if_(r.lt(h / 2 - 1), |p| p.puts("\r\n"));
        });
    });

    let (dst, src, ri) = (p.var(), p.var(), p.var());
    p.def(s.func("I_ReadScreen"), &[dst], |p| {
        p.set(src, mem(screens));
        p.for_(ri, 0, SCREENWIDTH * SCREENHEIGHT, |p| {
            p.set(mem(dst + ri), mem(src + ri))
        });
    });

    // ---- network: linuxdoom's single-player path (no -net)
    let dc = p.alloc(s.size("doomcom_t"));
    let (doomcom, netgame) = (s.global("doomcom"), s.global("netgame"));
    p.def(s.func("I_InitNetwork"), &[], |p| {
        for (f, v) in [
            ("id", DOOMCOM_ID),
            ("ticdup", 1),
            ("extratics", 0),
            ("numplayers", 1),
            ("numnodes", 1),
            ("deathmatch", 0),
            ("consoleplayer", 0),
        ] {
            p.set(mem(dc + s.field("doomcom_t", f)), v);
        }
        p.set(mem(doomcom), dc);
        p.set(mem(netgame), 0);
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use wsdoom::dsl::A;
    use wsdoom::vm;

    const LUMP_ADDR: i64 = 7000;

    struct T {
        p: Prog,
        s: Symbols,
        screen: i64,
    }

    /// Program with hand-built symbols, fake C functions and the platform layer.
    /// D_PostEvent prints "D<key> " / "U<key> ".
    fn setup() -> T {
        let mut p = Prog::new();
        let mut s = Symbols::default();
        for bi in BUILTINS {
            s.funcs.insert(bi.name.into(), p.func(bi.arity));
        }
        for (name, arity) in [
            ("malloc", 1),
            ("sprintf", 2),
            ("D_PostEvent", 1),
            ("W_CacheLumpName", 2),
        ] {
            s.funcs.insert(name.into(), p.func(arity));
        }
        for g in ["doomcom", "netgame", "usegamma"] {
            s.globals.insert(g.into(), p.alloc(1));
        }
        // gamma level g maps c to (c + g) % 256
        let gamma: Vec<i64> = (0..5 * 256).map(|i| (i % 256 + i / 256) % 256).collect();
        s.globals.insert("gammatable".into(), p.arr_init(&gamma).0);
        let screens = p.alloc(5);
        s.globals.insert("screens".into(), screens);
        let screen = p.alloc(64000);
        p.set_init(screens, &[screen]);
        let mut layout = |ty: &str, fields: &[&str], size: i64| {
            for (i, f) in fields.iter().enumerate() {
                s.fields.insert((ty.into(), f.to_string()), i as i64);
            }
            s.sizes.insert(ty.into(), size);
        };
        layout("event_t", &["type", "data1", "data2", "data3"], 4);
        layout("ticcmd_t", &[], 6);
        layout("sfxinfo_t", &["data"], 1);
        let dc = [
            "id",
            "intnum",
            "command",
            "remotenode",
            "datalength",
            "numnodes",
            "ticdup",
            "extratics",
            "deathmatch",
            "savegame",
            "episode",
            "map",
            "skill",
            "consoleplayer",
            "numplayers",
            "angleoffset",
            "drone",
        ];
        layout("doomcom_t", &dc, dc.len() as i64 + 76);

        let e = p.var();
        p.def(s.func("D_PostEvent"), &[e], |p| {
            p.if_else(mem(e).eq(0), |p| p.putc('D'), |p| p.putc('U'));
            p.putn(mem(e + 1));
            p.putc(' ');
        });
        let (heap, n) = (p.alloc(100_000), p.var());
        let brk = p.var_init(heap);
        p.def(s.func("malloc"), &[n], |p| {
            p.inc(brk, n);
            p.ret(brk - n);
        });
        // fake sprintf: the format, then every argument as a character
        let (buf, fmt, args, cnt, i, j) = (p.var(), p.var(), p.arr(16), p.var(), p.var(), p.var());
        p.def_variadic(s.func("sprintf"), &[buf, fmt], args, cnt, |p| {
            p.set(j, 0);
            p.while_(mem(fmt + j).ne(0), |p| {
                p.set(mem(buf + j), mem(fmt + j));
                p.inc(j, 1);
            });
            p.for_(i, 0, cnt, |p| {
                p.set(mem(buf + j), args.at(i));
                p.inc(j, 1);
            });
            p.set(mem(buf + j), 0);
        });
        let (name, tag) = (p.var(), p.var());
        p.def(s.func("W_CacheLumpName"), &[name, tag], |p| {
            p.ret(LUMP_ADDR)
        });

        // palette n maps color i to 16 + 10n + i
        let palettes = (0..14)
            .map(|n| std::array::from_fn(|i| 16 + 10 * n + i as i64))
            .collect();
        define(
            &mut p,
            &s,
            &wadbake::Baked {
                lumps: vec![],
                palettes,
            },
        );
        T { p, s, screen }
    }

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

    /// `loops` times, as D_DoomLoop does: run I_StartFrame, print I_GetTime,
    /// ':', run I_StartTic, '|'.
    fn ticks(input: &[u8], loops: usize) -> String {
        let T { mut p, s, .. } = setup();
        for _ in 0..loops {
            p.call(s.func("I_StartFrame"), []);
            p.putn(s.func("I_GetTime").call([]));
            p.putc(':');
            p.call(s.func("I_StartTic"), []);
            p.putc('|');
        }
        run(p, input)
    }

    #[test]
    fn key_decoding() {
        let input: Vec<u8> = [
            &[0x81][..],
            b"a\x81",
            b"\x1b[A\x81",
            b"\x1b\x81", // lone ESC
            b"\rQ\x81",
            b"\x1bOP\x1b[15~\x1b[24~\x1b[[B\x81", // F1 F5 F12 F2
            b"z\x1b[1;5C\x1bOB\t\x7f\x84",
        ]
        .concat();
        assert_eq!(
            ticks(&input, 9),
            "0:D182 |1:|2:D97 |3:D173 |4:D27 |5:D13 D113 |6:D187 D191 D216 D188 |\
             10:D157 D174 D175 D9 D127 |11:|"
        );
    }

    #[test]
    fn keyup_synthesis() {
        let input = b"\x80a\x81\x85a\x84\x83\x83b\x81\x8c"; // 0x80: time protocol, 0 tics
                                                            // a: down at 1, repeated at 10 -> up at max(1+12, 10+6) = 16; b: down 17, up 29
        assert_eq!(
            ticks(input, 8),
            "0:D182 |1:D97 |6:|10:D97 |13:|16:U97 |17:D98 |29:U98 |"
        );
    }

    #[test]
    fn turn_based_fallback() {
        // no time bytes: each key is 4 tics, bytes >= 0x80 ignored, end of input is 1 tic
        assert_eq!(
            ticks(b"a\x85b\x1bx", 9),
            "0:D182 |4:D97 |8:D98 |12:D27 |16:D120 U97 |17:|18:|19:|20:U98 |"
        );
    }

    #[test]
    fn get_time_reads_only_when_spinning() {
        let T { mut p, s, .. } = setup();
        let gt = s.func("I_GetTime");
        for _ in 0..=SPIN_LIMIT {
            p.putn(gt.call([]));
            p.putc(' ');
        }
        p.call(s.func("I_StartFrame"), []);
        p.putn(gt.call([]));
        let zeros = "0 ".repeat(SPIN_LIMIT as usize);
        assert_eq!(run(p, b"\x82\x83"), format!("{zeros}2 5"));
    }

    #[test]
    fn window_host_gets_raw_frames_and_sends_releases() {
        let T { mut p, s, screen } = setup();
        let px: Vec<i64> = (0..64000).map(|i| i % 251).collect();
        p.set_init(screen, &px);
        let pal: Vec<i64> = (0..768).map(|i| i % 256).collect();
        let pal = p.arr_init(&pal).0;
        p.call(s.func("I_InitGraphics"), []);
        p.set(mem(s.global("usegamma")), 2);
        p.call(s.func("I_SetPalette"), [pal.into()]);
        p.call(s.func("I_FinishUpdate"), []);
        p.eval(s.func("I_GetTime").call([]));
        p.call(s.func("I_StartFrame"), []); // reads the key events up to a tic
        p.call(s.func("I_StartTic"), []); // no always-run Shift here
        let code = vm::parse(&vm::encode(&p.finish())).unwrap();
        let input: Vec<i64> = [
            0x81, 27, '[' as i64, '0' as i64, ';' as i64, '0' as i64, 'R' as i64,
        ]
        .into_iter()
        .chain([256 + KEY_UPARROW, 512 + KEY_UPARROW, 0x81])
        .collect();
        let mut it = input.into_iter();
        let mut out = Vec::new();
        vm::run(
            &code,
            &mut |_: &mut Vec<u8>| it.next().unwrap_or(-1),
            &mut out,
        )
        .unwrap();
        let out: Vec<char> = String::from_utf8(out).unwrap().chars().collect();
        let at = |pat: &str| {
            out.windows(2)
                .rposition(|w| w.iter().copied().eq(pat.chars()))
                .unwrap()
                + 2
        };
        let pal_at = at("\x1bP");
        let want_pal: Vec<char> = (0..768u32)
            .map(|i| char::from_u32((i % 256 + 2) % 256).unwrap())
            .collect();
        assert_eq!(out[pal_at..pal_at + 768], want_pal[..]);
        let frame_at = at("\x1bF");
        assert!(out[frame_at..frame_at + 64000]
            .iter()
            .zip(&px)
            .all(|(&c, &v)| c as i64 == v));
        let rest: String = out[frame_at + 64000..].iter().collect();
        assert_eq!(rest, "D173 U173 ");
    }

    #[test]
    fn ctrl_c_quits() {
        let T { mut p, s, .. } = setup();
        p.eval(s.func("I_GetTime").call([]));
        p.call(s.func("I_StartFrame"), []); // waits for a tic, reads Ctrl-C
        p.putc('!');
        assert_eq!(run(p, b"\x03"), "\x1b[0m\x1b[?25h");
    }

    #[test]
    fn error_formats_and_ends() {
        let T { mut p, s, .. } = setup();
        let fmt = cstr(&mut p, "x=");
        p.eval(
            s.func("I_Error")
                .call_vec(vec![fmt.into(), 'a'.into(), 'b'.into(), 2.into()]),
        );
        p.putc('!');
        assert_eq!(run(p, b""), "\x1b[0m\x1b[?25hError: x=ab\n");
    }

    /// Run I_InitGraphics on `input`, then `after`; return (output, rows of cells).
    fn graphics(
        input: &[u8],
        screen: impl Fn(i64, i64) -> i64,
        after: impl FnOnce(&mut Prog, &Symbols),
    ) -> String {
        let T {
            mut p,
            s,
            screen: addr,
        } = setup();
        let px: Vec<i64> = (0..64000).map(|i| screen(i % 320, i / 320)).collect();
        p.set_init(addr, &px);
        p.call(s.func("I_InitGraphics"), []);
        after(&mut p, &s);
        run(p, input)
    }

    /// Cells per output line of the last frame.
    fn frame(out: &str) -> Vec<&str> {
        out.rsplit("\x1b[H").next().unwrap().split("\r\n").collect()
    }

    fn assert_size(out: &str, w: usize, h: usize) {
        let lines = frame(out);
        assert_eq!(lines.len(), h / 2);
        for l in lines {
            assert_eq!(l.matches('▀').count(), w);
            assert!(l.ends_with("\x1b[0m"));
        }
    }

    fn finish(p: &mut Prog, s: &Symbols) {
        p.call(s.func("I_FinishUpdate"), []);
    }

    #[test]
    fn dsr_size_parsing() {
        let reply = b"\x81x\x1b[30;200R";
        let out = graphics(
            reply,
            |_, _| 0,
            |p, s| {
                finish(p, s);
                p.putc('#');
                p.call(s.func("I_StartTic"), []);
            },
        );
        assert!(out.starts_with("\x1b[?25l\x1b[2J\x1b[999;999H\x1b[6n\x1b[H"));
        let (frame_out, events) = out.split_once('#').unwrap();
        assert_size(frame_out, 80, 60); // 2*30 rows, 4:3
        assert_eq!(events, "D182 D120 "); // key typed before the reply is kept
        assert_size(&graphics(b"\x1b[50;100R", |_, _| 0, finish), 100, 74); // limited by columns
        assert_size(&graphics(b"\x1b[100;400R", |_, _| 0, finish), 266, 200); // capped at 200 rows
    }

    #[test]
    fn dsr_timeout_defaults_to_80x24() {
        let out = graphics(
            &[0x81; 40],
            |_, _| 0,
            |p, s| {
                finish(p, s);
                p.putc('#');
                p.putn(s.func("I_GetTime").call([]));
            },
        );
        let (frame_out, time) = out.split_once('#').unwrap();
        assert_size(frame_out, 64, 48);
        assert_eq!(time, "35");
    }

    #[test]
    fn frame_colors_and_palette() {
        // even rows color 1, odd rows color 2: top half-pixel 1, bottom 2
        let px = |_, y: i64| 1 + y % 2;
        let out = graphics(b"\x1b[100;400R", px, |p, s| {
            finish(p, s);
            p.call(s.func("I_SetPalette"), [(LUMP_ADDR + 768).into()]);
            finish(p, s);
            p.putc('#');
            p.call(s.func("I_SetPalette"), [LUMP_ADDR.into()]);
            finish(p, s);
        });
        let parts: Vec<&str> = out.split("\x1b[H").collect();
        let line = |pal: i64| {
            format!(
                "\x1b[38;5;{};48;5;{}m{}\x1b[0m",
                17 + 10 * pal,
                18 + 10 * pal,
                "▀".repeat(266)
            )
        };
        assert_eq!(frame(parts[1])[0], line(0));
        assert_eq!(frame(parts[2].split('#').next().unwrap())[0], line(1));
        assert_eq!(frame(parts[3])[99], line(0));
    }

    #[test]
    fn memory_screen_and_network() {
        let T { mut p, s, screen } = setup();
        p.set_init(screen + 63999, &[42]);
        let size = p.var();
        let zone = p.var();
        p.set(zone, s.func("I_ZoneBase").call([size.0.into()]));
        p.putn(size);
        p.putc(' ');
        let low = p.var();
        p.set(low, s.func("I_AllocLow").call([64000.into()]));
        p.call(s.func("I_ReadScreen"), [low.into()]);
        p.putn(A(0).at(low + 63999));
        p.putc(' ');
        p.putn(mem(s.func("I_BaseTiccmd").call([])));
        p.call(s.func("I_InitNetwork"), []);
        let dc = mem(s.global("doomcom"));
        for f in [
            "id",
            "numnodes",
            "numplayers",
            "ticdup",
            "extratics",
            "consoleplayer",
        ] {
            p.putc(' ');
            p.putn(mem(dc.clone() + s.field("doomcom_t", f)));
        }
        p.putc(' ');
        p.putn(s.func("I_RegisterSong").call([0.into()]));
        p.putn(s.func("I_SoundIsPlaying").call([0.into()]));
        assert_eq!(
            run(p, b""),
            format!("6000000 42 0 {DOOMCOM_ID} 1 1 1 0 0 10")
        );
    }
}
