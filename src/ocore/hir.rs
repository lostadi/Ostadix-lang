use std::collections::HashMap;

use super::ast::{Abi, BinaryOp, UnaryOp};
use super::Span;

pub type TypeId = usize;
pub type StructId = usize;
pub type EnumId = usize;
pub type FunctionId = usize;
pub type StaticId = usize;
pub type ConstId = usize;
pub type LocalId = usize;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Layout {
    pub size: u64,
    pub align: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Type {
    Bool,
    Int {
        signed: bool,
        bits: u16,
    },
    Usize,
    Isize,
    Float {
        bits: u16,
    },
    Void,
    Never,
    Pointer {
        mutable: bool,
        pointee: TypeId,
    },
    Array {
        element: TypeId,
        len: u64,
    },
    Struct(StructId),
    Enum(EnumId),
    Function {
        params: Vec<TypeId>,
        result: TypeId,
        abi: Abi,
    },
}

#[derive(Debug, Clone)]
pub struct StructType {
    pub name: String,
    pub fields: Vec<StructField>,
    pub packed: bool,
    pub requested_align: Option<u64>,
    pub layout: Layout,
}

#[derive(Debug, Clone)]
pub struct StructField {
    pub name: String,
    pub ty: TypeId,
    pub offset: u64,
}

#[derive(Debug, Clone)]
pub struct EnumType {
    pub name: String,
    pub variants: Vec<EnumVariant>,
    pub tag_size: u64,
    pub payload_offset: u64,
    pub layout: Layout,
}

#[derive(Debug, Clone)]
pub struct EnumVariant {
    pub name: String,
    pub payload: Vec<TypeId>,
    pub payload_layout: Layout,
}

#[derive(Debug, Clone)]
pub struct TypeContext {
    pub types: Vec<Type>,
    pub structs: Vec<StructType>,
    pub enums: Vec<EnumType>,
    interned: HashMap<Type, TypeId>,
    primitive_names: HashMap<String, TypeId>,
}

impl TypeContext {
    pub fn new() -> Self {
        let mut cx = Self {
            types: Vec::new(),
            structs: Vec::new(),
            enums: Vec::new(),
            interned: HashMap::new(),
            primitive_names: HashMap::new(),
        };
        cx.add_primitive("bool", Type::Bool);
        for bits in [8, 16, 32, 64] {
            cx.add_primitive(
                &format!("u{bits}"),
                Type::Int {
                    signed: false,
                    bits,
                },
            );
            cx.add_primitive(&format!("i{bits}"), Type::Int { signed: true, bits });
        }
        cx.add_primitive("usize", Type::Usize);
        cx.add_primitive("isize", Type::Isize);
        cx.add_primitive("f32", Type::Float { bits: 32 });
        cx.add_primitive("f64", Type::Float { bits: 64 });
        cx.add_primitive("void", Type::Void);
        cx.add_primitive("never", Type::Never);
        cx
    }

    fn add_primitive(&mut self, name: &str, ty: Type) {
        let id = self.intern(ty);
        self.primitive_names.insert(name.to_string(), id);
    }

    pub fn primitive(&self, name: &str) -> Option<TypeId> {
        self.primitive_names.get(name).copied()
    }

    pub fn intern(&mut self, ty: Type) -> TypeId {
        if let Some(id) = self.interned.get(&ty) {
            return *id;
        }
        let id = self.types.len();
        self.types.push(ty.clone());
        self.interned.insert(ty, id);
        id
    }

    pub fn add_struct_placeholder(&mut self, name: String) -> (StructId, TypeId) {
        let struct_id = self.structs.len();
        self.structs.push(StructType {
            name,
            fields: Vec::new(),
            packed: false,
            requested_align: None,
            layout: Layout { size: 0, align: 1 },
        });
        let ty = self.intern(Type::Struct(struct_id));
        (struct_id, ty)
    }

    pub fn add_enum_placeholder(&mut self, name: String) -> (EnumId, TypeId) {
        let enum_id = self.enums.len();
        self.enums.push(EnumType {
            name,
            variants: Vec::new(),
            tag_size: 1,
            payload_offset: 1,
            layout: Layout { size: 1, align: 1 },
        });
        let ty = self.intern(Type::Enum(enum_id));
        (enum_id, ty)
    }

    pub fn layout(&self, ty: TypeId) -> Layout {
        match &self.types[ty] {
            Type::Bool | Type::Int { bits: 8, .. } => Layout { size: 1, align: 1 },
            Type::Int { bits: 16, .. } => Layout { size: 2, align: 2 },
            Type::Int { bits: 32, .. } | Type::Float { bits: 32 } => Layout { size: 4, align: 4 },
            Type::Int { bits: 64, .. }
            | Type::Float { bits: 64 }
            | Type::Usize
            | Type::Isize
            | Type::Pointer { .. }
            | Type::Function { .. } => Layout { size: 8, align: 8 },
            Type::Int { bits, .. } | Type::Float { bits } => {
                panic!("invalid primitive width {bits}")
            }
            Type::Void | Type::Never => Layout { size: 0, align: 1 },
            Type::Array { element, len } => {
                let element = self.layout(*element);
                Layout {
                    size: element
                        .size
                        .checked_mul(*len)
                        .expect("array layout overflow"),
                    align: element.align,
                }
            }
            Type::Struct(id) => self.structs[*id].layout,
            Type::Enum(id) => self.enums[*id].layout,
        }
    }

    pub fn name(&self, ty: TypeId) -> String {
        match &self.types[ty] {
            Type::Bool => "bool".into(),
            Type::Int { signed, bits } => format!("{}{}", if *signed { 'i' } else { 'u' }, bits),
            Type::Usize => "usize".into(),
            Type::Isize => "isize".into(),
            Type::Float { bits } => format!("f{bits}"),
            Type::Void => "void".into(),
            Type::Never => "never".into(),
            Type::Pointer { mutable, pointee } => format!(
                "*{} {}",
                if *mutable { "mut" } else { "const" },
                self.name(*pointee)
            ),
            Type::Array { element, len } => format!("[{}; {len}]", self.name(*element)),
            Type::Struct(id) => self.structs[*id].name.clone(),
            Type::Enum(id) => self.enums[*id].name.clone(),
            Type::Function {
                params,
                result,
                abi,
            } => format!(
                "extern {:?} fn({}) -> {}",
                abi,
                params
                    .iter()
                    .map(|p| self.name(*p))
                    .collect::<Vec<_>>()
                    .join(", "),
                self.name(*result)
            ),
        }
    }

    pub fn is_integer(&self, ty: TypeId) -> bool {
        matches!(self.types[ty], Type::Int { .. } | Type::Usize | Type::Isize)
    }

    pub fn is_float(&self, ty: TypeId) -> bool {
        matches!(self.types[ty], Type::Float { .. })
    }

    pub fn is_scalar(&self, ty: TypeId) -> bool {
        matches!(
            self.types[ty],
            Type::Bool
                | Type::Int { .. }
                | Type::Usize
                | Type::Isize
                | Type::Float { .. }
                | Type::Pointer { .. }
                | Type::Function { .. }
        )
    }
}

impl Default for TypeContext {
    fn default() -> Self {
        Self::new()
    }
}

pub fn align_up(value: u64, align: u64) -> u64 {
    debug_assert!(align.is_power_of_two());
    (value + align - 1) & !(align - 1)
}

#[derive(Debug, Clone, Default)]
pub struct ItemAttrs {
    pub export: bool,
    pub no_mangle: bool,
    pub link_section: Option<String>,
    pub align: Option<u64>,
    pub used: bool,
    pub packed: bool,
    pub interrupt: bool,
    pub naked: bool,
    pub unsafe_linkage: bool,
}

#[derive(Debug, Clone)]
pub struct HirProgram {
    pub types: TypeContext,
    pub functions: Vec<HirFunction>,
    pub statics: Vec<HirStatic>,
    pub consts: Vec<HirConst>,
    pub symbols: HashMap<String, Symbol>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Symbol {
    Function(FunctionId),
    Static(StaticId),
    Const(ConstId),
    Type(TypeId),
}

#[derive(Debug, Clone)]
pub struct HirFunction {
    pub qualified_name: String,
    pub symbol: String,
    pub attrs: ItemAttrs,
    pub public: bool,
    pub unsafe_: bool,
    pub abi: Abi,
    pub params: Vec<LocalId>,
    pub result: TypeId,
    pub locals: Vec<HirLocal>,
    pub body: Option<HirBlock>,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub struct HirLocal {
    pub name: String,
    pub ty: TypeId,
    pub mutable: bool,
    pub parameter: bool,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub struct HirStatic {
    pub qualified_name: String,
    pub symbol: String,
    pub attrs: ItemAttrs,
    pub public: bool,
    pub mutable: bool,
    pub ty: TypeId,
    pub init: HirConstValue,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub struct HirConst {
    pub qualified_name: String,
    pub ty: TypeId,
    pub value: HirConstValue,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq)]
pub enum HirConstValue {
    Integer(u64),
    Bool(bool),
    Bytes(Vec<u8>),
    Array(Vec<HirConstValue>),
    Repeat(Box<HirConstValue>, u64),
    Struct(StructId, Vec<HirConstValue>),
    Enum(EnumId, usize, Vec<HirConstValue>),
    Zero,
}

#[derive(Debug, Clone)]
pub struct HirBlock {
    pub stmts: Vec<HirStmt>,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub struct HirStmt {
    pub kind: HirStmtKind,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub enum HirStmtKind {
    Let {
        local: LocalId,
        init: HirExpr,
    },
    Expr(HirExpr),
    If {
        condition: HirExpr,
        then_block: HirBlock,
        else_block: Option<HirBlock>,
    },
    While {
        condition: HirExpr,
        body: HirBlock,
    },
    Loop(HirBlock),
    Unsafe(HirBlock),
    Return(Option<HirExpr>),
    Break,
    Continue,
}

#[derive(Debug, Clone)]
pub struct HirExpr {
    pub kind: HirExprKind,
    pub ty: TypeId,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub enum HirExprKind {
    Constant(HirConstValue),
    Local(LocalId),
    Static(StaticId),
    Function(FunctionId),
    EnumVariant {
        enum_id: EnumId,
        variant: usize,
        args: Vec<HirExpr>,
    },
    Array(Vec<HirExpr>),
    ArrayRepeat {
        value: Box<HirExpr>,
        len: u64,
    },
    Struct {
        struct_id: StructId,
        fields: Vec<HirExpr>,
    },
    Unary {
        op: UnaryOp,
        operand: Box<HirExpr>,
    },
    Binary {
        op: BinaryOp,
        lhs: Box<HirExpr>,
        rhs: Box<HirExpr>,
    },
    Assign {
        op: Option<BinaryOp>,
        target: Box<HirExpr>,
        value: Box<HirExpr>,
    },
    Call {
        function: FunctionId,
        args: Vec<HirExpr>,
    },
    Intrinsic {
        intrinsic: Intrinsic,
        args: Vec<HirExpr>,
    },
    Field {
        base: Box<HirExpr>,
        field: usize,
        offset: u64,
    },
    Index {
        base: Box<HirExpr>,
        index: Box<HirExpr>,
    },
    Cast {
        value: Box<HirExpr>,
        to: TypeId,
    },
    Asm(HirAsm),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Intrinsic {
    VolatileLoad,
    VolatileStore,
    AtomicLoad,
    AtomicStore,
    AtomicExchange,
    AtomicCompareExchange,
    AtomicFetchAdd,
    In8,
    In16,
    In32,
    Out8,
    Out16,
    Out32,
    EnableInterrupts,
    DisableInterrupts,
    Halt,
    InvalidatePage,
    Syscall(u8),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemoryOrder {
    Relaxed = 0,
    Acquire = 1,
    Release = 2,
    AcqRel = 3,
    SeqCst = 4,
}

#[derive(Debug, Clone)]
pub struct HirAsm {
    pub template: String,
    pub operands: Vec<HirAsmOperand>,
    pub options: Vec<String>,
}

#[derive(Debug, Clone)]
pub enum HirAsmOperand {
    In {
        register: String,
        value: HirExpr,
    },
    Out {
        register: String,
        target: HirExpr,
    },
    InOut {
        register: String,
        input: HirExpr,
        output: HirExpr,
    },
}
