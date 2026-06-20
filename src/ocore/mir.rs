//! Typed SSA MIR for native O-core computation.
//!
//! Every `ValueId` has one defining instruction. Mutable source locals and
//! statics are explicit memory `Place`s; loading from them produces new SSA
//! values. This keeps machine computation separate from orchestration OIR.

use super::ast::{BinaryOp, UnaryOp};
use super::hir::*;
use super::{Diagnostic, Span};

pub type ValueId = usize;
pub type BlockId = usize;

#[derive(Debug, Clone)]
pub struct MirProgram {
    pub functions: Vec<MirFunction>,
    pub statics: Vec<MirStatic>,
}

#[derive(Debug, Clone)]
pub struct MirStatic {
    pub source: StaticId,
}

#[derive(Debug, Clone)]
pub struct MirFunction {
    pub source: FunctionId,
    pub local_types: Vec<TypeId>,
    pub values: Vec<TypeId>,
    pub blocks: Vec<BasicBlock>,
    pub entry: BlockId,
}

#[derive(Debug, Clone)]
pub struct BasicBlock {
    pub instructions: Vec<Instruction>,
    pub terminator: Terminator,
}

#[derive(Debug, Clone)]
pub enum Instruction {
    Const {
        dst: ValueId,
        value: u64,
    },
    FunctionAddress {
        dst: ValueId,
        function: FunctionId,
    },
    AddressOf {
        dst: ValueId,
        place: Place,
    },
    Load {
        dst: ValueId,
        place: Place,
        volatile: bool,
    },
    Store {
        place: Place,
        value: ValueId,
        volatile: bool,
    },
    Copy {
        destination: Place,
        source: Place,
        size: u64,
    },
    Unary {
        dst: ValueId,
        op: UnaryOp,
        operand: ValueId,
    },
    Binary {
        dst: ValueId,
        op: BinaryOp,
        lhs: ValueId,
        rhs: ValueId,
    },
    Cast {
        dst: ValueId,
        value: ValueId,
        from: TypeId,
        to: TypeId,
    },
    Call {
        dst: Option<ValueId>,
        function: FunctionId,
        args: Vec<ValueId>,
    },
    Intrinsic {
        dst: Option<ValueId>,
        intrinsic: Intrinsic,
        args: Vec<ValueId>,
    },
    Phi {
        dst: ValueId,
        incoming: Vec<(BlockId, ValueId)>,
    },
    Asm {
        template: String,
        operands: Vec<MirAsmOperand>,
        options: Vec<String>,
    },
}

#[derive(Debug, Clone)]
pub enum MirAsmOperand {
    In {
        register: String,
        value: ValueId,
    },
    Out {
        register: String,
        target: Place,
    },
    InOut {
        register: String,
        input: ValueId,
        output: Place,
    },
}

#[derive(Debug, Clone)]
pub struct Place {
    pub base: PlaceBase,
    pub projections: Vec<Projection>,
    pub ty: TypeId,
}

#[derive(Debug, Clone)]
pub enum PlaceBase {
    Local(LocalId),
    Static(StaticId),
    Pointer(ValueId),
}

#[derive(Debug, Clone)]
pub enum Projection {
    Field { offset: u64 },
    Index { index: ValueId, element_size: u64 },
}

#[derive(Debug, Clone)]
pub enum Terminator {
    Pending,
    Unreachable,
    Return(Option<ValueId>),
    Jump(BlockId),
    Branch {
        condition: ValueId,
        then_block: BlockId,
        else_block: BlockId,
    },
}

pub fn lower(program: &HirProgram) -> Result<MirProgram, Diagnostic> {
    let mut functions = Vec::new();
    for (source, function) in program.functions.iter().enumerate() {
        if function.body.is_some() {
            functions.push(FunctionBuilder::new(program, source).lower()?);
        }
    }
    Ok(MirProgram {
        functions,
        statics: program
            .statics
            .iter()
            .enumerate()
            .map(|(source, _)| MirStatic { source })
            .collect(),
    })
}

struct FunctionBuilder<'a> {
    program: &'a HirProgram,
    source: FunctionId,
    function: &'a HirFunction,
    values: Vec<TypeId>,
    blocks: Vec<BasicBlock>,
    current: BlockId,
    loops: Vec<(BlockId, BlockId)>, // break, continue
}

impl<'a> FunctionBuilder<'a> {
    fn new(program: &'a HirProgram, source: FunctionId) -> Self {
        Self {
            program,
            source,
            function: &program.functions[source],
            values: Vec::new(),
            blocks: vec![BasicBlock {
                instructions: Vec::new(),
                terminator: Terminator::Pending,
            }],
            current: 0,
            loops: Vec::new(),
        }
    }

    fn lower(mut self) -> Result<MirFunction, Diagnostic> {
        let body = self.function.body.as_ref().unwrap();
        self.lower_block(body)?;
        if !self.terminated(self.current) {
            let void = self.program.types.primitive("void").unwrap();
            if self.function.result == void {
                self.terminate(Terminator::Return(None));
            } else {
                self.terminate(Terminator::Unreachable);
            }
        }
        Ok(MirFunction {
            source: self.source,
            local_types: self.function.locals.iter().map(|l| l.ty).collect(),
            values: self.values,
            blocks: self.blocks,
            entry: 0,
        })
    }

    fn lower_block(&mut self, block: &HirBlock) -> Result<(), Diagnostic> {
        for stmt in &block.stmts {
            if self.terminated(self.current) {
                break;
            }
            self.lower_stmt(stmt)?;
        }
        Ok(())
    }

    fn lower_stmt(&mut self, stmt: &HirStmt) -> Result<(), Diagnostic> {
        match &stmt.kind {
            HirStmtKind::Let { local, init } => {
                let place = Place {
                    base: PlaceBase::Local(*local),
                    projections: Vec::new(),
                    ty: self.function.locals[*local].ty,
                };
                self.lower_initializer(place, init)?;
            }
            HirStmtKind::Expr(expr) => {
                let _ = self.lower_expr(expr)?;
            }
            HirStmtKind::Unsafe(block) => self.lower_block(block)?,
            HirStmtKind::Return(value) => {
                let value = value
                    .as_ref()
                    .map(|value| self.lower_expr(value))
                    .transpose()?;
                self.terminate(Terminator::Return(value));
            }
            HirStmtKind::If {
                condition,
                then_block,
                else_block,
            } => {
                let condition = self.lower_expr(condition)?;
                let then_id = self.new_block();
                let else_id = self.new_block();
                let join_id = self.new_block();
                self.terminate(Terminator::Branch {
                    condition,
                    then_block: then_id,
                    else_block: else_id,
                });

                self.current = then_id;
                self.lower_block(then_block)?;
                if !self.terminated(self.current) {
                    self.terminate(Terminator::Jump(join_id));
                }

                self.current = else_id;
                if let Some(else_block) = else_block {
                    self.lower_block(else_block)?;
                }
                if !self.terminated(self.current) {
                    self.terminate(Terminator::Jump(join_id));
                }
                self.current = join_id;
            }
            HirStmtKind::While { condition, body } => {
                let condition_block = self.new_block();
                let body_block = self.new_block();
                let exit_block = self.new_block();
                self.terminate(Terminator::Jump(condition_block));
                self.current = condition_block;
                let condition = self.lower_expr(condition)?;
                self.terminate(Terminator::Branch {
                    condition,
                    then_block: body_block,
                    else_block: exit_block,
                });
                self.loops.push((exit_block, condition_block));
                self.current = body_block;
                self.lower_block(body)?;
                self.loops.pop();
                if !self.terminated(self.current) {
                    self.terminate(Terminator::Jump(condition_block));
                }
                self.current = exit_block;
            }
            HirStmtKind::Loop(body) => {
                let body_block = self.new_block();
                let exit_block = self.new_block();
                self.terminate(Terminator::Jump(body_block));
                self.loops.push((exit_block, body_block));
                self.current = body_block;
                self.lower_block(body)?;
                self.loops.pop();
                if !self.terminated(self.current) {
                    self.terminate(Terminator::Jump(body_block));
                }
                self.current = exit_block;
            }
            HirStmtKind::Break => {
                let (target, _) = self.loops.last().copied().unwrap();
                self.terminate(Terminator::Jump(target));
            }
            HirStmtKind::Continue => {
                let (_, target) = self.loops.last().copied().unwrap();
                self.terminate(Terminator::Jump(target));
            }
        }
        Ok(())
    }

    fn lower_initializer(&mut self, place: Place, expr: &HirExpr) -> Result<(), Diagnostic> {
        match &expr.kind {
            HirExprKind::Struct { struct_id, fields } => {
                let def = &self.program.types.structs[*struct_id];
                for (field, value) in def.fields.iter().zip(fields) {
                    let mut field_place = place.clone();
                    field_place.projections.push(Projection::Field {
                        offset: field.offset,
                    });
                    field_place.ty = field.ty;
                    self.lower_initializer(field_place, value)?;
                }
            }
            HirExprKind::EnumVariant {
                enum_id,
                variant,
                args,
            } => {
                let def = &self.program.types.enums[*enum_id];
                let tag_ty = match def.tag_size {
                    1 => self.program.types.primitive("u8").unwrap(),
                    2 => self.program.types.primitive("u16").unwrap(),
                    4 => self.program.types.primitive("u32").unwrap(),
                    _ => unreachable!(),
                };
                let mut tag_place = place.clone();
                tag_place.ty = tag_ty;
                let tag = self.const_value(*variant as u64, tag_ty);
                self.emit(Instruction::Store {
                    place: tag_place,
                    value: tag,
                    volatile: false,
                });

                let payload_offset = def.payload_offset;
                let payload_types = def.variants[*variant].payload.clone();
                let mut offset = 0u64;
                for (ty, value) in payload_types.into_iter().zip(args) {
                    let layout = self.program.types.layout(ty);
                    offset = align_up(offset, layout.align);
                    let mut payload_place = place.clone();
                    payload_place.projections.push(Projection::Field {
                        offset: payload_offset + offset,
                    });
                    payload_place.ty = ty;
                    self.lower_initializer(payload_place, value)?;
                    offset += layout.size;
                }
            }
            HirExprKind::Array(values) => {
                let Type::Array { element, .. } = self.program.types.types[place.ty] else {
                    return Err(self.error(expr.span, "array initializer reached non-array place"));
                };
                let element_size = self.program.types.layout(element).size;
                for (index, value) in values.iter().enumerate() {
                    let index_value = self
                        .const_value(index as u64, self.program.types.primitive("usize").unwrap());
                    let mut element_place = place.clone();
                    element_place.projections.push(Projection::Index {
                        index: index_value,
                        element_size,
                    });
                    element_place.ty = element;
                    self.lower_initializer(element_place, value)?;
                }
            }
            HirExprKind::ArrayRepeat { value, len } => {
                let Type::Array { element, .. } = self.program.types.types[place.ty] else {
                    return Err(self.error(expr.span, "array repeat reached non-array place"));
                };
                let element_size = self.program.types.layout(element).size;
                for index in 0..*len {
                    let index_value =
                        self.const_value(index, self.program.types.primitive("usize").unwrap());
                    let mut element_place = place.clone();
                    element_place.projections.push(Projection::Index {
                        index: index_value,
                        element_size,
                    });
                    element_place.ty = element;
                    self.lower_initializer(element_place, value)?;
                }
            }
            HirExprKind::Constant(HirConstValue::Bytes(bytes)) => {
                let Type::Array { element, .. } = self.program.types.types[place.ty] else {
                    return Err(self.error(expr.span, "byte string reached non-array place"));
                };
                for (index, byte) in bytes.iter().enumerate() {
                    let index_value = self
                        .const_value(index as u64, self.program.types.primitive("usize").unwrap());
                    let byte_value = self.const_value(*byte as u64, element);
                    let mut element_place = place.clone();
                    element_place.projections.push(Projection::Index {
                        index: index_value,
                        element_size: 1,
                    });
                    element_place.ty = element;
                    self.emit(Instruction::Store {
                        place: element_place,
                        value: byte_value,
                        volatile: false,
                    });
                }
            }
            _ => {
                if !self.program.types.is_scalar(expr.ty) {
                    let source = self.lower_place(expr)?;
                    self.emit(Instruction::Copy {
                        destination: place,
                        source,
                        size: self.program.types.layout(expr.ty).size,
                    });
                    return Ok(());
                }
                let value = self.lower_expr(expr)?;
                self.emit(Instruction::Store {
                    place,
                    value,
                    volatile: false,
                });
            }
        }
        Ok(())
    }

    fn lower_expr(&mut self, expr: &HirExpr) -> Result<ValueId, Diagnostic> {
        match &expr.kind {
            HirExprKind::Constant(HirConstValue::Integer(value)) => {
                Ok(self.const_value(*value, expr.ty))
            }
            HirExprKind::Constant(HirConstValue::Bool(value)) => {
                Ok(self.const_value(u64::from(*value), expr.ty))
            }
            HirExprKind::Constant(_) => Err(self.error(
                expr.span,
                "aggregate constant is valid only as an initializer",
            )),
            HirExprKind::Local(_)
            | HirExprKind::Static(_)
            | HirExprKind::Field { .. }
            | HirExprKind::Index { .. } => {
                let place = self.lower_place(expr)?;
                let dst = self.new_value(expr.ty);
                self.emit(Instruction::Load {
                    dst,
                    place,
                    volatile: false,
                });
                Ok(dst)
            }
            HirExprKind::Function(function) => {
                let dst = self.new_value(expr.ty);
                self.emit(Instruction::FunctionAddress {
                    dst,
                    function: *function,
                });
                Ok(dst)
            }
            HirExprKind::EnumVariant { .. }
            | HirExprKind::Array(_)
            | HirExprKind::ArrayRepeat { .. }
            | HirExprKind::Struct { .. } => Err(self.error(
                expr.span,
                "aggregate value must initialize a local or static directly",
            )),
            HirExprKind::Unary { op, operand } => match op {
                UnaryOp::AddressOf { .. } => {
                    let place = self.lower_place(operand)?;
                    let dst = self.new_value(expr.ty);
                    self.emit(Instruction::AddressOf { dst, place });
                    Ok(dst)
                }
                UnaryOp::Deref => {
                    let place = self.lower_place(expr)?;
                    let dst = self.new_value(expr.ty);
                    self.emit(Instruction::Load {
                        dst,
                        place,
                        volatile: false,
                    });
                    Ok(dst)
                }
                _ => {
                    let operand = self.lower_expr(operand)?;
                    let dst = self.new_value(expr.ty);
                    self.emit(Instruction::Unary {
                        dst,
                        op: *op,
                        operand,
                    });
                    Ok(dst)
                }
            },
            HirExprKind::Binary { op, lhs, rhs }
                if matches!(op, BinaryOp::LogicalAnd | BinaryOp::LogicalOr) =>
            {
                self.lower_short_circuit(*op, lhs, rhs, expr.ty)
            }
            HirExprKind::Binary { op, lhs, rhs } => {
                let lhs = self.lower_expr(lhs)?;
                let rhs = self.lower_expr(rhs)?;
                let dst = self.new_value(expr.ty);
                self.emit(Instruction::Binary {
                    dst,
                    op: *op,
                    lhs,
                    rhs,
                });
                Ok(dst)
            }
            HirExprKind::Assign { op, target, value } => {
                let place = self.lower_place(target)?;
                if op.is_none() && !self.program.types.is_scalar(target.ty) {
                    let source = self.lower_place(value)?;
                    self.emit(Instruction::Copy {
                        destination: place,
                        source,
                        size: self.program.types.layout(target.ty).size,
                    });
                    return Ok(self.const_value(0, expr.ty));
                }
                let value = if let Some(op) = op {
                    let old = self.new_value(target.ty);
                    self.emit(Instruction::Load {
                        dst: old,
                        place: place.clone(),
                        volatile: false,
                    });
                    let rhs = self.lower_expr(value)?;
                    let combined = self.new_value(target.ty);
                    self.emit(Instruction::Binary {
                        dst: combined,
                        op: *op,
                        lhs: old,
                        rhs,
                    });
                    combined
                } else {
                    self.lower_expr(value)?
                };
                self.emit(Instruction::Store {
                    place,
                    value,
                    volatile: false,
                });
                Ok(self.const_value(0, expr.ty))
            }
            HirExprKind::Call { function, args } => {
                let args = args
                    .iter()
                    .map(|arg| self.lower_expr(arg))
                    .collect::<Result<Vec<_>, _>>()?;
                let void = self.program.types.primitive("void").unwrap();
                let never = self.program.types.primitive("never").unwrap();
                let dst = if expr.ty == void || expr.ty == never {
                    None
                } else {
                    Some(self.new_value(expr.ty))
                };
                self.emit(Instruction::Call {
                    dst,
                    function: *function,
                    args,
                });
                if expr.ty == never {
                    self.terminate(Terminator::Unreachable);
                }
                Ok(dst.unwrap_or_else(|| self.const_value(0, expr.ty)))
            }
            HirExprKind::Intrinsic { intrinsic, args } => {
                if *intrinsic == Intrinsic::VolatileLoad {
                    let pointer = self.lower_expr(&args[0])?;
                    let place = Place {
                        base: PlaceBase::Pointer(pointer),
                        projections: Vec::new(),
                        ty: expr.ty,
                    };
                    let dst = self.new_value(expr.ty);
                    self.emit(Instruction::Load {
                        dst,
                        place,
                        volatile: true,
                    });
                    return Ok(dst);
                }
                if *intrinsic == Intrinsic::VolatileStore {
                    let pointer = self.lower_expr(&args[0])?;
                    let value = self.lower_expr(&args[1])?;
                    self.emit(Instruction::Store {
                        place: Place {
                            base: PlaceBase::Pointer(pointer),
                            projections: Vec::new(),
                            ty: args[1].ty,
                        },
                        value,
                        volatile: true,
                    });
                    return Ok(self.const_value(0, expr.ty));
                }
                let args = args
                    .iter()
                    .map(|arg| self.lower_expr(arg))
                    .collect::<Result<Vec<_>, _>>()?;
                let void = self.program.types.primitive("void").unwrap();
                let dst = if expr.ty == void {
                    None
                } else {
                    Some(self.new_value(expr.ty))
                };
                self.emit(Instruction::Intrinsic {
                    dst,
                    intrinsic: *intrinsic,
                    args,
                });
                Ok(dst.unwrap_or_else(|| self.const_value(0, expr.ty)))
            }
            HirExprKind::Cast { value, to } => {
                let from = value.ty;
                let value = self.lower_expr(value)?;
                let dst = self.new_value(*to);
                self.emit(Instruction::Cast {
                    dst,
                    value,
                    from,
                    to: *to,
                });
                Ok(dst)
            }
            HirExprKind::Asm(asm) => {
                let mut operands = Vec::new();
                for operand in &asm.operands {
                    operands.push(match operand {
                        HirAsmOperand::In { register, value } => MirAsmOperand::In {
                            register: register.clone(),
                            value: self.lower_expr(value)?,
                        },
                        HirAsmOperand::Out { register, target } => MirAsmOperand::Out {
                            register: register.clone(),
                            target: self.lower_place(target)?,
                        },
                        HirAsmOperand::InOut {
                            register,
                            input,
                            output,
                        } => MirAsmOperand::InOut {
                            register: register.clone(),
                            input: self.lower_expr(input)?,
                            output: self.lower_place(output)?,
                        },
                    });
                }
                self.emit(Instruction::Asm {
                    template: asm.template.clone(),
                    operands,
                    options: asm.options.clone(),
                });
                Ok(self.const_value(0, expr.ty))
            }
        }
    }

    fn lower_short_circuit(
        &mut self,
        op: BinaryOp,
        lhs: &HirExpr,
        rhs: &HirExpr,
        ty: TypeId,
    ) -> Result<ValueId, Diagnostic> {
        let lhs = self.lower_expr(lhs)?;
        let rhs_block = self.new_block();
        let constant_block = self.new_block();
        let join_block = self.new_block();
        let (then_block, else_block, constant) = if op == BinaryOp::LogicalAnd {
            (rhs_block, constant_block, 0)
        } else {
            (constant_block, rhs_block, 1)
        };
        self.terminate(Terminator::Branch {
            condition: lhs,
            then_block,
            else_block,
        });

        self.current = rhs_block;
        let rhs = self.lower_expr(rhs)?;
        let rhs_predecessor = self.current;
        self.terminate(Terminator::Jump(join_block));

        self.current = constant_block;
        let constant = self.const_value(constant, ty);
        let constant_predecessor = self.current;
        self.terminate(Terminator::Jump(join_block));

        self.current = join_block;
        let dst = self.new_value(ty);
        self.emit(Instruction::Phi {
            dst,
            incoming: vec![(rhs_predecessor, rhs), (constant_predecessor, constant)],
        });
        Ok(dst)
    }

    fn lower_place(&mut self, expr: &HirExpr) -> Result<Place, Diagnostic> {
        match &expr.kind {
            HirExprKind::Local(local) => Ok(Place {
                base: PlaceBase::Local(*local),
                projections: Vec::new(),
                ty: expr.ty,
            }),
            HirExprKind::Static(id) => Ok(Place {
                base: PlaceBase::Static(*id),
                projections: Vec::new(),
                ty: expr.ty,
            }),
            HirExprKind::Unary {
                op: UnaryOp::Deref,
                operand,
            } => Ok(Place {
                base: PlaceBase::Pointer(self.lower_expr(operand)?),
                projections: Vec::new(),
                ty: expr.ty,
            }),
            HirExprKind::Field { base, offset, .. } => {
                let mut place = self.lower_place(base)?;
                place
                    .projections
                    .push(Projection::Field { offset: *offset });
                place.ty = expr.ty;
                Ok(place)
            }
            HirExprKind::Index { base, index } => {
                let index = self.lower_expr(index)?;
                let element_size = self.program.types.layout(expr.ty).size;
                let mut place = match self.program.types.types[base.ty] {
                    Type::Pointer { .. } => Place {
                        base: PlaceBase::Pointer(self.lower_expr(base)?),
                        projections: Vec::new(),
                        ty: expr.ty,
                    },
                    Type::Array { .. } => self.lower_place(base)?,
                    _ => return Err(self.error(expr.span, "invalid indexed place")),
                };
                place.projections.push(Projection::Index {
                    index,
                    element_size,
                });
                place.ty = expr.ty;
                Ok(place)
            }
            _ => Err(self.error(expr.span, "expression is not a MIR place")),
        }
    }

    fn const_value(&mut self, value: u64, ty: TypeId) -> ValueId {
        let dst = self.new_value(ty);
        self.emit(Instruction::Const { dst, value });
        dst
    }

    fn new_value(&mut self, ty: TypeId) -> ValueId {
        let id = self.values.len();
        self.values.push(ty);
        id
    }

    fn new_block(&mut self) -> BlockId {
        let id = self.blocks.len();
        self.blocks.push(BasicBlock {
            instructions: Vec::new(),
            terminator: Terminator::Pending,
        });
        id
    }

    fn emit(&mut self, instruction: Instruction) {
        debug_assert!(!self.terminated(self.current));
        self.blocks[self.current].instructions.push(instruction);
    }

    fn terminate(&mut self, terminator: Terminator) {
        self.blocks[self.current].terminator = terminator;
    }

    fn terminated(&self, block: BlockId) -> bool {
        !matches!(self.blocks[block].terminator, Terminator::Pending)
    }

    fn error(&self, span: Span, message: impl Into<String>) -> Diagnostic {
        Diagnostic {
            file: self.function.qualified_name.clone(),
            span,
            message: message.into(),
        }
    }
}

impl MirProgram {
    pub fn to_text(&self, hir: &HirProgram) -> String {
        let mut out = String::new();
        out.push_str("; O-core typed SSA MIR\n");
        for function in &self.functions {
            let source = &hir.functions[function.source];
            out.push_str(&format!(
                "fn {} [{}]\n",
                source.qualified_name, source.symbol
            ));
            for (block_id, block) in function.blocks.iter().enumerate() {
                out.push_str(&format!("  bb{block_id}:\n"));
                for instruction in &block.instructions {
                    out.push_str(&format!("    {}\n", format_instruction(instruction)));
                }
                out.push_str(&format!("    {}\n", format_terminator(&block.terminator)));
            }
        }
        out
    }
}

fn format_instruction(instruction: &Instruction) -> String {
    match instruction {
        Instruction::Const { dst, value } => format!("%{dst} = const {value}"),
        Instruction::FunctionAddress { dst, function } => {
            format!("%{dst} = function_address fn{function}")
        }
        Instruction::AddressOf { dst, place } => format!("%{dst} = addr {place:?}"),
        Instruction::Load {
            dst,
            place,
            volatile,
        } => format!(
            "%{dst} = {}load {place:?}",
            if *volatile { "volatile " } else { "" }
        ),
        Instruction::Store {
            place,
            value,
            volatile,
        } => format!(
            "{}store %{value}, {place:?}",
            if *volatile { "volatile " } else { "" }
        ),
        Instruction::Copy {
            destination,
            source,
            size,
        } => format!("copy {size} bytes {source:?} -> {destination:?}"),
        Instruction::Unary { dst, op, operand } => format!("%{dst} = {op:?} %{operand}"),
        Instruction::Binary { dst, op, lhs, rhs } => format!("%{dst} = {op:?} %{lhs}, %{rhs}"),
        Instruction::Cast { dst, value, to, .. } => format!("%{dst} = cast %{value} to t{to}"),
        Instruction::Call {
            dst,
            function,
            args,
        } => format!(
            "{}call fn{function}({})",
            dst.map(|d| format!("%{d} = ")).unwrap_or_default(),
            args.iter()
                .map(|a| format!("%{a}"))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        Instruction::Intrinsic {
            dst,
            intrinsic,
            args,
        } => format!(
            "{}intrinsic {intrinsic:?}({})",
            dst.map(|d| format!("%{d} = ")).unwrap_or_default(),
            args.iter()
                .map(|a| format!("%{a}"))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        Instruction::Phi { dst, incoming } => format!("%{dst} = phi {incoming:?}"),
        Instruction::Asm { template, .. } => format!("asm {template:?}"),
    }
}

fn format_terminator(terminator: &Terminator) -> String {
    match terminator {
        Terminator::Pending => "<missing terminator>".into(),
        Terminator::Unreachable => "unreachable".into(),
        Terminator::Return(None) => "return".into(),
        Terminator::Return(Some(value)) => format!("return %{value}"),
        Terminator::Jump(block) => format!("jump bb{block}"),
        Terminator::Branch {
            condition,
            then_block,
            else_block,
        } => format!("branch %{condition}, bb{then_block}, bb{else_block}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ocore::{parser, typeck};

    #[test]
    fn lowering_produces_ssa_and_control_flow() {
        let ast = parser::parse(
            "test.oc",
            r#"
module mir;
fn max(a: u64, b: u64) -> u64 {
    if a > b { return a; } else { return b; }
}
"#,
        )
        .unwrap();
        let hir = typeck::check(&[("test.oc".into(), ast)]).unwrap();
        let mir = lower(&hir).unwrap();
        assert_eq!(mir.functions.len(), 1);
        assert!(mir.functions[0].blocks.len() >= 4);
        let text = mir.to_text(&hir);
        assert!(text.contains("branch"));
        assert!(text.contains("return"));
    }
}
