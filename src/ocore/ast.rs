use super::Span;

pub type Path = Vec<String>;

#[derive(Debug, Clone, PartialEq)]
pub struct SourceModule {
    pub name: Path,
    pub uses: Vec<UseDecl>,
    pub items: Vec<Item>,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq)]
pub struct UseDecl {
    pub path: Path,
    pub alias: Option<String>,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Attribute {
    pub name: String,
    pub args: Vec<AttrArg>,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq)]
pub enum AttrArg {
    String(String),
    Integer(u64),
    Ident(String),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Item {
    pub attrs: Vec<Attribute>,
    pub public: bool,
    pub kind: ItemKind,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ItemKind {
    Function(Function),
    Struct(StructDef),
    Enum(EnumDef),
    Static(StaticDef),
    Const(ConstDef),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Function {
    pub name: String,
    pub unsafe_: bool,
    pub abi: Abi,
    pub params: Vec<Param>,
    pub return_type: TypeExpr,
    pub body: Option<Block>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Abi {
    OCore,
    SysV64,
    Interrupt,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Param {
    pub name: String,
    pub ty: TypeExpr,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq)]
pub struct StructDef {
    pub name: String,
    pub fields: Vec<FieldDef>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct FieldDef {
    pub name: String,
    pub ty: TypeExpr,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq)]
pub struct EnumDef {
    pub name: String,
    pub variants: Vec<VariantDef>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct VariantDef {
    pub name: String,
    pub payload: Vec<TypeExpr>,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq)]
pub struct StaticDef {
    pub name: String,
    pub mutable: bool,
    pub ty: TypeExpr,
    pub init: Expr,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ConstDef {
    pub name: String,
    pub ty: TypeExpr,
    pub init: Expr,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TypeExpr {
    pub kind: TypeExprKind,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq)]
pub enum TypeExprKind {
    Named(Path),
    Pointer {
        mutable: bool,
        pointee: Box<TypeExpr>,
    },
    Array {
        element: Box<TypeExpr>,
        len: u64,
    },
    FnPointer {
        params: Vec<TypeExpr>,
        result: Box<TypeExpr>,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub struct Block {
    pub stmts: Vec<Stmt>,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Stmt {
    pub kind: StmtKind,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq)]
pub enum StmtKind {
    Let {
        mutable: bool,
        name: String,
        ty: Option<TypeExpr>,
        init: Expr,
    },
    Expr(Expr),
    If {
        condition: Expr,
        then_block: Block,
        else_block: Option<Block>,
    },
    While {
        condition: Expr,
        body: Block,
    },
    Loop(Block),
    Unsafe(Block),
    Return(Option<Expr>),
    Break,
    Continue,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Expr {
    pub kind: ExprKind,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ExprKind {
    Integer(u64),
    Bool(bool),
    Byte(u8),
    String(String),
    ByteString(Vec<u8>),
    Path(Path),
    Array(Vec<Expr>),
    ArrayRepeat {
        value: Box<Expr>,
        len: u64,
    },
    Struct {
        path: Path,
        fields: Vec<(String, Expr)>,
    },
    Unary {
        op: UnaryOp,
        operand: Box<Expr>,
    },
    Binary {
        op: BinaryOp,
        lhs: Box<Expr>,
        rhs: Box<Expr>,
    },
    Assign {
        op: Option<BinaryOp>,
        target: Box<Expr>,
        value: Box<Expr>,
    },
    Call {
        callee: Box<Expr>,
        args: Vec<Expr>,
    },
    Field {
        base: Box<Expr>,
        name: String,
    },
    Index {
        base: Box<Expr>,
        index: Box<Expr>,
    },
    Cast {
        value: Box<Expr>,
        ty: TypeExpr,
    },
    Asm(AsmExpr),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnaryOp {
    Neg,
    Not,
    BitNot,
    Deref,
    AddressOf { mutable: bool },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinaryOp {
    Add,
    Sub,
    Mul,
    Div,
    Rem,
    BitAnd,
    BitOr,
    BitXor,
    ShiftLeft,
    ShiftRight,
    Eq,
    NotEq,
    Less,
    LessEq,
    Greater,
    GreaterEq,
    LogicalAnd,
    LogicalOr,
}

#[derive(Debug, Clone, PartialEq)]
pub struct AsmExpr {
    pub template: String,
    pub operands: Vec<AsmOperand>,
    pub options: Vec<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum AsmOperand {
    In {
        register: String,
        value: Expr,
    },
    Out {
        register: String,
        target: Expr,
    },
    InOut {
        register: String,
        input: Expr,
        output: Expr,
    },
}
