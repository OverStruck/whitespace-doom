//! DOOM in a web page: doom.ws, the Whitespace program, runs in the wsdoom
//! VM compiled to WebAssembly. The page answers DOOM's terminal-size query
//! as a window host (0;0), so the platform layer sends raw 320x200 frames
//! and palettes, shown on a canvas, and takes real key presses and releases.
//!
//! Build (needs the wasm32-unknown-unknown target and wasm-bindgen-cli
//! 0.2.129):
//!   cargo build --release -p doom-web --target wasm32-unknown-unknown
//!   wasm-bindgen --target web --no-typescript --out-dir pkg \
//!     target/wasm32-unknown-unknown/release/doom_web.wasm
//! index.html (at the repository root, which is also the GitHub Pages
//! site) loads it and fetches doom.ws from next to itself; the `doom`
//! binary serves the same files.

use std::cell::RefCell;
use std::collections::{HashSet, VecDeque};
use std::rc::Rc;
use wasm_bindgen::prelude::*;
use wasm_bindgen::{Clamped, JsCast};
use wasm_bindgen_futures::JsFuture;
use web_sys::{
    CanvasRenderingContext2d, Element, HtmlCanvasElement, ImageData, KeyboardEvent, Response,
    Window,
};
use wsdoom::vm::{self, Ins, Machine};

pub const WIDTH: usize = 320;
pub const HEIGHT: usize = 200;

/// The program's output, one value per character: text, the terminal-size
/// query, and the platform layer's raw palettes ("\x1bP" + 768 RGB values)
/// and frames ("\x1bF" + a palette index per pixel).
pub struct Screen {
    state: State,
    pub palette: [u8; 768],
    pub pixels: Vec<u8>,
    /// a whole frame arrived since this was last cleared
    pub fresh: bool,
    /// what the program printed as text (start-up messages, errors)
    pub text: String,
    /// input to send back to the program
    pub replies: VecDeque<i64>,
}

enum State {
    Text,
    Esc,
    Csi(String),
    Palette(usize),
    Frame(usize),
}

impl Default for Screen {
    fn default() -> Screen {
        Screen {
            state: State::Text,
            palette: [0; 768],
            pixels: vec![0; WIDTH * HEIGHT],
            fresh: false,
            text: String::new(),
            replies: VecDeque::new(),
        }
    }
}

impl Screen {
    pub fn feed(&mut self, bytes: &[u8]) {
        for ch in String::from_utf8_lossy(bytes).chars() {
            let c = ch as u32;
            self.state = match std::mem::replace(&mut self.state, State::Text) {
                State::Text if ch == '\x1b' => State::Esc,
                State::Text => {
                    self.text.push(ch);
                    State::Text
                }
                State::Esc => match ch {
                    '[' => State::Csi(String::new()),
                    'P' => State::Palette(0),
                    'F' => State::Frame(0),
                    _ => State::Text,
                },
                State::Csi(mut s) if !(0x40..0x7f).contains(&c) => {
                    s.push(ch);
                    State::Csi(s)
                }
                State::Csi(s) => {
                    if s == "6" && ch == 'n' {
                        self.replies.extend(b"\x1b[0;0R".map(i64::from));
                    }
                    State::Text
                }
                State::Palette(k) => {
                    self.palette[k] = c as u8;
                    if k + 1 < self.palette.len() {
                        State::Palette(k + 1)
                    } else {
                        State::Text
                    }
                }
                State::Frame(k) => {
                    self.pixels[k] = c as u8;
                    if k + 1 < self.pixels.len() {
                        State::Frame(k + 1)
                    } else {
                        self.fresh = true;
                        State::Text
                    }
                }
            };
        }
    }

    /// The current frame through the current palette, as RGBA.
    pub fn rgba(&self, out: &mut [u8]) {
        for (px, &i) in out.as_chunks_mut::<4>().0.iter_mut().zip(&self.pixels) {
            let i = i as usize * 3;
            *px = [
                self.palette[i],
                self.palette[i + 1],
                self.palette[i + 2],
                255,
            ];
        }
    }
}

/// DOOM's code (doomdef.h) for a key, by `KeyboardEvent.code`.
pub fn doom_key(code: &str) -> Option<i64> {
    let ascii = |c: u8| Some(c as i64);
    match code {
        "ArrowLeft" => Some(0xac),
        "ArrowUp" => Some(0xad),
        "ArrowRight" => Some(0xae),
        "ArrowDown" => Some(0xaf),
        "Escape" => Some(27),
        "Enter" | "NumpadEnter" => Some(13),
        "Tab" => Some(9),
        "Backspace" => Some(127),
        "Pause" => Some(0xff),
        "ShiftLeft" | "ShiftRight" => Some(0x80 + 0x36),
        "ControlLeft" | "ControlRight" => Some(0x80 + 0x1d),
        "AltLeft" | "AltRight" => Some(0x80 + 0x38),
        "F11" => Some(0x80 + 0x57),
        "F12" => Some(0x80 + 0x58),
        "Space" => ascii(b' '),
        "Minus" | "NumpadSubtract" => ascii(b'-'),
        "Equal" | "NumpadAdd" => ascii(b'='),
        "Comma" => ascii(b','),
        "Period" => ascii(b'.'),
        "Slash" => ascii(b'/'),
        "Semicolon" => ascii(b';'),
        "Quote" => ascii(b'\''),
        "BracketLeft" => ascii(b'['),
        "BracketRight" => ascii(b']'),
        "Backslash" => ascii(b'\\'),
        "Backquote" => ascii(b'`'),
        _ => {
            let one = |prefix: &str| {
                code.strip_prefix(prefix)
                    .filter(|s| s.len() == 1)
                    .map(|s| s.as_bytes()[0])
            };
            if let Some(n) = code
                .strip_prefix('F')
                .and_then(|n| n.parse::<i64>().ok())
                .filter(|n| (1..=10).contains(n))
            {
                Some(0x80 + 0x3a + n)
            } else if let Some(l) = one("Key") {
                ascii(l.to_ascii_lowercase())
            } else {
                one("Digit").and_then(ascii)
            }
        }
    }
}

struct Game {
    code: Vec<Ins>,
    m: Machine,
    out: Vec<u8>,
    screen: Screen,
    keys: Rc<RefCell<VecDeque<i64>>>,
    start: f64,
    reported: i64,
}

impl Game {
    /// Run until the program waits for time or about 10 ms passed;
    /// Ok(false) once it ended.
    fn step(&mut self, now: &dyn Fn() -> f64) -> Result<bool, String> {
        let deadline = now() + 10.0;
        loop {
            let Game {
                code,
                m,
                out,
                screen,
                keys,
                start,
                reported,
            } = self;
            let mut waiting = false;
            let mut input = |pending: &mut Vec<u8>| {
                screen.feed(pending);
                pending.clear();
                if let Some(b) = screen
                    .replies
                    .pop_front()
                    .or_else(|| keys.borrow_mut().pop_front())
                {
                    return b;
                }
                // the runner's clock: 0x80 + tics of 1/35 s since the last
                // report, which the first read always gets
                let tics = ((now() - *start) * 0.035) as i64;
                if *reported == 0 || tics > *reported {
                    let n = (tics - *reported).clamp(1, 127);
                    *reported += n;
                    return 0x80 + n;
                }
                waiting = true;
                vm::WAIT
            };
            let r = m.run(code, &mut input, out, 1 << 20);
            screen.feed(out);
            out.clear();
            match r {
                Ok(true) => return Ok(false),
                Err(e) => return Err(e),
                Ok(false) if waiting || now() > deadline => return Ok(true),
                Ok(false) => {}
            }
        }
    }
}

fn listen(
    window: &Window,
    event: &str,
    f: impl FnMut(KeyboardEvent) + 'static,
) -> Result<(), JsValue> {
    let f = Closure::<dyn FnMut(KeyboardEvent)>::new(f);
    window.add_event_listener_with_callback(event, f.as_ref().unchecked_ref())?;
    f.forget();
    Ok(())
}

#[wasm_bindgen(start)]
pub async fn start() -> Result<(), JsValue> {
    let window = web_sys::window().ok_or("no window")?;
    let document = window.document().ok_or("no document")?;
    let status: Element = document.get_element_by_id("status").ok_or("no #status")?;
    let canvas: HtmlCanvasElement = document
        .get_element_by_id("screen")
        .ok_or("no #screen")?
        .dyn_into()?;
    let ctx: CanvasRenderingContext2d = canvas
        .get_context("2d")?
        .ok_or("no 2d canvas")?
        .dyn_into()?;

    let response: Response = JsFuture::from(window.fetch_with_str("doom.ws"))
        .await?
        .dyn_into()?;
    if !response.ok() {
        return Err(format!("doom.ws: HTTP {}", response.status()).into());
    }
    let bytes = js_sys::Uint8Array::new(&JsFuture::from(response.array_buffer()?).await?).to_vec();
    let code = vm::parse(&String::from_utf8_lossy(&bytes))?;
    drop(bytes);

    // keys as the platform layer takes them from a window host
    let keys = Rc::new(RefCell::new(VecDeque::new()));
    let held = Rc::new(RefCell::new(HashSet::new()));
    for (event, offset) in [("keydown", 256), ("keyup", 512)] {
        let (keys, held) = (keys.clone(), held.clone());
        listen(&window, event, move |e| {
            if let Some(k) = doom_key(&e.code()) {
                e.prevent_default();
                keys.borrow_mut().push_back(offset + k);
                if offset == 256 {
                    held.borrow_mut().insert(k);
                } else {
                    held.borrow_mut().remove(&k);
                }
            }
        })?;
    }
    // releases go missing while the page is in the background
    let (k, h) = (keys.clone(), held.clone());
    let blur = Closure::<dyn FnMut()>::new(move || {
        k.borrow_mut()
            .extend(h.borrow_mut().drain().map(|k| 512 + k))
    });
    window.add_event_listener_with_callback("blur", blur.as_ref().unchecked_ref())?;
    blur.forget();

    let perf = window.performance().ok_or("no performance clock")?;
    let now = move || perf.now();
    let mut game = Game {
        code,
        m: Machine::default(),
        out: Vec::new(),
        screen: Screen::default(),
        keys,
        start: now(),
        reported: 0,
    };
    let mut rgba = vec![0u8; WIDTH * HEIGHT * 4];
    status.set_text_content(None);

    // one slice of the program per animation frame
    let tick = Rc::new(RefCell::new(None::<Closure<dyn FnMut()>>));
    let first = tick.clone();
    let win = window.clone();
    *first.borrow_mut() = Some(Closure::new(move || {
        let running = game.step(&now);
        if game.screen.fresh {
            game.screen.fresh = false;
            game.screen.rgba(&mut rgba);
            if let Ok(img) = ImageData::new_with_u8_clamped_array_and_sh(
                Clamped(&rgba),
                WIDTH as u32,
                HEIGHT as u32,
            ) {
                let _ = ctx.put_image_data(&img, 0.0, 0.0);
            }
        }
        match running {
            Ok(true) => {
                let _ = win.request_animation_frame(
                    tick.borrow().as_ref().unwrap().as_ref().unchecked_ref(),
                );
            }
            Ok(false) => {
                let text = &game.screen.text;
                let msg = text
                    .find("Error:")
                    .map_or("DOOM has quit. Reload the page to play again.", |i| {
                        text[i..].trim()
                    });
                status.set_text_content(Some(msg));
            }
            Err(e) => status.set_text_content(Some(&format!("Whitespace VM error: {e}"))),
        }
    }));
    window.request_animation_frame(first.borrow().as_ref().unwrap().as_ref().unchecked_ref())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn screen_splits_text_queries_palettes_and_frames() {
        let mut s = Screen::default();
        s.feed(b"DOOM\n\x1b[?25l\x1b[2J\x1b[999;999H\x1b[6n");
        assert_eq!(s.text, "DOOM\n");
        assert_eq!(
            s.replies
                .iter()
                .map(|&b| b as u8 as char)
                .collect::<String>(),
            "\x1b[0;0R"
        );
        // values above 127 are two UTF-8 bytes; the VM writes whole characters,
        // but a frame may span two feeds
        let packet = |kind: &str, n: u32, f: fn(u32) -> u32| -> String {
            std::iter::once(kind.to_string())
                .chain((0..n).map(|i| char::from_u32(f(i)).unwrap().to_string()))
                .collect()
        };
        s.feed(packet("\x1bP", 768, |i| 255 - i % 256).as_bytes());
        let frame = packet("\x1bF", 64000, |i| i % 256);
        let (a, b) = frame.split_at(frame.char_indices().nth(1000).unwrap().0);
        s.feed(a.as_bytes());
        assert!(!s.fresh);
        s.feed(b.as_bytes());
        assert!(s.fresh);
        assert_eq!(s.pixels[300], 44); // 300 % 256
        let mut rgba = vec![0; WIDTH * HEIGHT * 4];
        s.rgba(&mut rgba);
        // pixel 1 has index 1: palette bytes 3..6 hold 252, 251, 250
        assert_eq!(rgba[4..8], [252, 251, 250, 255]);
        assert_eq!(s.text, "DOOM\n");
    }

    #[test]
    fn browser_keys_map_to_doom_codes() {
        let keys = [
            "KeyW",
            "Digit3",
            "F1",
            "F10",
            "F11",
            "ControlLeft",
            "ShiftRight",
            "AltLeft",
            "ArrowUp",
            "Space",
            "Comma",
            "KeyAB",
            "MetaLeft",
            "F13",
        ];
        let codes: Vec<Option<i64>> = keys.iter().map(|k| doom_key(k)).collect();
        assert_eq!(
            codes,
            [
                Some(119),
                Some(51),
                Some(0xbb),
                Some(0xc4),
                Some(0xd7),
                Some(0x9d),
                Some(0xb6),
                Some(0xb8),
                Some(0xad),
                Some(32),
                Some(44),
                None,
                None,
                None
            ]
        );
    }
}
