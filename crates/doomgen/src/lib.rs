//! The build of doom.ws: the original DOOM source (vendor/linuxdoom-1.10)
//! compiled to Whitespace, with the runtime and platform layers and the
//! shareware WAD (wad/doom1.wad) baked in, plus helpers to run it headless.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use wsdoom::cc::symbols::{Builtin, Symbols};
use wsdoom::dsl::Prog;
use wsdoom::vm::{self, Ins};

pub struct Build {
    pub code: Vec<Ins>,
    pub notes: Vec<String>,
    pub syms: Symbols,
}

pub fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

pub fn build() -> Result<Build, String> {
    let src = root().join("vendor/linuxdoom-1.10");
    const PLATFORM: [&str; 5] = [
        "i_main.c",
        "i_system.c",
        "i_video.c",
        "i_sound.c",
        "i_net.c",
    ];
    let mut files: Vec<PathBuf> = std::fs::read_dir(&src)
        .map_err(|e| format!("{}: {e}", src.display()))?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| {
            p.extension().is_some_and(|x| x == "c")
                && !PLATFORM.contains(&p.file_name().unwrap().to_str().unwrap())
        })
        .collect();
    files.sort();
    let defines = [("NORMALUNIX", "1"), ("LINUX", "1")];
    let units = files
        .iter()
        .map(|f| ccfront::parse_file(f, std::slice::from_ref(&src), &defines))
        .collect::<Result<Vec<_>, _>>()?;
    let wad = wadbake::bake(
        &std::fs::read(root().join("wad/doom1.wad")).map_err(|e| format!("wad/doom1.wad: {e}"))?,
    )?;
    let mut p = Prog::new();
    let builtins: Vec<Builtin> = runtime::BUILTINS
        .iter()
        .chain(platform::BUILTINS)
        .copied()
        .collect();
    let (syms, debug) = ccback::compile(&mut p, &units, &builtins)?;
    runtime::define(&mut p, &syms, &wad);
    platform::define(&mut p, &syms, &wad);
    runtime::entry(&mut p, &syms);
    Ok(Build {
        code: p.finish(),
        notes: debug.notes,
        syms,
    })
}

/// Input bytes from a script: `.` one tic of time, `~N` N tics, `\e` ESC,
/// `\n` Enter, anything else itself.
pub fn script(s: &str) -> Vec<i64> {
    let mut out = vec![];
    let mut cs = s.chars().peekable();
    while let Some(c) = cs.next() {
        match c {
            '.' => out.push(0x81),
            '~' => {
                let mut n = String::new();
                while let Some(d) = cs.peek().filter(|d| d.is_ascii_digit()) {
                    n.push(*d);
                    cs.next();
                }
                let mut n: i64 = n.parse().unwrap_or(1);
                while n > 0 {
                    out.push(0x80 + n.min(127));
                    n -= n.min(127);
                }
            }
            '\\' => match cs.next() {
                Some('e') => out.push(27),
                Some('n') => out.push(13),
                Some(c) => out.push(c as i64),
                None => {}
            },
            c => out.push(c as i64),
        }
    }
    out
}

impl Build {
    /// Source position of instruction `pc` of the parsed program: the
    /// enclosing function and, inside C code, the last statement's line.
    pub fn where_is(&self, pc: usize) -> String {
        let names: HashMap<i64, &str> = self
            .syms
            .funcs
            .iter()
            .map(|(n, f)| (f.label(), n.as_str()))
            .collect();
        let (mut func, mut note, mut k) = ("?", -1, 0);
        for &ins in &self.code {
            match ins {
                Ins::Label(l) => {
                    if let Some(n) = names.get(&l) {
                        func = n;
                        note = -1;
                    }
                }
                Ins::Note(n) => note = n,
                _ => {
                    if k == pc {
                        break;
                    }
                    k += 1;
                }
            }
        }
        match self.notes.get(note as usize) {
            Some(n) if note >= 0 => n.clone(),
            _ => format!("in builtin {func}"),
        }
    }

    /// The parsed program, as the VM runs it.
    pub fn parsed(&self) -> Vec<Ins> {
        vm::parse(&vm::encode(&self.code)).expect("re-parse")
    }
}
