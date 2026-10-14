//! `doomgen`             write doom.ws at the repository root
//! `doomgen run SCRIPT [OUT]`  build in memory and run headless (see
//!                       `doomgen::script`); after the script the clock keeps
//!                       running until a step budget ends. Errors map to C.

use wsdoom::vm;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let t = std::time::Instant::now();
    let b = doomgen::build().unwrap_or_else(|e| {
        eprintln!("doomgen: {e}");
        std::process::exit(1)
    });
    eprintln!("built {} instructions in {:?}", b.code.len(), t.elapsed());
    match args.first().map(String::as_str) {
        None => {
            let ws = vm::encode(&b.code);
            std::fs::write(doomgen::root().join("doom.ws"), &ws).expect("writing doom.ws");
            println!("doom.ws: {} bytes", ws.len());
        }
        Some("run") => {
            let mut it = doomgen::script(args.get(1).map_or("", String::as_str)).into_iter();
            let parsed = b.parsed();
            let (mut m, mut out) = (vm::Machine::default(), Vec::new());
            let t = std::time::Instant::now();
            let r = m.run(
                &parsed,
                &mut |_: &mut Vec<u8>| it.next().unwrap_or(0x81),
                &mut out,
                20_000_000_000,
            );
            eprintln!("ran in {:?}, {} output bytes", t.elapsed(), out.len());
            if let Some(path) = args.get(2) {
                std::fs::write(path, &out).expect("writing output");
            }
            match r {
                Ok(done) => eprintln!(
                    "{}",
                    if done {
                        "program ended"
                    } else {
                        "step budget used up"
                    }
                ),
                Err(e) => {
                    eprintln!("error: {e}\n  at {}", b.where_is(m.pc - 1));
                    for &ret in m.calls.iter().rev() {
                        eprintln!("  called from {}", b.where_is(ret - 1));
                    }
                    std::process::exit(1);
                }
            }
        }
        Some(other) => {
            eprintln!("doomgen: unknown command `{other}`");
            std::process::exit(2);
        }
    }
}
