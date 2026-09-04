//! AArch64 freestanding assembly generation from typed SSA MIR.
//!
//! This is deliberately a conservative scalar backend. It uses AAPCS64
//! integer/pointer registers, spills every local and SSA value to the stack,
//! and rejects target features whose semantics are not implemented instead of
//! selecting an approximately equivalent instruction.
//!
//! The current lowering reserves x13--x17 as emitter scratch registers; source
//! values are always reloaded from frame slots around their use. Compiler-built
//! MIR also emits at most one phi in each fresh short-circuit join block, so the
//! sequential phi edge move below is a complete copy for every source program.
//! A future public Machine IR or multi-phi lowering must add parallel-copy
//! resolution and preserve the scratch-register reservation explicitly.

use std::fmt::Write as _;

use super::ast::{Abi, BinaryOp, UnaryOp};
use super::hir::*;
use super::mir::*;
use super::{Diagnostic, Span};

pub fn emit_assembly(hir: &HirProgram, mir: &MirProgram) -> Result<String, Diagnostic> {
    emit_assembly_with_function_sections(hir, mir, false)
}

pub(crate) fn emit_assembly_with_function_sections(
    hir: &HirProgram,
    mir: &MirProgram,
    function_sections: bool,
) -> Result<String, Diagnostic> {
    let mut out = String::new();
    out.push_str(".file \"ocore\"\n");
    emit_statics(hir, &mut out)?;
    for function in &mir.functions {
        FunctionCodegen::new(hir, function).emit(&mut out, function_sections)?;
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
            for (payload_ty, value) in variant_def.payload.iter().zip(payload) {
                let layout = hir.types.layout(*payload_ty);
                let next = align_up(cursor, layout.align);
                if next > cursor {
                    writeln!(out, ".zero {}", next - cursor).unwrap();
                }
                emit_const(hir, *payload_ty, value, out)?;
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
        2 => writeln!(out, ".hword {value}").unwrap(),
        4 => writeln!(out, ".word {value}").unwrap(),
        8 => writeln!(out, ".xword {value}").unwrap(),
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
            label_prefix: format!(".L_ocore_aarch64_{}", mir.source),
        }
    }

    fn emit(&self, out: &mut String, function_sections: bool) -> Result<(), Diagnostic> {
        if self.source.attrs.naked {
            return Err(codegen_error(
                "@naked functions are not implemented by the AArch64 backend",
            ));
        }
        if self.source.attrs.interrupt || self.source.abi == Abi::Interrupt {
            return Err(codegen_error(
                "interrupt ABI functions are not implemented by the AArch64 backend",
            ));
        }
        if self.source.abi == Abi::SysV64 {
            return Err(codegen_error(
                "extern \"sysv64\" is the AMD64 System V ABI and is unsupported on AArch64; use extern \"ocore\" for the compiler-versioned native ABI",
            ));
        }
        if self.source.params.len() > 8 {
            return Err(codegen_error(
                "AArch64 scalar functions currently support at most 8 arguments",
            ));
        }

        let default_section =
            function_sections.then(|| format!(".text.ocore_fn.{}", self.source.symbol));
        let section = self
            .source
            .attrs
            .link_section
            .as_deref()
            .or(default_section.as_deref())
            .unwrap_or(".text");
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
        out.push_str("  stp x29, x30, [sp, #-16]!\n  mov x29, sp\n");
        self.reserve_stack(out);
        self.store_parameters(out)?;
        writeln!(out, "  b {}_bb{}", self.label_prefix, self.mir.entry).unwrap();

        for (block_id, block) in self.mir.blocks.iter().enumerate() {
            writeln!(out, "{}_bb{}:", self.label_prefix, block_id).unwrap();
            for (instruction_index, instruction) in block.instructions.iter().enumerate() {
                if !matches!(instruction, Instruction::Phi { .. }) {
                    self.emit_instruction(block_id, instruction_index, instruction, out)?;
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

    fn reserve_stack(&self, out: &mut String) {
        let mut remaining = self.frame.size;
        while remaining != 0 {
            let chunk = remaining.min(4080);
            writeln!(out, "  sub sp, sp, #{chunk}").unwrap();
            remaining -= chunk;
        }
    }

    fn store_parameters(&self, out: &mut String) -> Result<(), Diagnostic> {
        for (index, local) in self.source.params.iter().enumerate() {
            let ty = self.source.locals[*local].ty;
            self.require_scalar_type(ty, "function parameter")?;
            if self.hir.types.is_float(ty) {
                return Err(codegen_error(
                    "floating-point parameters are not implemented by the AArch64 backend",
                ));
            }
            let register = format!("x{index}");
            self.store_reg_to_frame(&register, self.frame.locals[*local], ty, out)?;
        }
        Ok(())
    }

    fn emit_instruction(
        &self,
        block_id: BlockId,
        instruction_index: usize,
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
                emit_u64("x9", *value, out);
                self.normalize_scalar("x9", self.mir.values[*dst], out)?;
                self.store_value(*dst, "x9", out);
            }
            Instruction::FunctionAddress { dst, function } => {
                self.validate_function_address(*dst, *function)?;
                emit_symbol_address("x9", &self.hir.functions[*function].symbol, out);
                self.store_value(*dst, "x9", out);
            }
            Instruction::AddressOf { dst, place } => {
                let Type::Pointer { pointee, .. } = self.hir.types.types[self.mir.values[*dst]]
                else {
                    return Err(codegen_error("address result is not a pointer"));
                };
                if pointee != place.ty {
                    return Err(codegen_error("address result pointee type mismatch"));
                }
                self.place_address(place, "x10", out)?;
                self.store_value(*dst, "x10", out);
            }
            Instruction::Load { dst, place, .. } => {
                self.require_same_type(self.mir.values[*dst], place.ty, "load result")?;
                self.require_scalar_type(place.ty, "load")?;
                self.place_address(place, "x10", out)?;
                self.load_memory("x9", "x10", place.ty, out)?;
                self.store_value(*dst, "x9", out);
            }
            Instruction::Store { place, value, .. } => {
                self.require_same_type(self.mir.values[*value], place.ty, "store value")?;
                self.require_scalar_type(place.ty, "store")?;
                self.load_value(*value, "x9", out);
                self.place_address(place, "x10", out)?;
                self.store_memory("x9", "x10", place.ty, out)?;
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
                if *size != 0 {
                    let loop_label = format!(
                        "{}_bb{}_copy{}_loop",
                        self.label_prefix, block_id, instruction_index
                    );
                    self.place_address(destination, "x10", out)?;
                    self.place_address(source, "x11", out)?;
                    emit_u64("x12", *size, out);
                    writeln!(out, "{loop_label}:").unwrap();
                    out.push_str(
                        "  ldrb w13, [x11], #1\n  strb w13, [x10], #1\n  subs x12, x12, #1\n",
                    );
                    writeln!(out, "  b.ne {loop_label}").unwrap();
                }
            }
            Instruction::Unary { dst, op, operand } => {
                self.validate_unary(*dst, *op, *operand)?;
                self.load_value(*operand, "x9", out);
                match op {
                    UnaryOp::Neg => out.push_str("  neg x9, x9\n"),
                    UnaryOp::Not => out.push_str("  eor x9, x9, #1\n"),
                    UnaryOp::BitNot => out.push_str("  mvn x9, x9\n"),
                    UnaryOp::Deref | UnaryOp::AddressOf { .. } => {
                        return Err(codegen_error("place unary operation escaped MIR lowering"));
                    }
                }
                self.normalize_scalar("x9", self.mir.values[*dst], out)?;
                self.store_value(*dst, "x9", out);
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
                self.load_value(*value, "x9", out);
                self.normalize_scalar("x9", *from, out)?;
                self.normalize_scalar("x9", *to, out)?;
                self.store_value(*dst, "x9", out);
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
            Instruction::Asm { .. } => {
                return Err(codegen_error(
                    "inline assembly is not implemented by the AArch64 backend",
                ));
            }
        }
        Ok(())
    }

    fn validate_function_address(
        &self,
        dst: ValueId,
        function: FunctionId,
    ) -> Result<(), Diagnostic> {
        let target = &self.hir.functions[function];
        if target.abi == Abi::SysV64 {
            return Err(codegen_error(
                "cannot take an AMD64 sysv64 function address on AArch64",
            ));
        }
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
            BinaryOp::ShiftLeft | BinaryOp::ShiftRight => {
                self.require_integer_type(lhs_ty, "shifted value")?;
                self.require_integer_type(rhs_ty, "shift count")?;
                self.require_same_type(dst_ty, lhs_ty, "shift result")
            }
            BinaryOp::Add
            | BinaryOp::Sub
            | BinaryOp::Mul
            | BinaryOp::Div
            | BinaryOp::Rem
            | BinaryOp::BitAnd
            | BinaryOp::BitOr
            | BinaryOp::BitXor => {
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

    fn emit_binary(
        &self,
        dst: ValueId,
        op: BinaryOp,
        lhs: ValueId,
        rhs: ValueId,
        out: &mut String,
    ) -> Result<(), Diagnostic> {
        self.validate_binary(dst, op, lhs, rhs)?;
        self.load_value(lhs, "x9", out);
        self.load_value(rhs, "x10", out);
        let signed = is_signed(&self.hir.types.types[self.mir.values[lhs]]);
        match op {
            BinaryOp::Add | BinaryOp::Sub => {
                if let Type::Pointer { pointee, .. } = self.hir.types.types[self.mir.values[lhs]] {
                    let scale = self.hir.types.layout(pointee).size;
                    if scale != 1 {
                        emit_u64("x11", scale, out);
                        out.push_str("  mul x10, x10, x11\n");
                    }
                }
                writeln!(
                    out,
                    "  {} x9, x9, x10",
                    if op == BinaryOp::Add { "add" } else { "sub" }
                )
                .unwrap();
            }
            BinaryOp::Mul => out.push_str("  mul x9, x9, x10\n"),
            BinaryOp::Div | BinaryOp::Rem => {
                // Unlike AMD64 div/idiv, AArch64 returns zero for a zero
                // divisor and does not trap on signed MIN / -1 overflow.
                // Preserve the existing O-core fail-closed arithmetic
                // behavior explicitly before selecting sdiv/udiv.
                out.push_str("  cbnz x10, 1f\n  brk #0\n1:\n");
                if signed && self.hir.types.layout(self.mir.values[lhs]).size == 8 {
                    emit_u64("x11", 0x8000_0000_0000_0000, out);
                    out.push_str(
                        "  cmp x9, x11\n  b.ne 2f\n  cmn x10, #1\n  b.ne 2f\n  brk #0\n2:\n",
                    );
                }
                writeln!(
                    out,
                    "  {} x11, x9, x10",
                    if signed { "sdiv" } else { "udiv" }
                )
                .unwrap();
                if op == BinaryOp::Rem {
                    out.push_str("  msub x9, x11, x10, x9\n");
                } else {
                    out.push_str("  mov x9, x11\n");
                }
            }
            BinaryOp::BitAnd => out.push_str("  and x9, x9, x10\n"),
            BinaryOp::BitOr => out.push_str("  orr x9, x9, x10\n"),
            BinaryOp::BitXor => out.push_str("  eor x9, x9, x10\n"),
            BinaryOp::ShiftLeft => out.push_str("  lsl x9, x9, x10\n"),
            BinaryOp::ShiftRight => out.push_str(if signed {
                "  asr x9, x9, x10\n"
            } else {
                "  lsr x9, x9, x10\n"
            }),
            BinaryOp::Eq
            | BinaryOp::NotEq
            | BinaryOp::Less
            | BinaryOp::LessEq
            | BinaryOp::Greater
            | BinaryOp::GreaterEq => {
                out.push_str("  cmp x9, x10\n");
                let condition = match (op, signed) {
                    (BinaryOp::Eq, _) => "eq",
                    (BinaryOp::NotEq, _) => "ne",
                    (BinaryOp::Less, true) => "lt",
                    (BinaryOp::LessEq, true) => "le",
                    (BinaryOp::Greater, true) => "gt",
                    (BinaryOp::GreaterEq, true) => "ge",
                    (BinaryOp::Less, false) => "lo",
                    (BinaryOp::LessEq, false) => "ls",
                    (BinaryOp::Greater, false) => "hi",
                    (BinaryOp::GreaterEq, false) => "hs",
                    _ => unreachable!(),
                };
                writeln!(out, "  cset x9, {condition}").unwrap();
            }
            BinaryOp::LogicalAnd | BinaryOp::LogicalOr => {
                return Err(codegen_error(
                    "logical operator escaped short-circuit lowering",
                ));
            }
        }
        self.normalize_scalar("x9", self.mir.values[dst], out)?;
        self.store_value(dst, "x9", out);
        Ok(())
    }

    fn validate_call(
        &self,
        dst: Option<ValueId>,
        function: FunctionId,
        args: &[ValueId],
    ) -> Result<(), Diagnostic> {
        let target = &self.hir.functions[function];
        if target.abi == Abi::SysV64 {
            return Err(codegen_error(
                "extern \"sysv64\" is the AMD64 System V ABI and cannot be called from AArch64",
            ));
        }
        if target.abi == Abi::Interrupt || target.attrs.interrupt {
            return Err(codegen_error(
                "interrupt handler reached AArch64 direct-call lowering",
            ));
        }
        if args.len() > 8 || target.params.len() > 8 {
            return Err(codegen_error(
                "AArch64 scalar calls currently support at most 8 arguments",
            ));
        }
        if args.len() != target.params.len() {
            return Err(codegen_error("direct call argument count mismatch"));
        }
        for (arg, local) in args.iter().zip(&target.params) {
            let expected = target.locals[*local].ty;
            self.require_scalar_type(expected, "direct call argument")?;
            self.require_same_type(self.mir.values[*arg], expected, "direct call argument")?;
            if self.hir.types.is_float(expected) {
                return Err(codegen_error(
                    "floating-point calls are not implemented by the AArch64 backend",
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
                if self.hir.types.is_float(target.result) {
                    return Err(codegen_error(
                        "floating-point calls are not implemented by the AArch64 backend",
                    ));
                }
                Ok(())
            }
            _ => Err(codegen_error("direct call result shape mismatch")),
        }
    }

    fn emit_call(
        &self,
        dst: Option<ValueId>,
        function: FunctionId,
        args: &[ValueId],
        out: &mut String,
    ) -> Result<(), Diagnostic> {
        self.validate_call(dst, function, args)?;
        for (index, value) in args.iter().enumerate() {
            self.load_value(*value, &format!("x{index}"), out);
        }
        writeln!(out, "  bl {}", self.hir.functions[function].symbol).unwrap();
        if let Some(dst) = dst {
            self.normalize_scalar("x0", self.mir.values[dst], out)?;
            self.store_value(dst, "x0", out);
        }
        Ok(())
    }

    fn validate_intrinsic(
        &self,
        dst: Option<ValueId>,
        intrinsic: Intrinsic,
        args: &[ValueId],
    ) -> Result<(), Diagnostic> {
        match intrinsic {
            Intrinsic::In8
            | Intrinsic::In16
            | Intrinsic::In32
            | Intrinsic::Out8
            | Intrinsic::Out16
            | Intrinsic::Out32 => {
                return Err(codegen_error(
                    "x86 port-I/O intrinsics are unsupported on AArch64",
                ));
            }
            Intrinsic::AtomicLoad
            | Intrinsic::AtomicStore
            | Intrinsic::AtomicExchange
            | Intrinsic::AtomicCompareExchange
            | Intrinsic::AtomicFetchAdd => {
                return Err(codegen_error(
                    "atomic intrinsics are not implemented by the AArch64 backend",
                ));
            }
            Intrinsic::InvalidatePage => {
                return Err(codegen_error(
                    "invalidate_page is not implemented by the AArch64 backend",
                ));
            }
            Intrinsic::VolatileLoad | Intrinsic::VolatileStore => {
                return Err(codegen_error(
                    "volatile intrinsic escaped MIR memory lowering",
                ));
            }
            _ => {}
        }

        let u64_ty = self.hir.types.primitive("u64").unwrap();
        let signature = match intrinsic {
            Intrinsic::EnableInterrupts | Intrinsic::DisableInterrupts | Intrinsic::Halt => {
                (Vec::new(), None)
            }
            Intrinsic::Syscall(count) if count <= 6 => {
                (vec![u64_ty; count as usize + 1], Some(u64_ty))
            }
            Intrinsic::Syscall(_) => {
                return Err(codegen_error(
                    "AArch64 syscall lowering supports syscall0 through syscall6",
                ));
            }
            _ => unreachable!(),
        };
        if args.len() != signature.0.len() {
            return Err(codegen_error("intrinsic argument count mismatch"));
        }
        for (arg, expected) in args.iter().zip(signature.0) {
            self.require_same_type(self.mir.values[*arg], expected, "intrinsic argument")?;
        }
        self.require_optional_result(dst, signature.1, "intrinsic result")
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
            Intrinsic::EnableInterrupts => out.push_str("  msr daifclr, #2\n  isb\n"),
            Intrinsic::DisableInterrupts => out.push_str("  msr daifset, #2\n  isb\n"),
            Intrinsic::Halt => out.push_str("  wfi\n"),
            Intrinsic::Syscall(count) => {
                self.load_value(args[0], "x8", out);
                for (index, value) in args[1..].iter().take(count as usize).enumerate() {
                    self.load_value(*value, &format!("x{index}"), out);
                }
                out.push_str("  svc #0\n");
            }
            _ => unreachable!(),
        }
        if let Some(dst) = dst {
            self.normalize_scalar("x0", self.mir.values[dst], out)?;
            self.store_value(dst, "x0", out);
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
            Terminator::Unreachable => out.push_str("  brk #0\n"),
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
                        if self.hir.types.is_float(self.source.result) {
                            return Err(codegen_error(
                                "floating-point returns are not implemented by the AArch64 backend",
                            ));
                        }
                        self.load_value(*value, "x0", out);
                    }
                    (None, true) => {}
                    _ => return Err(codegen_error("return value shape mismatch")),
                }
                self.emit_epilogue(out);
            }
            Terminator::Jump(target) => {
                self.emit_phi_moves(block_id, *target, out)?;
                writeln!(out, "  b {}_bb{target}", self.label_prefix).unwrap();
            }
            Terminator::Branch {
                condition,
                then_block,
                else_block,
            } => {
                let bool_ty = self.hir.types.primitive("bool").unwrap();
                self.require_same_type(self.mir.values[*condition], bool_ty, "branch condition")?;
                let then_edge = format!("{}_bb{}_then_edge", self.label_prefix, block_id);
                self.load_value(*condition, "x9", out);
                writeln!(out, "  cbnz x9, {then_edge}").unwrap();
                self.emit_phi_moves(block_id, *else_block, out)?;
                writeln!(out, "  b {}_bb{else_block}", self.label_prefix).unwrap();
                writeln!(out, "{then_edge}:").unwrap();
                self.emit_phi_moves(block_id, *then_block, out)?;
                writeln!(out, "  b {}_bb{then_block}", self.label_prefix).unwrap();
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
                self.load_value(*value, "x9", out);
                self.store_value(*dst, "x9", out);
            }
        }
        Ok(())
    }

    fn emit_epilogue(&self, out: &mut String) {
        out.push_str("  mov sp, x29\n  ldp x29, x30, [sp], #16\n  ret\n");
    }

    fn place_address(
        &self,
        place: &Place,
        register: &str,
        out: &mut String,
    ) -> Result<(), Diagnostic> {
        match place.base {
            PlaceBase::Local(local) => {
                emit_frame_address(register, self.frame.locals[local], out);
            }
            PlaceBase::Static(static_id) => {
                emit_symbol_address(register, &self.hir.statics[static_id].symbol, out);
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
                        let scratch = scratch_other_than(register, "x16", "x14");
                        emit_u64(scratch, *offset, out);
                        writeln!(out, "  add {register}, {register}, {scratch}").unwrap();
                    }
                }
                Projection::Index {
                    index,
                    element_size,
                } => {
                    self.require_integer_type(self.mir.values[*index], "place index")?;
                    let index_register = scratch_other_than(register, "x17", "x13");
                    self.load_value(*index, index_register, out);
                    if *element_size != 1 {
                        let scale_register = if register != "x16" && index_register != "x16" {
                            "x16"
                        } else {
                            "x14"
                        };
                        emit_u64(scale_register, *element_size, out);
                        writeln!(
                            out,
                            "  mul {index_register}, {index_register}, {scale_register}"
                        )
                        .unwrap();
                    }
                    writeln!(out, "  add {register}, {register}, {index_register}").unwrap();
                }
            }
        }
        Ok(())
    }

    fn load_value(&self, value: ValueId, register: &str, out: &mut String) {
        let address = frame_address_scratch(register);
        emit_frame_address(address, self.frame.values[value], out);
        writeln!(out, "  ldr {register}, [{address}]").unwrap();
    }

    fn store_value(&self, value: ValueId, register: &str, out: &mut String) {
        let address = frame_address_scratch(register);
        emit_frame_address(address, self.frame.values[value], out);
        writeln!(out, "  str {register}, [{address}]").unwrap();
    }

    fn store_reg_to_frame(
        &self,
        register: &str,
        offset: u64,
        ty: TypeId,
        out: &mut String,
    ) -> Result<(), Diagnostic> {
        let address = frame_address_scratch(register);
        emit_frame_address(address, offset, out);
        self.store_memory(register, address, ty, out)
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
        let wdst = wreg(dst)?;
        match (size, signed) {
            (1, false) => writeln!(out, "  ldrb {wdst}, [{address}]").unwrap(),
            (1, true) => writeln!(out, "  ldrsb {dst}, [{address}]").unwrap(),
            (2, false) => writeln!(out, "  ldrh {wdst}, [{address}]").unwrap(),
            (2, true) => writeln!(out, "  ldrsh {dst}, [{address}]").unwrap(),
            (4, false) => writeln!(out, "  ldr {wdst}, [{address}]").unwrap(),
            (4, true) => writeln!(out, "  ldrsw {dst}, [{address}]").unwrap(),
            (8, _) => writeln!(out, "  ldr {dst}, [{address}]").unwrap(),
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
        let wsrc = wreg(src)?;
        match size {
            1 => writeln!(out, "  strb {wsrc}, [{address}]").unwrap(),
            2 => writeln!(out, "  strh {wsrc}, [{address}]").unwrap(),
            4 => writeln!(out, "  str {wsrc}, [{address}]").unwrap(),
            8 => writeln!(out, "  str {src}, [{address}]").unwrap(),
            _ => return Err(codegen_error(format!("cannot scalar-store {size} bytes"))),
        }
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
        let wregister = wreg(register)?;
        match (size, signed) {
            (1, false) => writeln!(out, "  uxtb {wregister}, {wregister}").unwrap(),
            (1, true) => writeln!(out, "  sxtb {register}, {wregister}").unwrap(),
            (2, false) => writeln!(out, "  uxth {wregister}, {wregister}").unwrap(),
            (2, true) => writeln!(out, "  sxth {register}, {wregister}").unwrap(),
            (4, false) => writeln!(out, "  mov {wregister}, {wregister}").unwrap(),
            (4, true) => writeln!(out, "  sxtw {register}, {wregister}").unwrap(),
            (8, _) | (0, _) => {}
            _ => {
                return Err(codegen_error(format!(
                    "cannot normalize {size}-byte scalar"
                )));
            }
        }
        Ok(())
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
}

fn emit_u64(register: &str, value: u64, out: &mut String) {
    let low = value & 0xffff;
    writeln!(out, "  movz {register}, #0x{low:x}").unwrap();
    for shift in [16u32, 32, 48] {
        let part = (value >> shift) & 0xffff;
        if part != 0 {
            writeln!(out, "  movk {register}, #0x{part:x}, lsl #{shift}").unwrap();
        }
    }
}

fn emit_symbol_address(register: &str, symbol: &str, out: &mut String) {
    writeln!(out, "  adrp {register}, {symbol}").unwrap();
    writeln!(out, "  add {register}, {register}, :lo12:{symbol}").unwrap();
}

fn emit_frame_address(register: &str, offset: u64, out: &mut String) {
    let scratch = scratch_other_than(register, "x16", "x14");
    emit_u64(scratch, offset, out);
    writeln!(out, "  sub {register}, x29, {scratch}").unwrap();
}

fn frame_address_scratch(register: &str) -> &'static str {
    scratch_other_than(register, "x15", "x14")
}

fn scratch_other_than(
    register: &str,
    preferred: &'static str,
    fallback: &'static str,
) -> &'static str {
    if register == preferred {
        fallback
    } else {
        preferred
    }
}

fn wreg(register: &str) -> Result<String, Diagnostic> {
    let Some(index) = register.strip_prefix('x') else {
        return Err(codegen_error(format!(
            "unsupported AArch64 register `{register}`"
        )));
    };
    let number = index
        .parse::<u8>()
        .map_err(|_| codegen_error(format!("unsupported AArch64 register `{register}`")))?;
    if number > 30 {
        return Err(codegen_error(format!(
            "unsupported AArch64 register `{register}`"
        )));
    }
    Ok(format!("w{number}"))
}

fn is_signed(ty: &Type) -> bool {
    matches!(ty, Type::Int { signed: true, .. } | Type::Isize)
}

fn codegen_error(message: impl Into<String>) -> Diagnostic {
    Diagnostic {
        file: "<aarch64-codegen>".into(),
        span: Span::default(),
        message: message.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ocore::{mir, parser, typeck};

    fn assembly(source: &str) -> Result<String, Diagnostic> {
        let ast = parser::parse("aarch64-test.oc", source)?;
        let hir = typeck::check(&[("aarch64-test.oc".into(), ast)])?;
        let mir = mir::lower(&hir)?;
        emit_assembly(&hir, &mir)
    }

    fn sectioned_assembly(source: &str) -> Result<String, Diagnostic> {
        let ast = parser::parse("aarch64-test.oc", source)?;
        let hir = typeck::check(&[("aarch64-test.oc".into(), ast)])?;
        let mir = mir::lower(&hir)?;
        emit_assembly_with_function_sections(&hir, &mir, true)
    }

    #[test]
    fn emits_aapcs64_scalar_memory_calls_and_control_flow() {
        let asm = assembly(
            r#"
module arm;
static mut TOTAL: u64 = 7;
fn add(left: u64, right: u64) -> u64 { return left + right; }
@export @no_mangle
unsafe fn exercise(pointer: *mut u64, left: u64, right: u64) -> u64 {
    let current: u64 = volatile_load(pointer);
    volatile_store(pointer, current + 1);
    if left < right {
        return add(current, left);
    }
    return current - right;
}
"#,
        )
        .unwrap();
        assert!(asm.contains("exercise:"));
        assert!(asm.contains("bl _O_arm__add"));
        assert!(asm.contains("ldr x9, [x10]"));
        assert!(asm.contains("str x9, [x10]"));
        assert!(asm.contains("cset x9, lo"));
        assert!(asm.contains("cbnz x9"));
        assert!(asm.contains("stp x29, x30"));
    }

    #[test]
    fn emits_opt_in_function_sections() {
        let asm = sectioned_assembly(
            "module arm_sections; fn add(a: u64, b: u64) -> u64 { return a + b; }",
        )
        .unwrap();
        assert!(asm.contains(".section .text.ocore_fn._O_arm_sections__add"));
    }

    #[test]
    fn emits_syscall_interrupt_masks_and_halt() {
        let asm = assembly(
            r#"
module system;
@export @no_mangle
unsafe fn invoke(number: u64, value: u64) -> u64 {
    return syscall1(number, value);
}
@export @no_mangle
unsafe fn idle() -> never {
    disable_interrupts();
    enable_interrupts();
    loop { halt(); }
}
"#,
        )
        .unwrap();
        assert!(asm.contains("ldr x8"));
        assert!(asm.contains("ldr x0"));
        assert!(asm.contains("svc #0"));
        assert!(asm.contains("msr daifset, #2"));
        assert!(asm.contains("msr daifclr, #2"));
        assert!(asm.contains("wfi"));
    }

    #[test]
    fn rejects_unimplemented_target_features() {
        let port_error = assembly(
            r#"
module target_error;
unsafe fn read_port() -> u8 { return inb(0x3f8); }
"#,
        )
        .unwrap_err();
        assert!(port_error.message.contains("port-I/O"));

        let atomic_error = assembly(
            r#"
module target_error;
unsafe fn load(pointer: *const u64) -> u64 {
    return atomic_load(pointer, relaxed);
}
"#,
        )
        .unwrap_err();
        assert!(atomic_error.message.contains("atomic intrinsics"));

        let asm_error = assembly(
            r#"
module target_error;
unsafe fn raw(value: u64) -> void {
    asm!("nop", in("x0") value);
}
"#,
        )
        .unwrap_err();
        assert!(asm_error.message.contains("inline assembly"));
    }

    #[test]
    fn rejects_more_than_eight_scalar_arguments() {
        let error = assembly(
            r#"
module many;
fn too_many(a: u64, b: u64, c: u64, d: u64, e: u64, f: u64, g: u64, h: u64, i: u64) -> u64 {
    return a + b + c + d + e + f + g + h + i;
}
"#,
        )
        .unwrap_err();
        assert!(error.message.contains("at most 8 arguments"));
    }

    #[test]
    fn rejects_amd64_sysv64_calls_on_aarch64() {
        let error = assembly(
            r#"
module foreign_abi;
extern "sysv64" fn foreign(value: u64) -> u64;
fn invoke(value: u64) -> u64 { return foreign(value); }
"#,
        )
        .unwrap_err();
        assert!(error.message.contains("AMD64 System V ABI"));
    }

    #[test]
    fn division_traps_instead_of_using_aarch64_silent_error_results() {
        let asm = assembly(
            r#"
module arithmetic;
fn signed_div(left: i64, right: i64) -> i64 { return left / right; }
fn unsigned_rem(left: u64, right: u64) -> u64 { return left % right; }
"#,
        )
        .unwrap();
        assert_eq!(asm.matches("cbnz x10, 1f").count(), 2);
        assert!(asm.contains("cmn x10, #1"));
        assert!(asm.matches("brk #0").count() >= 3);
    }
}
