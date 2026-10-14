//! DOOM in Whitespace as one executable, with doom.ws built in. `doom`
//! serves the game's web page (the same Whitespace program, run by the
//! interpreter compiled to WebAssembly, see crates/web) on localhost and
//! opens it in a browser window; `doom --terminal` plays it in the terminal;
//! `doom [--clock] program.ws` runs any other Whitespace program.
//!
//! On a terminal, keys reach the program immediately and unechoed; Ctrl-C
//! quits. With the clock (always on for the built-in game), a read that
//! finds no key pending reports elapsed time instead: one byte 0x80+N for N
//! tics of 1/35 s (N = 1..127), waiting at most one tic. The first read
//! always reports time, which tells the program the clock is there.

use std::collections::VecDeque;
use std::io::{IsTerminal, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::process::{exit, Command, Stdio};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::time::{Duration, Instant};

fn stty(args: &[&str]) -> Option<String> {
    let out = Command::new("stty")
        .args(args)
        .stdin(Stdio::inherit())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn restore(saved: &Option<String>) {
    if let Some(s) = saved {
        print!("\x1b[0m\x1b[?25h");
        let _ = std::io::stdout().flush();
        stty(&[s]);
    }
}

/// The game itself, compiled into the binary, and its web page.
const DOOM: &str = include_str!("../../doom.ws");
const PAGE: &str = include_str!("../../index.html");
const PAGE_JS: &str = include_str!("../../pkg/doom_web.js");
const PAGE_WASM: &[u8] = include_bytes!("../../pkg/doom_web_bg.wasm");
/// One DOOM tic.
const TIC: Duration = Duration::from_nanos(1_000_000_000 / 35);

fn usage() -> ! {
    eprintln!("usage: doom [--terminal | [--clock] program.ws]");
    exit(2)
}

/// Serve the web page and doom.ws on localhost until killed.
fn serve() -> ! {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap_or_else(|e| {
        eprintln!("doom: can't serve the game: {e}");
        exit(1)
    });
    let url = format!("http://{}/", listener.local_addr().expect("bound address"));
    println!("DOOM is playing at {url} (Ctrl-C here stops it)");
    open_window(&url);
    for stream in listener.incoming().flatten() {
        std::thread::spawn(move || respond(stream));
    }
    exit(1)
}

fn respond(mut stream: TcpStream) {
    let mut buf = [0u8; 4096];
    let n = stream.read(&mut buf).unwrap_or(0);
    let request = String::from_utf8_lossy(&buf[..n]);
    let (kind, body): (&str, &[u8]) = match request.split_whitespace().nth(1).unwrap_or("") {
        "/" | "/index.html" => ("text/html; charset=utf-8", PAGE.as_bytes()),
        "/pkg/doom_web.js" => ("text/javascript", PAGE_JS.as_bytes()),
        "/pkg/doom_web_bg.wasm" => ("application/wasm", PAGE_WASM),
        "/doom.ws" => ("text/plain", DOOM.as_bytes()),
        _ => ("", b""),
    };
    let status = if kind.is_empty() {
        "404 Not Found"
    } else {
        "200 OK"
    };
    let head = format!("HTTP/1.1 {status}\r\nContent-Type: {kind}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len());
    let _ = stream
        .write_all(head.as_bytes())
        .and_then(|_| stream.write_all(body));
}

/// A 4:3 app window of a Chromium-based browser if there is one, otherwise
/// the default browser.
fn open_window(url: &str) {
    let app = format!("--app={url}");
    let size = "--window-size=960,750";
    let ok = |c: &mut Command| c.stderr(Stdio::null()).status().is_ok_and(|s| s.success());
    let opened = if cfg!(target_os = "macos") {
        [
            "Google Chrome",
            "Microsoft Edge",
            "Brave Browser",
            "Chromium",
        ]
        .iter()
        .any(|b| ok(Command::new("open").args(["-na", b, "--args", &app, size])))
            || ok(Command::new("open").arg(url))
    } else if cfg!(windows) {
        ok(Command::new("cmd").args(["/C", "start", "", url]))
    } else {
        [
            "google-chrome",
            "chromium",
            "chromium-browser",
            "microsoft-edge",
        ]
        .iter()
        .any(|b| {
            Command::new(b)
                .args([&app, size])
                .stderr(Stdio::null())
                .spawn()
                .is_ok()
        }) || ok(Command::new("xdg-open").arg(url))
    };
    if !opened {
        println!("Open that address in a web browser.");
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (clock, path) = match args.as_slice() {
        [] => serve(),
        [t] if t == "--terminal" => (true, None),
        [c, path] if c == "--clock" => (true, Some(path)),
        [path] if !path.starts_with('-') => (false, Some(path)),
        _ => usage(),
    };
    let (name, src) = match path {
        None => ("doom.ws", DOOM.to_string()),
        Some(p) => (
            p.as_str(),
            std::fs::read_to_string(p).unwrap_or_else(|e| {
                eprintln!("{p}: {e}");
                exit(2)
            }),
        ),
    };
    let code = wsdoom::vm::parse(&src).unwrap_or_else(|e| {
        eprintln!("{name}: {e}");
        exit(1)
    });

    let saved = if std::io::stdin().is_terminal() {
        stty(&["-g"])
    } else {
        None
    };
    if saved.is_some() {
        stty(&["-icanon", "-echo", "-isig", "min", "1"]);
    }

    // a reader thread lets the main thread wait for keys with a deadline;
    // each message is one read() of the terminal, i.e. usually one key press
    let (tx, rx) = mpsc::channel::<Vec<u8>>();
    std::thread::spawn(move || {
        let mut stdin = std::io::stdin().lock();
        let mut buf = [0u8; 64];
        loop {
            let n = stdin.read(&mut buf).unwrap_or(0);
            if tx.send(buf[..n].to_vec()).is_err() || n == 0 {
                break;
            }
        }
    });

    let mut stdout = std::io::stdout().lock();
    let mut out = Vec::new();
    let mut queue: VecDeque<u8> = VecDeque::new(); // bytes the program has not read yet
    let mut eof = false;
    let start = Instant::now();
    let mut reported: u64 = 0;
    let result = wsdoom::vm::run(
        &code,
        &mut |pending: &mut Vec<u8>| {
            let _ = stdout.write_all(pending);
            let _ = stdout.flush();
            pending.clear();
            if let Some(b) = queue.pop_front() {
                return b as i64;
            }
            if eof {
                return -1;
            }
            let chunk = if clock {
                // no key pending: report elapsed tics (the first read always does)
                loop {
                    let now = (start.elapsed().as_nanos() * 35 / 1_000_000_000) as u64;
                    if reported == 0 || now > reported {
                        let n = now.saturating_sub(reported).clamp(1, 127);
                        reported += n;
                        return 0x80 + n as i64;
                    }
                    let next = start + TIC * (reported + 1) as u32;
                    match rx.recv_timeout(next.saturating_duration_since(Instant::now())) {
                        Ok(c) => break c,
                        Err(RecvTimeoutError::Timeout) => {}
                        Err(RecvTimeoutError::Disconnected) => break vec![],
                    }
                }
            } else {
                rx.recv().unwrap_or_default()
            };
            if chunk.is_empty() {
                eof = true;
                return -1;
            }
            if saved.is_some() && chunk.contains(&3) {
                // reset colours and show the cursor, which the game hides
                let _ = stdout.write_all(b"\x1b[0m\x1b[?25h\r\n");
                let _ = stdout.flush();
                restore(&saved);
                exit(130);
            }
            queue.extend(&chunk);
            queue.pop_front().map_or(-1, |b| b as i64)
        },
        &mut out,
    );
    let _ = stdout.write_all(&out);
    let _ = stdout.flush();
    restore(&saved);
    if let Err(e) = result {
        eprintln!("{name}: {e}");
        exit(1);
    }
}
