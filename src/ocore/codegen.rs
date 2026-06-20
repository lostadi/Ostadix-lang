//! x86_64 freestanding assembly generation from typed SSA MIR.

use std::collections::HashMap;
use std::fmt::Write as _;

use super::ast::{Abi, BinaryOp, UnaryOp};
use super::hir::*;
use super::mir::*;
use super::{Diagnostic, Span};

pub fn emit_assembly(hir: &HirProgram, mir: &MirProgram) -> Result<String, Diagnostic> {
    let mut out = String::new();
    out.push_str(".intel_syntax noprefix\n");
    out.push_str(".file \"ocore\"\n");
    emit_statics(hir, &mut out)?;
    for function in &mir.functions {
        FunctionCodegen::new(hir, function).emit(&mut out)?;
    }
    out.push_str(".section .note.GNU-stack,\"\",@progbits\n");
    Ok(out)
}

fn emit_statics(hir: &HirProgram, out: &mut String) -> Result<(), Diagnostic> {
    for static_ in &hir.statics {
        let section = static_.attrs.link_section.as_deref().unwrap_or({
            if static_.mutable {
                ".data"
            } else {
                ".rodata"
            }
        });
        writeln!(out, ".section {section}").unwrap();
        let align = static_
            .attrs
            .align
            .unwrap_or_else(|| hir.types.layout(static_.ty).align);
        writeln!(out, ".balign {align}").unwrap();
        if static_.attrs.export || static_.public {
            writeln!(out, ".globl {}", static_.symbol).unwrap();
        } else {
            writeln!(out, ".local {}", static_.symbol).unwrap();
        }
        writeln!(out, ".type {}, @object", static_.symbol).unwrap();
        writeln!(out, "{}:", static_.symbol).unwrap();
        emit_const(hir, static_.ty, &static_.init, out)?;
        writeln!(out, ".size {}, .-{}", static_.symbol, static_.symbol).unwrap();
    }
    Ok(())
}

fn emit_const(
    hir: &HirProgram,
    ty: TypeId,
    value: &HirConstValue,
    out: &mut String,
) -> Result<(), Diagnostic> {
    match (value, &hir.types.types[ty]) {
        (HirConstValue::Integer(value), _) => {
            emit_integer(hir.types.layout(ty).size, *value, out)?;
        }
        (HirConstValue::Bool(value), _) => {
            emit_integer(hir.types.layout(ty).size, u64::from(*value), out)?;
        }
        (HirConstValue::Bytes(bytes), Type::Array { .. }) => {
            if bytes.is_empty() {
                out.push_str(".zero 0\n");
            } else {
                out.push_str(".byte ");
                for (index, byte) in bytes.iter().enumerate() {
                    if index != 0 {
                        out.push_str(", ");
                    }
                    write!(out, "{byte}").unwrap();
                }
                out.push('\n');
            }
        }
        (HirConstValue::Array(values), Type::Array { element, .. }) => {
            for value in values {
                emit_const(hir, *element, value, out)?;
            }
        }
        (HirConstValue::Repeat(value, count), Type::Array { element, .. }) => {
            for _ in 0..*count {
                emit_const(hir, *element, value, out)?;
            }
        }
        (HirConstValue::Struct(id, values), Type::Struct(expected)) if id == expected => {
            let def = &hir.types.structs[*id];
            let mut cursor = 0u64;
            for (field, value) in def.fields.iter().zip(values) {
                if field.offset > cursor {
                    writeln!(out, ".zero {}", field.offset - cursor).unwrap();
                }
                emit_const(hir, field.ty, value, out)?;
                cursor = field.offset + hir.types.layout(field.ty).size;
            }
            if def.layout.size > cursor {
                writeln!(out, ".zero {}", def.layout.size - cursor).unwrap();
            }
        }
        (HirConstValue::Enum(id, variant, payload), Type::Enum(expected)) if id == expected => {
            let def = &hir.types.enums[*id];
            emit_integer(def.tag_size, *variant as u64, out)?;
            if def.payload_offset > def.tag_size {
                writeln!(out, ".zero {}", def.payload_offset - def.tag_size).unwrap();
            }
            let variant_def = &def.variants[*variant];
            let mut cursor = 0u64;
            for (ty, value) in variant_def.payload.iter().zip(payload) {
                let layout = hir.types.layout(*ty);
                let next = align_up(cursor, layout.align);
                if next > cursor {
                    writeln!(out, ".zero {}", next - cursor).unwrap();
                }
                emit_const(hir, *ty, value, out)?;
                cursor = next + layout.size;
            }
            let used = def.payload_offset + cursor;
            if def.layout.size > used {
                writeln!(out, ".zero {}", def.layout.size - used).unwrap();
            }
        }
        (HirConstValue::Zero, _) => {
            writeln!(out, ".zero {}", hir.types.layout(ty).size).unwrap();
        }
        _ => return Err(codegen_error("constant/type mismatch during data emission")),
    }
    Ok(())
}

fn emit_integer(size: u64, value: u64, out: &mut String) -> Result<(), Diagnostic> {
    match size {
        1 => writeln!(out, ".byte {value}").unwrap(),
        2 => writeln!(out, ".word {value}").unwrap(),
        4 => writeln!(out, ".long {value}").unwrap(),
        8 => writeln!(out, ".quad {value}").unwrap(),
        _ => return Err(codegen_error(format!("cannot emit {size}-byte scalar"))),
    }
    Ok(())
}

struct FrameLayout {
    locals: Vec<u64>,
    values: Vec<u64>,
    size: u64,
}

impl FrameLayout {
    fn new(types: &TypeContext, function: &MirFunction) -> Self {
        let mut cursor = 0u64;
        let mut locals = Vec::new();
        for ty in &function.local_types {
            let layout = types.layout(*ty);
            cursor = align_up(cursor, layout.align.max(1));
            cursor += layout.size.max(1);
            locals.push(cursor);
        }
        let mut values = Vec::new();
        for _ in &function.values {
            cursor = align_up(cursor, 8);
            cursor += 8;
            values.push(cursor);
        }
        Self {
            locals,
            values,
            size: align_up(cursor, 16),
        }
    }
}

struct FunctionCodegen<'a> {
    hir: &'a HirProgram,
    mir: &'a MirFunction,
    source: &'a HirFunction,
    frame: FrameLayout,
    label_prefix: String,
}

impl<'a> FunctionCodegen<'a> {
    fn new(hir: &'a HirProgram, mir: &'a MirFunction) -> Self {
        let source = &hir.functions[mir.source];
        Self {
            hir,
            mir,
            source,
            frame: FrameLayout::new(&hir.types, mir),
            label_prefix: format!(".L_ocore_{}", mir.source),
        }
    }

    fn emit(&self, out: &mut String) -> Result<(), Diagnostic> {
        let section = self.source.attrs.link_section.as_deref().unwrap_or(".text");
        writeln!(out, ".section {section},\"ax\",@progbits").unwrap();
        let align = self.source.attrs.align.unwrap_or(16);
        writeln!(out, ".balign {align}").unwrap();
        if self.source.attrs.export || self.source.public {
            writeln!(out, ".globl {}", self.source.symbol).unwrap();
        } else {
            writeln!(out, ".local {}", self.source.symbol).unwrap();
        }
        writeln!(out, ".type {}, @function", self.source.symbol).unwrap();
        writeln!(out, "{}:", self.source.symbol).unwrap();

        if self.source.attrs.naked {
            self.emit_naked(out)?;
            writeln!(
                out,
                ".size {}, .-{}",
                self.source.symbol, self.source.symbol
            )
            .unwrap();
            return Ok(());
        }

        if self.source.abi == Abi::Interrupt {
            self.emit_interrupt_prologue(out);
        } else {
            out.push_str("  push rbp\n  mov rbp, rsp\n");
        }
        let stack_size = self.stack_size();
        if stack_size != 0 {
            writeln!(out, "  sub rsp, {stack_size}").unwrap();
        }
        self.store_parameters(out)?;
        writeln!(out, "  jmp {}_bb{}", self.label_prefix, self.mir.entry).unwrap();

        for (block_id, block) in self.mir.blocks.iter().enumerate() {
            writeln!(out, "{}_bb{}:", self.label_prefix, block_id).unwrap();
            for instruction in &block.instructions {
                if !matches!(instruction, Instruction::Phi { .. }) {
                    self.emit_instruction(instruction, out)?;
                }
            }
            self.emit_terminator(block_id, &block.terminator, out)?;
        }
        writeln!(
            out,
            ".size {}, .-{}",
            self.source.symbol, self.source.symbol
        )
        .unwrap();
        Ok(())
    }

    fn emit_naked(&self, out: &mut String) -> Result<(), Diagnostic> {
        for block in &self.mir.blocks {
            for instruction in &block.instructions {
                match instruction {
                    Instruction::Asm {
                        template, operands, ..
                    } if operands.is_empty() => {
                        for line in template.lines() {
                            writeln!(out, "  {line}").unwrap();
                        }
                    }
                    Instruction::Const { .. } => {}
                    _ => {
                        return Err(codegen_error(
                            "@naked function bodies may contain only operand-free asm!",
                        ))
                    }
                }
            }
        }
        Ok(())
    }

    fn emit_interrupt_prologue(&self, out: &mut String) {
        out.push_str(
            "  push rax\n  push rcx\n  push rdx\n  push rsi\n  push rdi\n  push r8\n  push r9\n  push r10\n  push r11\n  push rbp\n  mov rbp, rsp\n",
        );
    }

    fn store_parameters(&self, out: &mut String) -> Result<(), Diagnostic> {
        let registers = ["rdi", "rsi", "rdx", "rcx", "r8", "r9"];
        for (index, local) in self.source.params.iter().enumerate() {
            let ty = self.source.locals[*local].ty;
            if !self.hir.types.is_scalar(ty) {
                return Err(codegen_error("aggregate parameters are not implemented"));
            }
            let offset = self.frame.locals[*local];
            if index < registers.len() {
                self.store_reg_to_frame(registers[index], offset, ty, out)?;
            } else {
                let caller_offset = 16 + (index - registers.len()) as u64 * 8;
                writeln!(out, "  mov rax, qword ptr [rbp+{caller_offset}]").unwrap();
                self.store_reg_to_frame("rax", offset, ty, out)?;
            }
        }
        Ok(())
    }

    fn emit_instruction(
        &self,
        instruction: &Instruction,
        out: &mut String,
    ) -> Result<(), Diagnostic> {
        match instruction {
            Instruction::Const { dst, value } => {
                writeln!(out, "  mov rax, {value}").unwrap();
                self.store_value(*dst, "rax", out);
            }
            Instruction::FunctionAddress { dst, function } => {
                writeln!(
                    out,
                    "  lea rax, [rip+{}]",
                    self.hir.functions[*function].symbol
                )
                .unwrap();
                self.store_value(*dst, "rax", out);
            }
            Instruction::AddressOf { dst, place } => {
                self.place_address(place, "r11", out)?;
                self.store_value(*dst, "r11", out);
            }
            Instruction::Load { dst, place, .. } => {
                self.place_address(place, "r11", out)?;
                self.load_memory("rax", "r11", place.ty, out)?;
                self.store_value(*dst, "rax", out);
            }
            Instruction::Store { place, value, .. } => {
                self.load_value(*value, "rax", out);
                self.place_address(place, "r11", out)?;
                self.store_memory("rax", "r11", place.ty, out)?;
            }
            Instruction::Copy {
                destination,
                source,
                size,
            } => {
                self.place_address(destination, "rdi", out)?;
                self.place_address(source, "rsi", out)?;
                writeln!(out, "  mov rcx, {size}").unwrap();
                out.push_str("  rep movsb\n");
            }
            Instruction::Unary { dst, op, operand } => {
                self.load_value(*operand, "rax", out);
                match op {
                    UnaryOp::Neg => out.push_str("  neg rax\n"),
                    UnaryOp::Not => out.push_str("  xor rax, 1\n"),
                    UnaryOp::BitNot => out.push_str("  not rax\n"),
                    UnaryOp::Deref | UnaryOp::AddressOf { .. } => {
                        return Err(codegen_error("place unary operation escaped MIR lowering"))
                    }
                }
                self.store_value(*dst, "rax", out);
            }
            Instruction::Binary { dst, op, lhs, rhs } => {
                self.emit_binary(*dst, *op, *lhs, *rhs, out)?;
            }
            Instruction::Cast {
                dst,
                value,
                from,
                to,
            } => {
                self.load_value(*value, "rax", out);
                self.normalize_scalar("rax", *from, out)?;
                self.normalize_scalar("rax", *to, out)?;
                self.store_value(*dst, "rax", out);
            }
            Instruction::Call {
                dst,
                function,
                args,
            } => self.emit_call(*dst, *function, args, out)?,
            Instruction::Intrinsic {
                dst,
                intrinsic,
                args,
            } => self.emit_intrinsic(*dst, *intrinsic, args, out)?,
            Instruction::Phi { .. } => unreachable!(),
            Instruction::Asm {
                template, operands, ..
            } => self.emit_asm(template, operands, out)?,
        }
        Ok(())
    }

    fn emit_binary(
        &self,
        dst: ValueId,
        op: BinaryOp,
        lhs: ValueId,
        rhs: ValueId,
        out: &mut String,
    ) -> Result<(), Diagnostic> {
        self.load_value(lhs, "rax", out);
        self.load_value(rhs, "rcx", out);
        let signed = is_signed(&self.hir.types.types[self.mir.values[lhs]]);
        match op {
            BinaryOp::Add | BinaryOp::Sub => {
                if let Type::Pointer { pointee, .. } = self.hir.types.types[self.mir.values[lhs]] {
                    let scale = self.hir.types.layout(pointee).size;
                    if scale != 1 {
                        writeln!(out, "  imul rcx, {scale}").unwrap();
                    }
                }
                writeln!(
                    out,
                    "  {} rax, rcx",
                    if op == BinaryOp::Add { "add" } else { "sub" }
                )
                .unwrap();
            }
            BinaryOp::Mul => out.push_str("  imul rax, rcx\n"),
            BinaryOp::Div | BinaryOp::Rem => {
                if signed {
                    out.push_str("  cqo\n  idiv rcx\n");
                } else {
                    out.push_str("  xor rdx, rdx\n  div rcx\n");
                }
                if op == BinaryOp::Rem {
                    out.push_str("  mov rax, rdx\n");
                }
            }
            BinaryOp::BitAnd => out.push_str("  and rax, rcx\n"),
            BinaryOp::BitOr => out.push_str("  or rax, rcx\n"),
            BinaryOp::BitXor => out.push_str("  xor rax, rcx\n"),
            BinaryOp::ShiftLeft => out.push_str("  shl rax, cl\n"),
            BinaryOp::ShiftRight => {
                out.push_str(if signed {
                    "  sar rax, cl\n"
                } else {
                    "  shr rax, cl\n"
                });
            }
            BinaryOp::Eq
            | BinaryOp::NotEq
            | BinaryOp::Less
            | BinaryOp::LessEq
            | BinaryOp::Greater
            | BinaryOp::GreaterEq => {
                out.push_str("  cmp rax, rcx\n");
                let cc = match (op, signed) {
                    (BinaryOp::Eq, _) => "e",
                    (BinaryOp::NotEq, _) => "ne",
                    (BinaryOp::Less, true) => "l",
                    (BinaryOp::LessEq, true) => "le",
                    (BinaryOp::Greater, true) => "g",
                    (BinaryOp::GreaterEq, true) => "ge",
                    (BinaryOp::Less, false) => "b",
                    (BinaryOp::LessEq, false) => "be",
                    (BinaryOp::Greater, false) => "a",
                    (BinaryOp::GreaterEq, false) => "ae",
                    _ => unreachable!(),
                };
                writeln!(out, "  set{cc} al\n  movzx rax, al").unwrap();
            }
            BinaryOp::LogicalAnd | BinaryOp::LogicalOr => {
                return Err(codegen_error(
                    "logical operator escaped short-circuit lowering",
                ))
            }
        }
        self.normalize_scalar("rax", self.mir.values[dst], out)?;
        self.store_value(dst, "rax", out);
        Ok(())
    }

    fn emit_call(
        &self,
        dst: Option<ValueId>,
        function: FunctionId,
        args: &[ValueId],
        out: &mut String,
    ) -> Result<(), Diagnostic> {
        let registers = ["rdi", "rsi", "rdx", "rcx", "r8", "r9"];
        let stack_count = args.len().saturating_sub(registers.len());
        let pad = stack_count % 2;
        if pad != 0 {
            out.push_str("  sub rsp, 8\n");
        }
        for value in args.iter().skip(registers.len()).rev() {
            self.load_value(*value, "rax", out);
            out.push_str("  push rax\n");
        }
        for (value, register) in args.iter().zip(registers) {
            self.load_value(*value, register, out);
        }
        writeln!(out, "  call {}", self.hir.functions[function].symbol).unwrap();
        if stack_count + pad != 0 {
            writeln!(out, "  add rsp, {}", (stack_count + pad) * 8).unwrap();
        }
        if let Some(dst) = dst {
            self.normalize_scalar("rax", self.mir.values[dst], out)?;
            self.store_value(dst, "rax", out);
        }
        Ok(())
    }

    fn emit_intrinsic(
        &self,
        dst: Option<ValueId>,
        intrinsic: Intrinsic,
        args: &[ValueId],
        out: &mut String,
    ) -> Result<(), Diagnostic> {
        match intrinsic {
            Intrinsic::In8 | Intrinsic::In16 | Intrinsic::In32 => {
                self.load_value(args[0], "rdx", out);
                out.push_str(match intrinsic {
                    Intrinsic::In8 => "  in al, dx\n  movzx rax, al\n",
                    Intrinsic::In16 => "  in ax, dx\n  movzx rax, ax\n",
                    _ => "  in eax, dx\n",
                });
            }
            Intrinsic::Out8 | Intrinsic::Out16 | Intrinsic::Out32 => {
                self.load_value(args[0], "rdx", out);
                self.load_value(args[1], "rax", out);
                out.push_str(match intrinsic {
                    Intrinsic::Out8 => "  out dx, al\n",
                    Intrinsic::Out16 => "  out dx, ax\n",
                    _ => "  out dx, eax\n",
                });
            }
            Intrinsic::EnableInterrupts => out.push_str("  sti\n"),
            Intrinsic::DisableInterrupts => out.push_str("  cli\n"),
            Intrinsic::Halt => out.push_str("  hlt\n"),
            Intrinsic::InvalidatePage => {
                self.load_value(args[0], "rax", out);
                out.push_str("  invlpg [rax]\n");
            }
            Intrinsic::Syscall(count) => {
                let regs = ["rdi", "rsi", "rdx", "r10", "r8", "r9"];
                self.load_value(args[0], "rax", out);
                for (value, reg) in args[1..].iter().zip(regs.iter().take(count as usize)) {
                    self.load_value(*value, reg, out);
                }
                out.push_str("  syscall\n");
            }
            Intrinsic::AtomicLoad => {
                self.load_value(args[0], "r11", out);
                let ty = self.mir.values[dst.unwrap()];
                self.load_memory("rax", "r11", ty, out)?;
            }
            Intrinsic::AtomicStore => {
                self.load_value(args[0], "r11", out);
                self.load_value(args[1], "rax", out);
                let pointee = pointer_pointee(&self.hir.types, self.mir.values[args[0]])?;
                let order = self.const_from_value(args[2]);
                if order == Some(MemoryOrder::SeqCst as u64) {
                    self.emit_xchg("rax", "r11", pointee, out)?;
                } else {
                    self.store_memory("rax", "r11", pointee, out)?;
                }
            }
            Intrinsic::AtomicExchange => {
                self.load_value(args[0], "r11", out);
                self.load_value(args[1], "rax", out);
                let ty = self.mir.values[dst.unwrap()];
                self.emit_xchg("rax", "r11", ty, out)?;
            }
            Intrinsic::AtomicFetchAdd => {
                self.load_value(args[0], "r11", out);
                self.load_value(args[1], "rax", out);
                let ty = self.mir.values[dst.unwrap()];
                let size = self.hir.types.layout(ty).size;
                writeln!(
                    out,
                    "  lock xadd {} ptr [r11], {}",
                    size_name(size)?,
                    subreg("rax", size)?
                )
                .unwrap();
            }
            Intrinsic::AtomicCompareExchange => {
                self.load_value(args[0], "r11", out);
                self.load_value(args[1], "rax", out);
                self.load_value(args[2], "rcx", out);
                let ty = self.mir.values[dst.unwrap()];
                let size = self.hir.types.layout(ty).size;
                writeln!(
                    out,
                    "  lock cmpxchg {} ptr [r11], {}",
                    size_name(size)?,
                    subreg("rcx", size)?
                )
                .unwrap();
            }
            Intrinsic::VolatileLoad | Intrinsic::VolatileStore => {
                return Err(codegen_error(
                    "volatile intrinsic escaped MIR memory lowering",
                ))
            }
        }
        if let Some(dst) = dst {
            self.store_value(dst, "rax", out);
        }
        Ok(())
    }

    fn emit_xchg(
        &self,
        reg: &str,
        address: &str,
        ty: TypeId,
        out: &mut String,
    ) -> Result<(), Diagnostic> {
        let size = self.hir.types.layout(ty).size;
        writeln!(
            out,
            "  xchg {} ptr [{address}], {}",
            size_name(size)?,
            subreg(reg, size)?
        )
        .unwrap();
        Ok(())
    }

    fn const_from_value(&self, value: ValueId) -> Option<u64> {
        self.mir
            .blocks
            .iter()
            .flat_map(|b| &b.instructions)
            .find_map(|i| {
                if let Instruction::Const { dst, value: v } = i {
                    (*dst == value).then_some(*v)
                } else {
                    None
                }
            })
    }

    fn emit_asm(
        &self,
        template: &str,
        operands: &[MirAsmOperand],
        out: &mut String,
    ) -> Result<(), Diagnostic> {
        let mut outputs = Vec::new();
        for operand in operands {
            match operand {
                MirAsmOperand::In { register, value } => {
                    validate_register(register)?;
                    self.load_value(*value, register, out);
                }
                MirAsmOperand::Out { register, target } => {
                    validate_register(register)?;
                    outputs.push((register.as_str(), target));
                }
                MirAsmOperand::InOut {
                    register,
                    input,
                    output,
                } => {
                    validate_register(register)?;
                    self.load_value(*input, register, out);
                    outputs.push((register.as_str(), output));
                }
            }
        }
        for line in template.lines() {
            writeln!(out, "  {line}").unwrap();
        }
        for (register, target) in outputs {
            let address = if register == "r11" { "r10" } else { "r11" };
            self.place_address(target, address, out)?;
            self.store_memory(register, address, target.ty, out)?;
        }
        Ok(())
    }

    fn emit_terminator(
        &self,
        block_id: BlockId,
        terminator: &Terminator,
        out: &mut String,
    ) -> Result<(), Diagnostic> {
        match terminator {
            Terminator::Pending => return Err(codegen_error("MIR block has no terminator")),
            Terminator::Unreachable => out.push_str("  ud2\n"),
            Terminator::Return(value) => {
                if let Some(value) = value {
                    self.load_value(*value, "rax", out);
                }
                self.emit_epilogue(out);
            }
            Terminator::Jump(target) => {
                self.emit_phi_moves(block_id, *target, out);
                writeln!(out, "  jmp {}_bb{target}", self.label_prefix).unwrap();
            }
            Terminator::Branch {
                condition,
                then_block,
                else_block,
            } => {
                let then_edge = format!("{}_bb{}_then_edge", self.label_prefix, block_id);
                self.load_value(*condition, "rax", out);
                out.push_str("  test rax, rax\n");
                writeln!(out, "  jne {then_edge}").unwrap();
                self.emit_phi_moves(block_id, *else_block, out);
                writeln!(out, "  jmp {}_bb{else_block}", self.label_prefix).unwrap();
                writeln!(out, "{then_edge}:").unwrap();
                self.emit_phi_moves(block_id, *then_block, out);
                writeln!(out, "  jmp {}_bb{then_block}", self.label_prefix).unwrap();
            }
        }
        Ok(())
    }

    fn emit_phi_moves(&self, from: BlockId, to: BlockId, out: &mut String) {
        for instruction in &self.mir.blocks[to].instructions {
            let Instruction::Phi { dst, incoming } = instruction else {
                continue;
            };
            if let Some((_, value)) = incoming.iter().find(|(block, _)| *block == from) {
                self.load_value(*value, "rax", out);
                self.store_value(*dst, "rax", out);
            }
        }
    }

    fn emit_epilogue(&self, out: &mut String) {
        let stack_size = self.stack_size();
        if stack_size != 0 {
            writeln!(out, "  add rsp, {stack_size}").unwrap();
        }
        if self.source.abi == Abi::Interrupt {
            out.push_str(
                "  pop rbp\n  pop r11\n  pop r10\n  pop r9\n  pop r8\n  pop rdi\n  pop rsi\n  pop rdx\n  pop rcx\n  pop rax\n  iretq\n",
            );
        } else {
            out.push_str("  pop rbp\n  ret\n");
        }
    }

    fn stack_size(&self) -> u64 {
        // A same-privilege x86_64 interrupt pushes RIP, CS, and RFLAGS (24
        // bytes). After saving nine volatile registers plus RBP, the stack is
        // eight bytes off the SysV call boundary. Reserve one padding slot so
        // interrupt handlers can safely call ordinary O-core functions.
        self.frame.size + u64::from(self.source.abi == Abi::Interrupt) * 8
    }

    fn place_address(
        &self,
        place: &Place,
        register: &str,
        out: &mut String,
    ) -> Result<(), Diagnostic> {
        match place.base {
            PlaceBase::Local(local) => {
                writeln!(out, "  lea {register}, [rbp-{}]", self.frame.locals[local]).unwrap();
            }
            PlaceBase::Static(static_id) => {
                writeln!(
                    out,
                    "  lea {register}, [rip+{}]",
                    self.hir.statics[static_id].symbol
                )
                .unwrap();
            }
            PlaceBase::Pointer(value) => self.load_value(value, register, out),
        }
        for projection in &place.projections {
            match projection {
                Projection::Field { offset } => {
                    if *offset != 0 {
                        writeln!(out, "  add {register}, {offset}").unwrap();
                    }
                }
                Projection::Index {
                    index,
                    element_size,
                } => {
                    let scratch = if register == "r10" { "r11" } else { "r10" };
                    self.load_value(*index, scratch, out);
                    if *element_size != 1 {
                        writeln!(out, "  imul {scratch}, {element_size}").unwrap();
                    }
                    writeln!(out, "  add {register}, {scratch}").unwrap();
                }
            }
        }
        Ok(())
    }

    fn load_value(&self, value: ValueId, register: &str, out: &mut String) {
        writeln!(
            out,
            "  mov {register}, qword ptr [rbp-{}]",
            self.frame.values[value]
        )
        .unwrap();
    }

    fn store_value(&self, value: ValueId, register: &str, out: &mut String) {
        writeln!(
            out,
            "  mov qword ptr [rbp-{}], {register}",
            self.frame.values[value]
        )
        .unwrap();
    }

    fn store_reg_to_frame(
        &self,
        register: &str,
        offset: u64,
        ty: TypeId,
        out: &mut String,
    ) -> Result<(), Diagnostic> {
        let size = self.hir.types.layout(ty).size;
        writeln!(
            out,
            "  mov {} ptr [rbp-{offset}], {}",
            size_name(size)?,
            subreg(register, size)?
        )
        .unwrap();
        Ok(())
    }

    fn load_memory(
        &self,
        dst: &str,
        address: &str,
        ty: TypeId,
        out: &mut String,
    ) -> Result<(), Diagnostic> {
        let size = self.hir.types.layout(ty).size;
        let signed = is_signed(&self.hir.types.types[ty]);
        match (size, signed) {
            (1, false) => writeln!(out, "  movzx {dst}, byte ptr [{address}]").unwrap(),
            (1, true) => writeln!(out, "  movsx {dst}, byte ptr [{address}]").unwrap(),
            (2, false) => writeln!(out, "  movzx {dst}, word ptr [{address}]").unwrap(),
            (2, true) => writeln!(out, "  movsx {dst}, word ptr [{address}]").unwrap(),
            (4, false) => {
                writeln!(out, "  mov {}, dword ptr [{address}]", subreg(dst, 4)?).unwrap()
            }
            (4, true) => writeln!(out, "  movsxd {dst}, dword ptr [{address}]").unwrap(),
            (8, _) => writeln!(out, "  mov {dst}, qword ptr [{address}]").unwrap(),
            _ => return Err(codegen_error(format!("cannot scalar-load {size} bytes"))),
        }
        Ok(())
    }

    fn store_memory(
        &self,
        src: &str,
        address: &str,
        ty: TypeId,
        out: &mut String,
    ) -> Result<(), Diagnostic> {
        let size = self.hir.types.layout(ty).size;
        writeln!(
            out,
            "  mov {} ptr [{address}], {}",
            size_name(size)?,
            subreg(src, size)?
        )
        .unwrap();
        Ok(())
    }

    fn normalize_scalar(
        &self,
        register: &str,
        ty: TypeId,
        out: &mut String,
    ) -> Result<(), Diagnostic> {
        let size = self.hir.types.layout(ty).size;
        let signed = is_signed(&self.hir.types.types[ty]);
        match (size, signed) {
            (1, false) => writeln!(out, "  movzx {register}, {}", subreg(register, 1)?).unwrap(),
            (1, true) => writeln!(out, "  movsx {register}, {}", subreg(register, 1)?).unwrap(),
            (2, false) => writeln!(out, "  movzx {register}, {}", subreg(register, 2)?).unwrap(),
            (2, true) => writeln!(out, "  movsx {register}, {}", subreg(register, 2)?).unwrap(),
            (4, false) => {
                let r32 = subreg(register, 4)?;
                writeln!(out, "  mov {r32}, {r32}").unwrap();
            }
            (4, true) => writeln!(out, "  movsxd {register}, {}", subreg(register, 4)?).unwrap(),
            (8, _) | (0, _) => {}
            _ => {
                return Err(codegen_error(format!(
                    "cannot normalize {size}-byte scalar"
                )))
            }
        }
        Ok(())
    }
}

fn pointer_pointee(types: &TypeContext, ty: TypeId) -> Result<TypeId, Diagnostic> {
    match types.types[ty] {
        Type::Pointer { pointee, .. } => Ok(pointee),
        _ => Err(codegen_error("expected pointer type")),
    }
}

fn is_signed(ty: &Type) -> bool {
    matches!(ty, Type::Int { signed: true, .. } | Type::Isize)
}

fn size_name(size: u64) -> Result<&'static str, Diagnostic> {
    match size {
        1 => Ok("byte"),
        2 => Ok("word"),
        4 => Ok("dword"),
        8 => Ok("qword"),
        _ => Err(codegen_error(format!("unsupported scalar size {size}"))),
    }
}

fn subreg(register: &str, size: u64) -> Result<&'static str, Diagnostic> {
    let table: HashMap<&str, [&str; 4]> = HashMap::from([
        ("rax", ["al", "ax", "eax", "rax"]),
        ("rbx", ["bl", "bx", "ebx", "rbx"]),
        ("rcx", ["cl", "cx", "ecx", "rcx"]),
        ("rdx", ["dl", "dx", "edx", "rdx"]),
        ("rsi", ["sil", "si", "esi", "rsi"]),
        ("rdi", ["dil", "di", "edi", "rdi"]),
        ("r8", ["r8b", "r8w", "r8d", "r8"]),
        ("r9", ["r9b", "r9w", "r9d", "r9"]),
        ("r10", ["r10b", "r10w", "r10d", "r10"]),
        ("r11", ["r11b", "r11w", "r11d", "r11"]),
    ]);
    let index = match size {
        1 => 0,
        2 => 1,
        4 => 2,
        8 => 3,
        _ => return Err(codegen_error(format!("unsupported register width {size}"))),
    };
    table
        .get(register)
        .map(|v| v[index])
        .ok_or_else(|| codegen_error(format!("unsupported register `{register}`")))
}

fn validate_register(register: &str) -> Result<(), Diagnostic> {
    const ALLOWED: &[&str] = &["rax", "rcx", "rdx", "rsi", "rdi", "r8", "r9", "r10", "r11"];
    if ALLOWED.contains(&register) {
        Ok(())
    } else {
        Err(codegen_error(format!(
            "unsupported asm register `{register}`"
        )))
    }
}

fn codegen_error(message: impl Into<String>) -> Diagnostic {
    Diagnostic {
        file: "<x86_64-codegen>".into(),
        span: Span::default(),
        message: message.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ocore::{mir, parser, typeck};

    #[test]
    fn emits_freestanding_x86_64_assembly() {
        let ast = parser::parse(
            "test.oc",
            r#"
module codegen;
@export @no_mangle
unsafe fn add_and_out(a: u64, b: u64) -> u64 {
    let value: u64 = a + b;
    unsafe { outb(0x3f8, value as u8); }
    return value;
}
"#,
        )
        .unwrap();
        let hir = typeck::check(&[("test.oc".into(), ast)]).unwrap();
        let mir = mir::lower(&hir).unwrap();
        let asm = emit_assembly(&hir, &mir).unwrap();
        assert!(asm.contains("add_and_out:"));
        assert!(asm.contains("out dx, al"));
        assert!(asm.contains("ret"));
    }

    #[test]
    fn emits_struct_copy_and_enum_initialization() {
        let ast = parser::parse(
            "test.oc",
            r#"
module aggregates;
struct Pair { left: u64, right: u64 }
enum Maybe { none, some(u64) }
fn use_aggregates() -> u64 {
    let first: Pair = Pair { left: 10, right: 20 };
    let mut second: Pair = Pair { left: 0, right: 0 };
    second = first;
    let state: Maybe = Maybe::some(7);
    return second.right;
}
"#,
        )
        .unwrap();
        let hir = typeck::check(&[("test.oc".into(), ast)]).unwrap();
        let mir = mir::lower(&hir).unwrap();
        let text = mir.to_text(&hir);
        assert!(text.contains("copy 16 bytes"));
        let asm = emit_assembly(&hir, &mir).unwrap();
        assert!(asm.contains("rep movsb"));
    }
}
