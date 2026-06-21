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
        (HirConstValue::Integer(value), Type::Int { .. } | Type::Usize | Type::Isize) => {
            emit_integer(hir.types.layout(ty).size, *value, out)?;
        }
        (HirConstValue::Bool(value), Type::Bool) => {
            emit_integer(hir.types.layout(ty).size, u64::from(*value), out)?;
        }
        (HirConstValue::Bytes(bytes), Type::Array { element, len })
            if bytes.len() as u64 == *len
                && matches!(
                    hir.types.types[*element],
                    Type::Int {
                        signed: false,
                        bits: 8
                    }
                ) =>
        {
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
        (HirConstValue::Array(values), Type::Array { element, len })
            if values.len() as u64 == *len =>
        {
            for value in values {
                emit_const(hir, *element, value, out)?;
            }
        }
        (HirConstValue::Repeat(value, count), Type::Array { element, len }) if count == len => {
            for _ in 0..*count {
                emit_const(hir, *element, value, out)?;
            }
        }
        (HirConstValue::Struct(id, values), Type::Struct(expected))
            if id == expected && values.len() == hir.types.structs[*id].fields.len() =>
        {
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
        (HirConstValue::Enum(id, variant, payload), Type::Enum(expected))
            if id == expected
                && hir.types.enums[*id]
                    .variants
                    .get(*variant)
                    .is_some_and(|definition| definition.payload.len() == payload.len()) =>
        {
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
            if self.source.abi == Abi::SysV64 && self.hir.types.is_float(ty) {
                return Err(codegen_error(
                    "floating-point parameter escaped sysv64 ABI checking",
                ));
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
                if self.hir.types.is_float(self.mir.values[*dst]) {
                    return Err(codegen_error(
                        "floating-point constant escaped O-core type checking",
                    ));
                }
                writeln!(out, "  mov rax, {value}").unwrap();
                self.store_value(*dst, "rax", out);
            }
            Instruction::FunctionAddress { dst, function } => {
                self.validate_function_address(*dst, *function)?;
                writeln!(
                    out,
                    "  lea rax, [rip+{}]",
                    self.hir.functions[*function].symbol
                )
                .unwrap();
                self.store_value(*dst, "rax", out);
            }
            Instruction::AddressOf { dst, place } => {
                let Type::Pointer { pointee, .. } = self.hir.types.types[self.mir.values[*dst]]
                else {
                    return Err(codegen_error("address result is not a pointer"));
                };
                if pointee != place.ty {
                    return Err(codegen_error("address result pointee type mismatch"));
                }
                self.place_address(place, "r11", out)?;
                self.store_value(*dst, "r11", out);
            }
            Instruction::Load { dst, place, .. } => {
                self.require_same_type(self.mir.values[*dst], place.ty, "load result")?;
                self.require_scalar_type(place.ty, "load")?;
                self.place_address(place, "r11", out)?;
                self.load_memory("rax", "r11", place.ty, out)?;
                self.store_value(*dst, "rax", out);
            }
            Instruction::Store { place, value, .. } => {
                self.require_same_type(self.mir.values[*value], place.ty, "store value")?;
                self.require_scalar_type(place.ty, "store")?;
                self.load_value(*value, "rax", out);
                self.place_address(place, "r11", out)?;
                self.store_memory("rax", "r11", place.ty, out)?;
            }
            Instruction::Copy {
                destination,
                source,
                size,
            } => {
                if destination.ty != source.ty
                    || *size != self.hir.types.layout(destination.ty).size
                {
                    return Err(codegen_error("aggregate copy type or size mismatch"));
                }
                self.place_address(destination, "rdi", out)?;
                self.place_address(source, "rsi", out)?;
                writeln!(out, "  mov rcx, {size}").unwrap();
                out.push_str("  rep movsb\n");
            }
            Instruction::Unary { dst, op, operand } => {
                self.validate_unary(*dst, *op, *operand)?;
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
                self.require_same_type(self.mir.values[*value], *from, "cast source")?;
                self.require_same_type(self.mir.values[*dst], *to, "cast result")?;
                self.require_scalar_type(*from, "cast source")?;
                self.require_scalar_type(*to, "cast result")?;
                if self.hir.types.is_float(*from) || self.hir.types.is_float(*to) {
                    return Err(codegen_error(
                        "floating-point cast escaped O-core type checking",
                    ));
                }
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

    fn validate_function_address(
        &self,
        dst: ValueId,
        function: FunctionId,
    ) -> Result<(), Diagnostic> {
        let target = &self.hir.functions[function];
        let expected_params = target
            .params
            .iter()
            .map(|local| target.locals[*local].ty)
            .collect::<Vec<_>>();
        match &self.hir.types.types[self.mir.values[dst]] {
            Type::Function {
                params,
                result,
                abi,
            } if params == &expected_params && *result == target.result && *abi == target.abi => {
                Ok(())
            }
            _ => Err(codegen_error("function address type mismatch")),
        }
    }

    fn validate_unary(
        &self,
        dst: ValueId,
        op: UnaryOp,
        operand: ValueId,
    ) -> Result<(), Diagnostic> {
        let operand_ty = self.mir.values[operand];
        let dst_ty = self.mir.values[dst];
        match op {
            UnaryOp::Neg | UnaryOp::BitNot => {
                self.require_integer_type(operand_ty, "unary operand")?;
                self.require_same_type(dst_ty, operand_ty, "unary result")
            }
            UnaryOp::Not => {
                let bool_ty = self.hir.types.primitive("bool").unwrap();
                self.require_same_type(operand_ty, bool_ty, "logical-not operand")?;
                self.require_same_type(dst_ty, bool_ty, "logical-not result")
            }
            UnaryOp::Deref | UnaryOp::AddressOf { .. } => {
                Err(codegen_error("place unary operation escaped MIR lowering"))
            }
        }
    }

    fn validate_binary(
        &self,
        dst: ValueId,
        op: BinaryOp,
        lhs: ValueId,
        rhs: ValueId,
    ) -> Result<(), Diagnostic> {
        let lhs_ty = self.mir.values[lhs];
        let rhs_ty = self.mir.values[rhs];
        let dst_ty = self.mir.values[dst];
        if self.hir.types.is_float(lhs_ty) || self.hir.types.is_float(rhs_ty) {
            return Err(codegen_error(
                "floating-point binary operation escaped O-core type checking",
            ));
        }
        match op {
            BinaryOp::Add | BinaryOp::Sub
                if matches!(self.hir.types.types[lhs_ty], Type::Pointer { .. }) =>
            {
                self.require_integer_type(rhs_ty, "pointer offset")?;
                self.require_same_type(dst_ty, lhs_ty, "pointer arithmetic result")
            }
            BinaryOp::Add
            | BinaryOp::Sub
            | BinaryOp::Mul
            | BinaryOp::Div
            | BinaryOp::Rem
            | BinaryOp::BitAnd
            | BinaryOp::BitOr
            | BinaryOp::BitXor
            | BinaryOp::ShiftLeft
            | BinaryOp::ShiftRight => {
                self.require_integer_type(lhs_ty, "binary left operand")?;
                self.require_same_type(rhs_ty, lhs_ty, "binary right operand")?;
                self.require_same_type(dst_ty, lhs_ty, "binary result")
            }
            BinaryOp::Eq
            | BinaryOp::NotEq
            | BinaryOp::Less
            | BinaryOp::LessEq
            | BinaryOp::Greater
            | BinaryOp::GreaterEq => {
                self.require_scalar_type(lhs_ty, "comparison operand")?;
                self.require_same_type(rhs_ty, lhs_ty, "comparison right operand")?;
                let bool_ty = self.hir.types.primitive("bool").unwrap();
                self.require_same_type(dst_ty, bool_ty, "comparison result")
            }
            BinaryOp::LogicalAnd | BinaryOp::LogicalOr => Err(codegen_error(
                "logical operator escaped short-circuit lowering",
            )),
        }
    }

    fn validate_call(
        &self,
        dst: Option<ValueId>,
        function: FunctionId,
        args: &[ValueId],
    ) -> Result<(), Diagnostic> {
        let target = &self.hir.functions[function];
        if target.abi == Abi::Interrupt {
            return Err(codegen_error(
                "interrupt handler reached direct-call lowering",
            ));
        }
        if args.len() != target.params.len() {
            return Err(codegen_error("direct call argument count mismatch"));
        }
        for (arg, local) in args.iter().zip(&target.params) {
            let expected = target.locals[*local].ty;
            self.require_scalar_type(expected, "direct call argument")?;
            self.require_same_type(self.mir.values[*arg], expected, "direct call argument")?;
            if target.abi == Abi::SysV64 && self.hir.types.is_float(expected) {
                return Err(codegen_error(
                    "floating-point argument escaped sysv64 ABI checking",
                ));
            }
        }
        let no_result = matches!(
            self.hir.types.types[target.result],
            Type::Void | Type::Never
        );
        match (dst, no_result) {
            (None, true) => Ok(()),
            (Some(dst), false) => {
                self.require_scalar_type(target.result, "direct call result")?;
                self.require_same_type(self.mir.values[dst], target.result, "direct call result")?;
                if target.abi == Abi::SysV64 && self.hir.types.is_float(target.result) {
                    return Err(codegen_error(
                        "floating-point result escaped sysv64 ABI checking",
                    ));
                }
                Ok(())
            }
            _ => Err(codegen_error("direct call result shape mismatch")),
        }
    }

    fn validate_intrinsic(
        &self,
        dst: Option<ValueId>,
        intrinsic: Intrinsic,
        args: &[ValueId],
    ) -> Result<(), Diagnostic> {
        let u8_ty = self.hir.types.primitive("u8").unwrap();
        let u16_ty = self.hir.types.primitive("u16").unwrap();
        let u32_ty = self.hir.types.primitive("u32").unwrap();
        let u64_ty = self.hir.types.primitive("u64").unwrap();
        let usize_ty = self.hir.types.primitive("usize").unwrap();
        let signature = match intrinsic {
            Intrinsic::In8 => Some((vec![u16_ty], Some(u8_ty))),
            Intrinsic::In16 => Some((vec![u16_ty], Some(u16_ty))),
            Intrinsic::In32 => Some((vec![u16_ty], Some(u32_ty))),
            Intrinsic::Out8 => Some((vec![u16_ty, u8_ty], None)),
            Intrinsic::Out16 => Some((vec![u16_ty, u16_ty], None)),
            Intrinsic::Out32 => Some((vec![u16_ty, u32_ty], None)),
            Intrinsic::EnableInterrupts | Intrinsic::DisableInterrupts | Intrinsic::Halt => {
                Some((vec![], None))
            }
            Intrinsic::InvalidatePage => Some((vec![usize_ty], None)),
            Intrinsic::Syscall(count) => Some((vec![u64_ty; count as usize + 1], Some(u64_ty))),
            Intrinsic::VolatileLoad | Intrinsic::VolatileStore => {
                return Err(codegen_error(
                    "volatile intrinsic escaped MIR memory lowering",
                ))
            }
            Intrinsic::AtomicLoad
            | Intrinsic::AtomicStore
            | Intrinsic::AtomicExchange
            | Intrinsic::AtomicCompareExchange
            | Intrinsic::AtomicFetchAdd => None,
        };
        if let Some((expected_args, expected_result)) = signature {
            if args.len() != expected_args.len() {
                return Err(codegen_error("intrinsic argument count mismatch"));
            }
            for (arg, expected) in args.iter().zip(expected_args) {
                self.require_same_type(self.mir.values[*arg], expected, "intrinsic argument")?;
            }
            return self.require_optional_result(dst, expected_result, "intrinsic result");
        }

        let (value_count, order_count) = match intrinsic {
            Intrinsic::AtomicLoad => (0, 1),
            Intrinsic::AtomicStore | Intrinsic::AtomicExchange | Intrinsic::AtomicFetchAdd => {
                (1, 1)
            }
            Intrinsic::AtomicCompareExchange => (2, 2),
            _ => unreachable!(),
        };
        if args.len() != 1 + value_count + order_count {
            return Err(codegen_error("atomic intrinsic argument count mismatch"));
        }
        let pointer_ty = self.mir.values[args[0]];
        let Type::Pointer { mutable, pointee } = self.hir.types.types[pointer_ty] else {
            return Err(codegen_error("atomic intrinsic requires a pointer"));
        };
        self.require_integer_type(pointee, "atomic pointee")?;
        if !matches!(self.hir.types.layout(pointee).size, 1 | 2 | 4 | 8) {
            return Err(codegen_error(
                "atomic pointee must be a 1/2/4/8-byte integer",
            ));
        }
        if !mutable && intrinsic != Intrinsic::AtomicLoad {
            return Err(codegen_error("mutating atomic intrinsic requires *mut"));
        }
        for value in &args[1..1 + value_count] {
            self.require_same_type(self.mir.values[*value], pointee, "atomic value")?;
        }
        for order in &args[1 + value_count..] {
            self.require_same_type(self.mir.values[*order], u8_ty, "atomic ordering")?;
            let order = self
                .const_from_value(*order)
                .and_then(|value| u8::try_from(value).ok())
                .ok_or_else(|| codegen_error("atomic ordering must be constant in MIR"))?;
            super::typeck::validate_memory_order(intrinsic, order).map_err(codegen_error)?;
        }
        let result = (intrinsic != Intrinsic::AtomicStore).then_some(pointee);
        self.require_optional_result(dst, result, "atomic result")
    }

    fn require_optional_result(
        &self,
        actual: Option<ValueId>,
        expected: Option<TypeId>,
        context: &str,
    ) -> Result<(), Diagnostic> {
        match (actual, expected) {
            (None, None) => Ok(()),
            (Some(value), Some(ty)) => self.require_same_type(self.mir.values[value], ty, context),
            _ => Err(codegen_error(format!("{context} shape mismatch"))),
        }
    }

    fn require_same_type(
        &self,
        actual: TypeId,
        expected: TypeId,
        context: &str,
    ) -> Result<(), Diagnostic> {
        if actual == expected {
            Ok(())
        } else {
            Err(codegen_error(format!("{context} type mismatch")))
        }
    }

    fn require_scalar_type(&self, ty: TypeId, context: &str) -> Result<(), Diagnostic> {
        if self.hir.types.is_scalar(ty) {
            Ok(())
        } else {
            Err(codegen_error(format!("{context} requires a scalar type")))
        }
    }

    fn require_integer_type(&self, ty: TypeId, context: &str) -> Result<(), Diagnostic> {
        if self.hir.types.is_integer(ty) {
            Ok(())
        } else {
            Err(codegen_error(format!("{context} requires an integer type")))
        }
    }

    fn emit_binary(
        &self,
        dst: ValueId,
        op: BinaryOp,
        lhs: ValueId,
        rhs: ValueId,
        out: &mut String,
    ) -> Result<(), Diagnostic> {
        self.validate_binary(dst, op, lhs, rhs)?;
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
        self.validate_call(dst, function, args)?;
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
        self.validate_intrinsic(dst, intrinsic, args)?;
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
                    self.require_asm_value(self.mir.values[*value])?;
                    self.load_value(*value, register, out);
                }
                MirAsmOperand::Out { register, target } => {
                    validate_register(register)?;
                    self.require_asm_value(target.ty)?;
                    outputs.push((register.as_str(), target));
                }
                MirAsmOperand::InOut {
                    register,
                    input,
                    output,
                } => {
                    validate_register(register)?;
                    self.require_asm_value(self.mir.values[*input])?;
                    self.require_asm_value(output.ty)?;
                    self.require_same_type(
                        self.mir.values[*input],
                        output.ty,
                        "assembly inout operand",
                    )?;
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

    fn require_asm_value(&self, ty: TypeId) -> Result<(), Diagnostic> {
        self.require_scalar_type(ty, "inline assembly operand")?;
        if self.hir.types.is_float(ty) {
            return Err(codegen_error(
                "floating-point inline assembly operand escaped type checking",
            ));
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
                let no_result = matches!(
                    self.hir.types.types[self.source.result],
                    Type::Void | Type::Never
                );
                match (value, no_result) {
                    (Some(value), false) => {
                        self.require_scalar_type(self.source.result, "return value")?;
                        self.require_same_type(
                            self.mir.values[*value],
                            self.source.result,
                            "return value",
                        )?;
                        if self.source.abi == Abi::SysV64
                            && self.hir.types.is_float(self.source.result)
                        {
                            return Err(codegen_error(
                                "floating-point result escaped sysv64 ABI checking",
                            ));
                        }
                        self.load_value(*value, "rax", out);
                    }
                    (None, true) => {}
                    _ => return Err(codegen_error("return value shape mismatch")),
                }
                self.emit_epilogue(out);
            }
            Terminator::Jump(target) => {
                self.emit_phi_moves(block_id, *target, out)?;
                writeln!(out, "  jmp {}_bb{target}", self.label_prefix).unwrap();
            }
            Terminator::Branch {
                condition,
                then_block,
                else_block,
            } => {
                let bool_ty = self.hir.types.primitive("bool").unwrap();
                self.require_same_type(self.mir.values[*condition], bool_ty, "branch condition")?;
                let then_edge = format!("{}_bb{}_then_edge", self.label_prefix, block_id);
                self.load_value(*condition, "rax", out);
                out.push_str("  test rax, rax\n");
                writeln!(out, "  jne {then_edge}").unwrap();
                self.emit_phi_moves(block_id, *else_block, out)?;
                writeln!(out, "  jmp {}_bb{else_block}", self.label_prefix).unwrap();
                writeln!(out, "{then_edge}:").unwrap();
                self.emit_phi_moves(block_id, *then_block, out)?;
                writeln!(out, "  jmp {}_bb{then_block}", self.label_prefix).unwrap();
            }
        }
        Ok(())
    }

    fn emit_phi_moves(
        &self,
        from: BlockId,
        to: BlockId,
        out: &mut String,
    ) -> Result<(), Diagnostic> {
        for instruction in &self.mir.blocks[to].instructions {
            let Instruction::Phi { dst, incoming } = instruction else {
                continue;
            };
            if let Some((_, value)) = incoming.iter().find(|(block, _)| *block == from) {
                self.require_same_type(
                    self.mir.values[*value],
                    self.mir.values[*dst],
                    "phi incoming value",
                )?;
                self.load_value(*value, "rax", out);
                self.store_value(*dst, "rax", out);
            }
        }
        Ok(())
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
            PlaceBase::Pointer(value) => {
                if !matches!(
                    self.hir.types.types[self.mir.values[value]],
                    Type::Pointer { .. }
                ) {
                    return Err(codegen_error("place base is not a pointer"));
                }
                self.load_value(value, register, out);
            }
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
                    self.require_integer_type(self.mir.values[*index], "place index")?;
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

    #[test]
    fn rejects_float_mir_if_type_checking_is_bypassed() {
        let ast = parser::parse(
            "test.oc",
            r#"
module floats;
fn identity(value: f64) -> f64 { return value; }
"#,
        )
        .unwrap();
        let hir = typeck::check(&[("test.oc".into(), ast)]).unwrap();
        let mir = mir::lower(&hir).unwrap();
        let float_value = 0;
        assert!(hir.types.is_float(mir.functions[0].values[float_value]));

        let mut binary_mir = mir.clone();
        let bool_ty = hir.types.primitive("bool").unwrap();
        let binary_dst = binary_mir.functions[0].values.len();
        binary_mir.functions[0].values.push(bool_ty);
        binary_mir.functions[0].blocks[0]
            .instructions
            .push(Instruction::Binary {
                dst: binary_dst,
                op: BinaryOp::Less,
                lhs: float_value,
                rhs: float_value,
            });
        let binary_error = emit_assembly(&hir, &binary_mir).unwrap_err();
        assert!(binary_error.message.contains("floating-point binary"));

        let mut cast_mir = mir;
        let u64_ty = hir.types.primitive("u64").unwrap();
        let cast_dst = cast_mir.functions[0].values.len();
        cast_mir.functions[0].values.push(u64_ty);
        cast_mir.functions[0].blocks[0]
            .instructions
            .push(Instruction::Cast {
                dst: cast_dst,
                value: float_value,
                from: hir.types.primitive("f64").unwrap(),
                to: u64_ty,
            });
        let cast_error = emit_assembly(&hir, &cast_mir).unwrap_err();
        assert!(cast_error.message.contains("floating-point cast"));
    }

    #[test]
    fn rejects_other_mir_type_contract_violations() {
        let ast = parser::parse(
            "test.oc",
            r#"
module guards;
fn choose(flag: bool, count: u64) -> u64 {
    if flag { return count + 1; }
    return count;
}
"#,
        )
        .unwrap();
        let hir = typeck::check(&[("test.oc".into(), ast)]).unwrap();
        let mir = mir::lower(&hir).unwrap();
        let bool_ty = hir.types.primitive("bool").unwrap();
        let u8_ty = hir.types.primitive("u8").unwrap();
        let u64_ty = hir.types.primitive("u64").unwrap();
        let bool_value = mir.functions[0]
            .values
            .iter()
            .position(|ty| *ty == bool_ty)
            .unwrap();
        let integer_value = mir.functions[0]
            .values
            .iter()
            .position(|ty| *ty == u64_ty)
            .unwrap();

        let mut unary_mir = mir.clone();
        let unary_dst = unary_mir.functions[0].values.len();
        unary_mir.functions[0].values.push(bool_ty);
        unary_mir.functions[0].blocks[0]
            .instructions
            .push(Instruction::Unary {
                dst: unary_dst,
                op: UnaryOp::Neg,
                operand: bool_value,
            });
        let unary_error = emit_assembly(&hir, &unary_mir).unwrap_err();
        assert!(unary_error.message.contains("requires an integer type"));

        let mut branch_mir = mir.clone();
        let branch = branch_mir.functions[0]
            .blocks
            .iter_mut()
            .find_map(|block| match &mut block.terminator {
                Terminator::Branch { condition, .. } => Some(condition),
                _ => None,
            })
            .unwrap();
        *branch = integer_value;
        let branch_error = emit_assembly(&hir, &branch_mir).unwrap_err();
        assert!(branch_error
            .message
            .contains("branch condition type mismatch"));

        let mut atomic_mir = mir;
        let order = atomic_mir.functions[0].values.len();
        atomic_mir.functions[0].values.push(u8_ty);
        let result = atomic_mir.functions[0].values.len();
        atomic_mir.functions[0].values.push(u64_ty);
        atomic_mir.functions[0].blocks[0]
            .instructions
            .push(Instruction::Const {
                dst: order,
                value: MemoryOrder::Relaxed as u64,
            });
        atomic_mir.functions[0].blocks[0]
            .instructions
            .push(Instruction::Intrinsic {
                dst: Some(result),
                intrinsic: Intrinsic::AtomicLoad,
                args: vec![integer_value, order],
            });
        let atomic_error = emit_assembly(&hir, &atomic_mir).unwrap_err();
        assert!(atomic_error.message.contains("requires a pointer"));

        let atomic_ast = parser::parse(
            "atomic.oc",
            r#"
module guards;
unsafe fn load(pointer: *const u64) -> u64 {
    return atomic_load(pointer, relaxed);
}
"#,
        )
        .unwrap();
        let atomic_hir = typeck::check(&[("atomic.oc".into(), atomic_ast)]).unwrap();
        let mut invalid_order_mir = mir::lower(&atomic_hir).unwrap();
        let order = invalid_order_mir.functions[0]
            .blocks
            .iter_mut()
            .flat_map(|block| &mut block.instructions)
            .find_map(|instruction| match instruction {
                Instruction::Const { value, .. } if *value == MemoryOrder::Relaxed as u64 => {
                    Some(value)
                }
                _ => None,
            })
            .unwrap();
        *order = 255;
        let order_error = emit_assembly(&atomic_hir, &invalid_order_mir).unwrap_err();
        assert!(order_error.message.contains("invalid memory ordering"));
    }
}
