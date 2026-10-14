# DOOM in Whitespace

The original DOOM — id Software's linuxdoom-1.10 C source, unmodified — compiled to
[Whitespace](https://en.wikipedia.org/wiki/Whitespace_(programming_language)), a language
whose only characters are space, tab and newline. [`doom.ws`](doom.ws) is the whole game,
engine and shareware episode: 36 MB of whitespace.

**Play it: https://overstruck.github.io/whitespace-doom/**

Arrows move, Ctrl fires, Space opens, Shift runs, Alt strafes, 1–7 pick weapons, Tab shows
the map, Esc opens the menu.

## How it works

The Rust workspace is a C compiler and a Whitespace interpreter:

- `crates/ccfront` preprocesses and parses the C89 source; `crates/ccback` compiles it
  to Whitespace. Memory is one heap cell per C scalar (`sizeof` counts cells).
- `crates/runtime` (libc, game data access, savegames) and `crates/platform` (DOOM's
  `i_*.c`: video, keyboard, timer) are written in a Rust DSL that emits Whitespace
  (`src/dsl.rs`).
- The levels, graphics and demos become heap cells that the program stores at start-up.
- `crates/doomgen` links everything into `doom.ws`.

Whitespace programs can only read and print characters, so that is how DOOM talks to its
host:

- **Time:** when no key is waiting, the host sends byte `0x80+N` for N elapsed tics of
  1/35 s. Without it (any standard interpreter) DOOM runs turn by turn: 4 tics per key.
- **Screen:** in a terminal DOOM prints each frame as `▀` characters in 256 colours.
  The web page answers DOOM's screen-size query with `0;0`, then gets raw 320×200
  frames and palettes and sends real key presses and releases.

The web page runs `doom.ws` in the interpreter (`src/vm.rs`) compiled to WebAssembly
(`crates/web`).

## Run it locally

```sh
cargo build --release
./target/release/doom             # serves the page and opens it in a browser window
./target/release/doom --terminal  # plays in the terminal (z fires, always runs)
./target/release/doom other.ws    # runs any Whitespace program
```

## Rebuild

```sh
cargo run --release -p doomgen    # writes doom.ws
cargo test --release --workspace
```

The page's `pkg/` needs the `wasm32-unknown-unknown` target and wasm-bindgen-cli 0.2.129:

```sh
cargo build --release -p doom-web --target wasm32-unknown-unknown
wasm-bindgen --target web --no-typescript --out-dir pkg target/wasm32-unknown-unknown/release/doom_web.wasm
```

## Differences from DOS DOOM

No sound; saved games last until you quit or reload; the three demos' version byte
says 1.10 so linuxdoom plays them.

## License

GPL-2.0 ([LICENSE](LICENSE)). DOOM's source in `vendor/` is id Software's, released under
the GPL in 1999 (the files keep their 1997 headers). The game data inside `doom.ws` is
id Software's freely distributable shareware.
