use super::parse_file;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use wsdoom::cc::ast::*;

fn doom_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../vendor/linuxdoom-1.10")
}

fn parse_doom(file: &str) -> Unit {
    let dir = doom_dir();
    parse_file(
        &dir.join(file),
        std::slice::from_ref(&dir),
        &[("NORMALUNIX", "1"), ("LINUX", "1")],
    )
    .unwrap_or_else(|e| panic!("{e}"))
}

fn try_src(src: &str) -> Result<Unit, String> {
    static N: AtomicUsize = AtomicUsize::new(0);
    let dir = std::env::temp_dir().join(format!(
        "ccfront-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("t.c");
    std::fs::write(&path, src).unwrap();
    let r = parse_file(&path, &[], &[]);
    std::fs::remove_dir_all(&dir).unwrap();
    r
}

fn src(s: &str) -> Unit {
    try_src(s).unwrap_or_else(|e| panic!("{e}"))
}

fn var<'a>(u: &'a Unit, name: &str) -> &'a VarDecl {
    u.decls
        .iter()
        .rev()
        .find_map(|d| match d {
            Decl::Var(v) if v.name == name => Some(v),
            _ => None,
        })
        .unwrap_or_else(|| panic!("no declaration of {name}"))
}

fn func<'a>(u: &'a Unit, name: &str) -> &'a FuncDef {
    u.decls
        .iter()
        .find_map(|d| match d {
            Decl::Func(f) if f.name == name => Some(f),
            _ => None,
        })
        .unwrap_or_else(|| panic!("no definition of {name}"))
}

fn typedef<'a>(u: &'a Unit, name: &str) -> &'a Type {
    &u.typedefs
        .iter()
        .find(|t| t.0 == name)
        .unwrap_or_else(|| panic!("no typedef {name}"))
        .1
}

fn layout<'a>(u: &'a Unit, name: &str) -> &'a StructDef {
    match typedef(u, name) {
        Type::Struct(i) => &u.structs[*i],
        t => panic!("{name} is {t:?}"),
    }
}

fn offset(s: &StructDef, f: &str) -> usize {
    s.fields
        .iter()
        .find(|x| x.name == f)
        .unwrap_or_else(|| panic!("no field {f}"))
        .offset
}

fn int(kind: IntKind, signed: bool) -> Type {
    Type::Int { kind, signed }
}

fn ptr(t: Type) -> Type {
    Type::Ptr(Box::new(t))
}

fn init_expr(v: &VarDecl) -> &Expr {
    match &v.init {
        Some(Init::Expr(e)) => e,
        i => panic!("{}: init {i:?}", v.name),
    }
}

#[test]
fn parses_every_compiled_doom_file() {
    const SKIP: [&str; 5] = [
        "i_main.c",
        "i_system.c",
        "i_video.c",
        "i_sound.c",
        "i_net.c",
    ];
    let mut n = 0;
    let mut errors = Vec::new();
    for e in std::fs::read_dir(doom_dir()).unwrap() {
        let name = e.unwrap().file_name().into_string().unwrap();
        if name.ends_with(".c") && !SKIP.contains(&&*name) {
            let dir = doom_dir();
            match parse_file(
                &dir.join(&name),
                std::slice::from_ref(&dir),
                &[("NORMALUNIX", "1"), ("LINUX", "1")],
            ) {
                Ok(u) => assert!(!u.decls.is_empty(), "{name}"),
                Err(e) => errors.push(e),
            }
            n += 1;
        }
    }
    assert!(errors.is_empty(), "{}", errors.join("\n"));
    assert!(n >= 50, "only {n} files");
}

#[test]
fn doom_struct_layouts() {
    let u = parse_doom("p_mobj.c");
    // thinker_t: prev, next, function (actionf_t: union of 3 pointers = 1 cell).
    let th = layout(&u, "thinker_t");
    assert_eq!(th.size, 3);
    assert_eq!(layout(&u, "actionf_t").size, 1);
    assert!(layout(&u, "actionf_t").is_union);
    let st = layout(&u, "state_t");
    assert_eq!(st.size, 7);
    assert_eq!(offset(st, "action"), 3);
    assert_eq!(offset(st, "nextstate"), 4);
    assert_eq!(offset(st, "misc2"), 6);
    // mapthing_t: five shorts.
    assert_eq!(layout(&u, "mapthing_t").size, 5);
    let mo = layout(&u, "mobj_t");
    assert_eq!(mo.tag.as_deref(), Some("mobj_s"));
    assert_eq!(offset(mo, "x"), 3);
    assert_eq!(offset(mo, "angle"), 8);
    assert_eq!(offset(mo, "subsector"), 13);
    assert_eq!(offset(mo, "type"), 22);
    assert_eq!(offset(mo, "state"), 25);
    assert_eq!(offset(mo, "spawnpoint"), 35);
    assert_eq!(offset(mo, "tracer"), 40);
    assert_eq!(mo.size, 41);
    // Function pointer typedef from d_think.h.
    let Type::Ptr(f) = typedef(&u, "actionf_v") else {
        panic!()
    };
    assert_eq!(
        **f,
        Type::Func {
            ret: Box::new(Type::Void),
            params: None,
            variadic: false
        }
    );
    let Type::Ptr(f) = typedef(&u, "actionf_p2") else {
        panic!()
    };
    assert_eq!(
        **f,
        Type::Func {
            ret: Box::new(Type::Void),
            params: Some(vec![ptr(Type::Void), ptr(Type::Void)]),
            variadic: false
        }
    );
}

#[test]
fn doom_enum_sized_arrays() {
    let u = parse_doom("info.c");
    let states = var(&u, "states");
    let Type::Array(_, Some(n)) = states.ty else {
        panic!("{:?}", states.ty)
    };
    assert_eq!(n, 967);
    let Some(Init::List(items)) = &states.init else {
        panic!()
    };
    assert_eq!(items.len(), 967);
    let Type::Array(_, Some(n)) = var(&u, "mobjinfo").ty else {
        panic!()
    };
    assert_eq!(n, 137);
    let Type::Array(_, Some(n)) = var(&u, "sprnames").ty else {
        panic!()
    };
    assert_eq!(n, 138);
}

#[test]
fn tricky_declarators() {
    let u = src("typedef void (*actionf_v)();
        typedef struct line_s line_t;
        typedef int boolean;
        int check(boolean (*func)(line_t*), int x);
        char *names[4];
        int (*pa)[3];
        void (*table[2])(int);
        int *(*getf(int a))(char c) { return 0; }
        typedef int fn_t(int);
        fn_t *fp;
        void (*signal(int sig, void (*h)(int)))(int);");
    assert_eq!(
        *typedef(&u, "actionf_v"),
        ptr(Type::Func {
            ret: Box::new(Type::Void),
            params: None,
            variadic: false
        })
    );
    let line_t = typedef(&u, "line_t").clone();
    let Type::Func {
        params: Some(p), ..
    } = &var(&u, "check").ty
    else {
        panic!()
    };
    assert_eq!(
        p[0],
        ptr(Type::Func {
            ret: Box::new(Type::int()),
            params: Some(vec![ptr(line_t)]),
            variadic: false
        })
    );
    assert_eq!(
        var(&u, "names").ty,
        Type::Array(Box::new(ptr(int(IntKind::Char, true))), Some(4))
    );
    assert_eq!(
        var(&u, "pa").ty,
        ptr(Type::Array(Box::new(Type::int()), Some(3)))
    );
    let fv = Type::Func {
        ret: Box::new(Type::Void),
        params: Some(vec![Type::int()]),
        variadic: false,
    };
    assert_eq!(
        var(&u, "table").ty,
        Type::Array(Box::new(ptr(fv.clone())), Some(2))
    );
    let g = func(&u, "getf");
    assert_eq!(g.params, vec!["a".to_string()]);
    let ret = ptr(Type::Func {
        ret: Box::new(ptr(Type::int())),
        params: Some(vec![int(IntKind::Char, true)]),
        variadic: false,
    });
    assert_eq!(
        g.ty,
        Type::Func {
            ret: Box::new(ret),
            params: Some(vec![Type::int()]),
            variadic: false
        }
    );
    let fnt = Type::Func {
        ret: Box::new(Type::int()),
        params: Some(vec![Type::int()]),
        variadic: false,
    };
    assert_eq!(var(&u, "fp").ty, ptr(fnt));
    let Type::Func {
        ret,
        params: Some(p),
        ..
    } = &var(&u, "signal").ty
    else {
        panic!()
    };
    assert_eq!(**ret, ptr(fv.clone()));
    assert_eq!(p[1], ptr(fv));
}

#[test]
fn function_conventions() {
    let u = src("int printf(char *fmt, ...);
        old(a, b) char *b; { return a; }
        void none() {}
        static int arr(int v[10]) { return v[0]; }");
    let Type::Func { variadic, .. } = var(&u, "printf").ty else {
        panic!()
    };
    assert!(variadic);
    let f = func(&u, "old");
    assert_eq!(
        f.ty,
        Type::Func {
            ret: Box::new(Type::int()),
            params: Some(vec![Type::int(), ptr(int(IntKind::Char, true))]),
            variadic: false
        }
    );
    assert_eq!(f.params, vec!["a".to_string(), "b".to_string()]);
    assert_eq!(
        func(&u, "none").ty,
        Type::Func {
            ret: Box::new(Type::Void),
            params: Some(vec![]),
            variadic: false
        }
    );
    let a = func(&u, "arr");
    assert_eq!(a.storage, Storage::Static);
    assert_eq!(
        a.ty,
        Type::Func {
            ret: Box::new(Type::int()),
            params: Some(vec![ptr(Type::int())]),
            variadic: false
        }
    );
}

#[test]
fn integer_constant_types() {
    let u = src("unsigned a = 0xffffffff; long b = 2147483648; int c = 2147483647; int d = 017;
        long e = 5L; unsigned f = 1u; long long g = 4294967296; int h = 0x80000000; int i = 'a'; int j = '\\377';
        int k = '\\n'; long long l = 0x100000000; unsigned long m = 7ul;");
    let t = |n: &str| init_expr(var(&u, n)).clone();
    use IntKind::*;
    assert_eq!(t("a"), Expr::Int(0xffffffff, int(Int, false)));
    assert_eq!(t("b"), Expr::Int(2147483648, int(Long, false)));
    assert_eq!(t("c"), Expr::Int(2147483647, int(Int, true)));
    assert_eq!(t("d"), Expr::Int(15, int(Int, true)));
    assert_eq!(t("e"), Expr::Int(5, int(Long, true)));
    assert_eq!(t("f"), Expr::Int(1, int(Int, false)));
    assert_eq!(t("g"), Expr::Int(4294967296, int(LongLong, true)));
    assert_eq!(t("h"), Expr::Int(0x80000000, int(Int, false)));
    assert_eq!(t("i"), Expr::Int(97, Type::int()));
    assert_eq!(t("j"), Expr::Int(-1, Type::int()));
    assert_eq!(t("k"), Expr::Int(10, Type::int()));
    assert_eq!(t("l"), Expr::Int(0x100000000, int(LongLong, true)));
    assert_eq!(t("m"), Expr::Int(7, int(Long, false)));
}

#[test]
fn strings_and_initializer_sizes() {
    let u = src("#define GREET \"hel\" \"lo\"
        char s[] = GREET \"\\x21\\n\";
        char t[] = {\"ab\"};
        int a[] = {1, 2, 3,};
        struct p { int x, y; } ps[] = {1, 2, 3, 4, 5};
        struct p pb[] = {{1, 2}, {3, 4}};
        char *names[] = {\"a\", \"b\"};
        char grid[][3] = {\"ab\", \"cd\", \"ef\"};");
    let s = var(&u, "s");
    assert_eq!(*init_expr(s), Expr::Str(b"hello!\n".to_vec()));
    assert_eq!(
        s.ty,
        Type::Array(Box::new(int(IntKind::Char, true)), Some(8))
    );
    let len = |n: &str| match var(&u, n).ty {
        Type::Array(_, Some(n)) => n,
        ref t => panic!("{t:?}"),
    };
    assert_eq!(len("t"), 3);
    assert_eq!(len("a"), 3);
    assert_eq!(len("ps"), 3);
    assert_eq!(len("pb"), 2);
    assert_eq!(len("names"), 2);
    assert_eq!(len("grid"), 3);
}

#[test]
fn enums_and_case_labels() {
    let u = src(
        "enum color { RED, GREEN = 5, BLUE, LAST = BLUE * 2 + (1 << 3) };
        typedef struct { int a[4]; char c; } s_t;
        int arr[LAST];
        int sz[sizeof(s_t) + sizeof arr / sizeof arr[0]];
        int f(int x) {
            switch (x) {
            case RED: return 1;
            case 'A': return 2;
            case -1: return 3;
            case (unsigned)-1 > 0 ? 7 : 8: return 4;
            case sizeof(s_t): return 5;
            default: return BLUE;
            }
        }",
    );
    let len = |n: &str| match var(&u, n).ty {
        Type::Array(_, Some(n)) => n,
        ref t => panic!("{t:?}"),
    };
    assert_eq!(len("arr"), 6 * 2 + 8);
    assert_eq!(len("sz"), 5 + 20);
    let StmtKind::Block(b) = &func(&u, "f").body.kind else {
        panic!()
    };
    let StmtKind::Switch(_, body) = &b[0].kind else {
        panic!()
    };
    let StmtKind::Block(cases) = &body.kind else {
        panic!()
    };
    let labels: Vec<i64> = cases
        .iter()
        .filter_map(|c| {
            if let StmtKind::Case(v, _) = c.kind {
                Some(v)
            } else {
                None
            }
        })
        .collect();
    assert_eq!(labels, vec![0, 65, -1, 7, 5]);
    let StmtKind::Default(d) = &cases[5].kind else {
        panic!()
    };
    assert_eq!(d.kind, StmtKind::Return(Some(Expr::Int(6, Type::int()))));
}

#[test]
fn block_scope_typedefs_and_tags() {
    let u = src("typedef int T;
        struct s { int a; };
        int f(void) {
            T x;
            {
                typedef char *T;
                struct s { int a, b, c; } y;
                T z;
                int n = sizeof(struct s);
            }
            {
                int T = 3;
                return T * 2;
            }
        }
        int g = sizeof(struct s);");
    let StmtKind::Block(b) = &func(&u, "f").body.kind else {
        panic!()
    };
    let StmtKind::Block(inner) = &b[1].kind else {
        panic!()
    };
    let StmtKind::Decl(z) = &inner[2].kind else {
        panic!("{:?}", inner[2])
    };
    assert_eq!(z[0].ty, ptr(int(IntKind::Char, true)));
    let StmtKind::Decl(n) = &inner[3].kind else {
        panic!()
    };
    assert_eq!(
        n[0].init,
        Some(Init::Expr(Expr::SizeofType(Type::Struct(2))))
    );
    assert_eq!(u.structs[2].size, 3);
    assert_eq!(
        var(&u, "g").init,
        Some(Init::Expr(Expr::SizeofType(Type::Struct(1))))
    );
    assert_eq!(u.structs[1].size, 1);
    // `T * 2` with T shadowed by a variable is a multiplication.
    let StmtKind::Block(second) = &b[2].kind else {
        panic!()
    };
    let StmtKind::Return(Some(Expr::Binary(BinOp::Mul, _, _))) = &second[1].kind else {
        panic!("{:?}", second[1])
    };
}

#[test]
fn structs_forward_unions_nested() {
    let u = src("struct node;
        typedef struct node *link;
        struct node { link next; int v[3]; union { int i; struct { int a, b; } pair; } u; };
        struct node n;");
    let s = &u.structs[1];
    assert!(s.complete);
    assert_eq!(s.tag.as_deref(), Some("node"));
    assert_eq!(
        s.fields.iter().map(|f| f.offset).collect::<Vec<_>>(),
        vec![0, 1, 4]
    );
    assert_eq!(s.size, 6);
    assert_eq!(*typedef(&u, "link"), ptr(Type::Struct(1)));
    assert_eq!(var(&u, "n").ty, Type::Struct(1));
}

#[test]
fn expressions_precedence_and_casts() {
    let u = src("typedef unsigned char byte;
        int a, b, *p;
        int f(void) { return a + b * 2 << 1 == 3 && !p || a ? (byte *)p - (byte*)0 : -a; }
        int g(void) { a = b += 2, p[1]++; return *p++ + sizeof(int (*)[4]); }");
    let StmtKind::Block(b) = &func(&u, "f").body.kind else {
        panic!()
    };
    let StmtKind::Return(Some(Expr::Cond(c, t, _))) = &b[0].kind else {
        panic!()
    };
    let Expr::Binary(BinOp::Or, l, _) = &**c else {
        panic!("{c:?}")
    };
    let Expr::Binary(BinOp::And, eq, _) = &**l else {
        panic!()
    };
    let Expr::Binary(BinOp::Eq, sh, _) = &**eq else {
        panic!()
    };
    let Expr::Binary(BinOp::Shl, add, _) = &**sh else {
        panic!()
    };
    let Expr::Binary(BinOp::Add, _, mul) = &**add else {
        panic!()
    };
    assert!(matches!(**mul, Expr::Binary(BinOp::Mul, _, _)));
    let Expr::Binary(BinOp::Sub, cast, _) = &**t else {
        panic!()
    };
    assert_eq!(
        **cast,
        Expr::Cast(
            ptr(int(IntKind::Char, false)),
            Box::new(Expr::Ident("p".into()))
        )
    );
    let StmtKind::Block(b) = &func(&u, "g").body.kind else {
        panic!()
    };
    let StmtKind::Expr(Expr::Binary(BinOp::Comma, asg, _)) = &b[0].kind else {
        panic!()
    };
    let Expr::Assign(None, _, rhs) = &**asg else {
        panic!()
    };
    assert!(matches!(**rhs, Expr::Assign(Some(BinOp::Add), _, _)));
    let StmtKind::Return(Some(Expr::Binary(BinOp::Add, _, sz))) = &b[1].kind else {
        panic!()
    };
    assert_eq!(
        **sz,
        Expr::SizeofType(ptr(Type::Array(Box::new(Type::int()), Some(4))))
    );
}

#[test]
fn preprocessor() {
    let u = src("#define SQ(x) ((x) * (x))
        #define TWICE(f, v) f(f(v))
        #define STR(x) #x
        #define CAT(a, b) a ## b
        #if defined(SQ) && !defined NOPE && (1 << 4) == 16 && -1 < 0 && 0xffffffff > 0
        int ok = TWICE(SQ, 2);
        #elif 1
        #error wrong branch
        #else
        int bad;
        #endif
        #ifdef NOPE
        # error skipped
        #endif
        #if 0
        it's unterminated
        #endif
        char *s = STR(a  \"b\"  + c);
        int CAT(var, 2) = MAXINT;
        int line = \\
          1;
        /* multi
           line */ int after;");
    let sq = |e: Expr| {
        let p = Box::new(e);
        Expr::Binary(BinOp::Mul, p.clone(), p)
    };
    assert_eq!(*init_expr(var(&u, "ok")), sq(sq(Expr::Int(2, Type::int()))));
    assert_eq!(*init_expr(var(&u, "s")), Expr::Str(b"a \"b\" + c".to_vec()));
    assert_eq!(
        *init_expr(var(&u, "var2")),
        Expr::Int(2147483647, Type::int())
    );
    assert_eq!(var(&u, "after").pos.line, 23);
    assert!(try_src("#error boom\n").unwrap_err().contains("boom"));
    assert!(try_src("#define").is_err());
}

#[test]
fn self_referential_macro_is_not_reexpanded() {
    // `int self;` becomes `int self + 1;`: expanded once, never recursively.
    assert!(try_src("#define self self + 1\nint self;").is_err());
    let u = src("#define self self + 1\nint x = self;");
    assert_eq!(
        *init_expr(var(&u, "x")),
        Expr::Binary(
            BinOp::Add,
            Box::new(Expr::Ident("self".into())),
            Box::new(Expr::Int(1, Type::int()))
        )
    );
}

#[test]
fn positions_and_includes() {
    let dir = std::env::temp_dir().join(format!("ccfront-inc-{}", std::process::id()));
    std::fs::create_dir_all(dir.join("inc")).unwrap();
    std::fs::write(
        dir.join("inc/h.h"),
        "#include <stdio.h>\n\nextern int fromh;\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("m.c"),
        "#include \"h.h\"\nint main(void)\n{\n  return\n fromh;\n}\n",
    )
    .unwrap();
    let u = parse_file(&dir.join("m.c"), &[dir.join("inc")], &[]).unwrap();
    std::fs::remove_dir_all(&dir).unwrap();
    let h = var(&u, "fromh");
    assert!(u.files[h.pos.file as usize].ends_with("h.h"));
    assert_eq!(h.pos.line, 3);
    let m = func(&u, "main");
    assert!(u.files[m.pos.file as usize].ends_with("m.c"));
    assert_eq!(m.pos.line, 2);
    let StmtKind::Block(b) = &m.body.kind else {
        panic!()
    };
    assert_eq!(b[0].pos.line, 4);
}
