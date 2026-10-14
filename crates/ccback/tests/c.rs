//! C semantics of compiled programs, checked by running them.

use wsdoom::cc::symbols::Builtin;
use wsdoom::dsl::Prog;
use wsdoom::vm;

/// Compile one C file (with `putint(int)` printing a number and a space)
/// and run its main().
fn run(src: &str) -> String {
    let dir = std::env::temp_dir().join(format!("ccback-test-{}-{:p}", std::process::id(), src));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("t.c");
    std::fs::write(&path, src).unwrap();
    let unit = ccfront::parse_file(&path, &[], &[]).unwrap();
    let mut p = Prog::new();
    let builtins = [Builtin {
        name: "putint",
        arity: 1,
        variadic: false,
    }];
    let (syms, _) = ccback::compile(&mut p, &[unit], &builtins).unwrap();
    let a = p.var();
    p.def(syms.func("putint"), &[a], |p| {
        p.putn(a);
        p.putc(' ');
    });
    p.eval(syms.func("main").call([]));
    let code = vm::parse(&vm::encode(&p.finish())).unwrap();
    let mut out = vec![];
    let done = vm::Machine::default()
        .run(&code, &mut |_: &mut Vec<u8>| -1, &mut out, 100_000_000)
        .unwrap();
    assert!(
        done,
        "still running; output so far: {}",
        String::from_utf8_lossy(&out)
    );
    String::from_utf8(out).unwrap()
}

#[test]
fn unsigned_division_of_all_ones() {
    // r_segs.c: dc_iscale = 0xffffffffu / (unsigned)rw_scale
    let out = run("void putint(int);
        int scale = 0x20000;
        int main(void) { int s = 0xffffffffu / (unsigned)scale; putint(s); putint(0xffffffffu / 256); return 0; }");
    assert_eq!(out, "32767 16777215 ");
}

#[test]
fn bitwise_with_constant_masks() {
    // fast paths for constant masks must agree with two's complement bits
    let out = run("void putint(int);
        int v[6] = { 0, 1, -1, 4000000, -123456789, 0x7fffffff };
        int main(void) {
            int i;
            unsigned u = 0xdeadbeef;
            for (i = 0; i < 6; i++) {
                int x = v[i];
                putint(x & (63*64)); putint(x & ~0xff); putint(x & 0x0f0f); putint(x & 8);
                putint(x | 0x30); putint(x | ~7); putint(x ^ 0x5a5a); putint(x ^ -1);
            }
            putint((int)(u & ~0xffu)); putint((int)(u | 0x10)); putint((int)(u ^ ~0u));
            return 0;
        }");
    let v = [0i32, 1, -1, 4000000, -123456789, 0x7fffffff];
    let mut want = String::new();
    for x in v {
        for r in [
            x & (63 * 64),
            x & !0xff,
            x & 0x0f0f,
            x & 8,
            x | 0x30,
            x | !7,
            x ^ 0x5a5a,
            x ^ -1,
        ] {
            want += &format!("{r} ");
        }
    }
    let u = 0xdeadbeefu32;
    for r in [u & !0xff, u | 0x10, !u] {
        want += &format!("{} ", r as i32);
    }
    assert_eq!(out, want);
}
