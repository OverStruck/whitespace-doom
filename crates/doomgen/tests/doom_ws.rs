use wsdoom::vm;

/// The build reads wad/doom1.wad.
fn have_wad() -> bool {
    let there = doomgen::root().join("wad/doom1.wad").exists();
    if !there {
        eprintln!("skipped: no wad/doom1.wad");
    }
    there
}

#[test]
fn doom_ws_is_pure_whitespace() {
    let shipped = std::fs::read(doomgen::root().join("doom.ws")).unwrap();
    assert!(
        shipped.iter().all(|c| matches!(c, b' ' | b'\t' | b'\n')),
        "doom.ws holds other characters"
    );
}

#[test]
fn doom_ws_is_current() {
    if !have_wad() {
        return;
    }
    let ws = vm::encode(&doomgen::build().unwrap().code);
    let shipped = std::fs::read_to_string(doomgen::root().join("doom.ws")).unwrap();
    assert!(
        ws == shipped,
        "doom.ws is stale: run `cargo run --release -p doomgen`"
    );
}

#[test]
fn boots_to_the_title_screen() {
    if !have_wad() {
        return;
    }
    let b = doomgen::build().unwrap();
    let code = b.parsed();
    // clock running, terminal reports 50 rows x 160 columns
    let mut input = doomgen::script(".\\e[50;160R").into_iter();
    let (mut m, mut out) = (vm::Machine::default(), vec![]);
    let ended = m
        .run(
            &code,
            &mut |_: &mut Vec<u8>| input.next().unwrap_or(0x81),
            &mut out,
            400_000_000,
        )
        .unwrap();
    let text = String::from_utf8_lossy(&out);
    assert!(!ended, "DOOM quit: {text}");
    assert!(text.contains("DOOM Shareware Startup v1.10"));
    assert!(text.contains("ST_Init: Init status bar."));
    // a full frame of 50 rows of half blocks follows the start-up messages
    let frame = text.split("\x1b[H").nth(1).expect("no frame");
    assert_eq!(frame.split("\r\n").filter(|r| r.contains('▀')).count(), 50);
}
