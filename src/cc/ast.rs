//! Typed declarations, untyped expressions: the contract between the C
//! parser (`parse`) and code generation (`gen`).
//!
//! Memory model: every scalar (char, short, int, long, long long, pointer,
//! enum) occupies exactly one heap cell, so `size_of` counts cells. Struct
//! fields are laid out in declaration order without padding; a union is as
//! large as its largest member.

/// Source position, for diagnostics and debug info.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Pos {
    /// Index into `Unit::files`.
    pub file: u32,
    pub line: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IntKind {
    Char,
    Short,
    Int,
    Long,
    LongLong,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Type {
    Void,
    /// Enums are `Int { kind: Int, signed: true }`.
    Int {
        kind: IntKind,
        signed: bool,
    },
    /// float and double; only parsed, code generation rejects arithmetic on them.
    Float,
    Ptr(Box<Type>),
    /// `None` length: incomplete (`extern int a[];`, `int a[] = {...}` before
    /// the parser fills in the initializer count).
    Array(Box<Type>, Option<usize>),
    /// `params == None`: no prototype (`int f();`).
    Func {
        ret: Box<Type>,
        params: Option<Vec<Type>>,
        variadic: bool,
    },
    /// Index into `Unit::structs` (structs and unions).
    Struct(usize),
}

impl Type {
    pub fn int() -> Type {
        Type::Int {
            kind: IntKind::Int,
            signed: true,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Field {
    pub name: String,
    pub ty: Type,
    /// Cell offset from the start of the struct (0 for every union member).
    pub offset: usize,
}

#[derive(Clone, Debug, PartialEq)]
pub struct StructDef {
    pub tag: Option<String>,
    pub is_union: bool,
    /// False until the body has been seen (`struct line_s;` forward declarations).
    pub complete: bool,
    pub fields: Vec<Field>,
    /// Size in cells.
    pub size: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Storage {
    None,
    Extern,
    Static,
    Typedef,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Init {
    Expr(Expr),
    /// `{ a, b, { c } }`; C89 has no designators.
    List(Vec<Init>),
}

#[derive(Clone, Debug, PartialEq)]
pub struct VarDecl {
    pub name: String,
    pub ty: Type,
    pub storage: Storage,
    pub init: Option<Init>,
    pub pos: Pos,
}

#[derive(Clone, Debug, PartialEq)]
pub struct FuncDef {
    pub name: String,
    /// Always `Type::Func`, with the parameter types of the definition.
    pub ty: Type,
    pub storage: Storage,
    /// Parameter names, parallel to the type's parameter list (K&R
    /// definitions are converted to this form).
    pub params: Vec<String>,
    pub body: Stmt,
    pub pos: Pos,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Decl {
    /// Includes `extern` declarations and function prototypes
    /// (`ty` is `Type::Func`, no body). Typedefs are resolved by the parser
    /// and never appear here.
    Var(VarDecl),
    Func(FuncDef),
}

/// One preprocessed and parsed `.c` file.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Unit {
    /// Paths of the files positions refer to (the `.c` file and its headers).
    pub files: Vec<String>,
    pub structs: Vec<StructDef>,
    /// File-scope typedefs (name, resolved type), so later stages can look
    /// struct layouts up by typedef name (`event_t`, `mobj_t`, ...).
    pub typedefs: Vec<(String, Type)>,
    pub decls: Vec<Decl>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UnOp {
    Neg,
    Plus,
    /// `!`
    Not,
    /// `~`
    BitNot,
    Deref,
    Addr,
    PreInc,
    PreDec,
    PostInc,
    PostDec,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    Mod,
    Shl,
    Shr,
    BitAnd,
    BitOr,
    BitXor,
    Lt,
    Gt,
    Le,
    Ge,
    Eq,
    Ne,
    /// `&&`
    And,
    /// `||`
    Or,
    Comma,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Expr {
    /// Integer constant with the C type its spelling and value give it
    /// (e.g. `0xffffffff` is unsigned int, `1u` unsigned, `5L` long).
    Int(i64, Type),
    Float(f64),
    /// String literal bytes after escapes and concatenation, without the NUL.
    Str(Vec<u8>),
    /// Variable, function or enumerator name. Enumerators are replaced by
    /// `Int` in the parser.
    Ident(String),
    Unary(UnOp, Box<Expr>),
    Binary(BinOp, Box<Expr>, Box<Expr>),
    /// `a = b` (`None`) or `a op= b`.
    Assign(Option<BinOp>, Box<Expr>, Box<Expr>),
    Cond(Box<Expr>, Box<Expr>, Box<Expr>),
    Call(Box<Expr>, Vec<Expr>),
    Index(Box<Expr>, Box<Expr>),
    /// `a.b`
    Member(Box<Expr>, String),
    /// `a->b`
    Arrow(Box<Expr>, String),
    Cast(Type, Box<Expr>),
    SizeofType(Type),
    SizeofExpr(Box<Expr>),
}

#[derive(Clone, Debug, PartialEq)]
pub enum StmtKind {
    Expr(Expr),
    /// Block-scope declarations (auto or static), in order.
    Decl(Vec<VarDecl>),
    Block(Vec<Stmt>),
    If(Expr, Box<Stmt>, Option<Box<Stmt>>),
    While(Expr, Box<Stmt>),
    DoWhile(Box<Stmt>, Expr),
    /// The init clause is an expression statement or a declaration.
    For(Option<Box<Stmt>>, Option<Expr>, Option<Expr>, Box<Stmt>),
    Switch(Expr, Box<Stmt>),
    /// `case CONST:` with the constant already evaluated; labels the inner statement.
    Case(i64, Box<Stmt>),
    Default(Box<Stmt>),
    Break,
    Continue,
    Return(Option<Expr>),
    Goto(String),
    Label(String, Box<Stmt>),
    Empty,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Stmt {
    pub kind: StmtKind,
    pub pos: Pos,
}
