use std::collections::{HashMap, HashSet};

use super::ast;
use super::hir::*;
use super::{Diagnostic, Span};

type TcResult<T> = Result<T, Diagnostic>;

#[derive(Clone)]
struct ModuleScope {
    file: String,
    name: String,
    imports: HashMap<String, String>,
}

pub fn check(modules: &[(String, ast::SourceModule)]) -> TcResult<HirProgram> {
    Checker::new(modules).run()
}

struct Checker<'a> {
    modules: &'a [(String, ast::SourceModule)],
    scopes: Vec<ModuleScope>,
    program: HirProgram,
    struct_defs: HashMap<String, (StructId, usize, usize)>,
    enum_defs: HashMap<String, (EnumId, usize, usize)>,
    function_defs: Vec<(usize, usize)>,
    static_defs: Vec<(usize, usize)>,
    const_defs: Vec<(usize, usize)>,
}

impl<'a> Checker<'a> {
    fn new(modules: &'a [(String, ast::SourceModule)]) -> Self {
        Self {
            modules,
            scopes: Vec::new(),
            program: HirProgram {
                types: TypeContext::new(),
                functions: Vec::new(),
                statics: Vec::new(),
                consts: Vec::new(),
                symbols: HashMap::new(),
            },
            struct_defs: HashMap::new(),
            enum_defs: HashMap::new(),
            function_defs: Vec::new(),
            static_defs: Vec::new(),
            const_defs: Vec::new(),
        }
    }

    fn run(mut self) -> TcResult<HirProgram> {
        self.build_module_scopes()?;
        self.collect_type_names()?;
        self.define_types()?;
        self.compute_layouts()?;
        self.collect_value_signatures()?;
        self.check_constants_and_statics()?;
        self.check_function_bodies()?;
        Ok(self.program)
    }

    fn build_module_scopes(&mut self) -> TcResult<()> {
        let mut seen = HashSet::new();
        for (file, module) in self.modules {
            let name = path_string(&module.name);
            if !seen.insert(name.clone()) {
                return Err(diag(
                    file,
                    module.span,
                    format!("duplicate module `{name}`"),
                ));
            }
            let mut imports = HashMap::new();
            for import in &module.uses {
                let qualified = path_string(&import.path);
                let alias = import
                    .alias
                    .clone()
                    .or_else(|| import.path.last().cloned())
                    .unwrap();
                if imports.insert(alias.clone(), qualified).is_some() {
                    return Err(diag(
                        file,
                        import.span,
                        format!("duplicate import name `{alias}`"),
                    ));
                }
            }
            self.scopes.push(ModuleScope {
                file: file.clone(),
                name,
                imports,
            });
        }
        Ok(())
    }

    fn collect_type_names(&mut self) -> TcResult<()> {
        for (module_index, (_, module)) in self.modules.iter().enumerate() {
            for (item_index, item) in module.items.iter().enumerate() {
                match &item.kind {
                    ast::ItemKind::Struct(def) => {
                        let qn = qualify(&self.scopes[module_index].name, &def.name);
                        self.ensure_name_free(&qn, module_index, item.span)?;
                        let (id, ty) = self.program.types.add_struct_placeholder(qn.clone());
                        self.program.symbols.insert(qn.clone(), Symbol::Type(ty));
                        self.struct_defs.insert(qn, (id, module_index, item_index));
                    }
                    ast::ItemKind::Enum(def) => {
                        let qn = qualify(&self.scopes[module_index].name, &def.name);
                        self.ensure_name_free(&qn, module_index, item.span)?;
                        let (id, ty) = self.program.types.add_enum_placeholder(qn.clone());
                        self.program.symbols.insert(qn.clone(), Symbol::Type(ty));
                        self.enum_defs.insert(qn, (id, module_index, item_index));
                    }
                    _ => {}
                }
            }
        }
        Ok(())
    }

    fn define_types(&mut self) -> TcResult<()> {
        let structs: Vec<_> = self.struct_defs.values().copied().collect();
        for (id, module_index, item_index) in structs {
            let item = &self.modules[module_index].1.items[item_index];
            let ast::ItemKind::Struct(def) = &item.kind else {
                unreachable!()
            };
            let attrs = parse_attrs(&self.scopes[module_index].file, item)?;
            let mut names = HashSet::new();
            let mut fields = Vec::new();
            for field in &def.fields {
                if !names.insert(field.name.clone()) {
                    return Err(diag(
                        &self.scopes[module_index].file,
                        field.span,
                        format!("duplicate field `{}`", field.name),
                    ));
                }
                let ty = self.resolve_type(module_index, &field.ty)?;
                fields.push(StructField {
                    name: field.name.clone(),
                    ty,
                    offset: 0,
                });
            }
            let entry = &mut self.program.types.structs[id];
            entry.fields = fields;
            entry.packed = attrs.packed;
            entry.requested_align = attrs.align;
        }

        let enums: Vec<_> = self.enum_defs.values().copied().collect();
        for (id, module_index, item_index) in enums {
            let item = &self.modules[module_index].1.items[item_index];
            let ast::ItemKind::Enum(def) = &item.kind else {
                unreachable!()
            };
            let mut names = HashSet::new();
            let mut variants = Vec::new();
            for variant in &def.variants {
                if !names.insert(variant.name.clone()) {
                    return Err(diag(
                        &self.scopes[module_index].file,
                        variant.span,
                        format!("duplicate enum variant `{}`", variant.name),
                    ));
                }
                let payload = variant
                    .payload
                    .iter()
                    .map(|ty| self.resolve_type(module_index, ty))
                    .collect::<TcResult<Vec<_>>>()?;
                variants.push(EnumVariant {
                    name: variant.name.clone(),
                    payload,
                    payload_layout: Layout { size: 0, align: 1 },
                });
            }
            self.program.types.enums[id].variants = variants;
        }
        Ok(())
    }

    fn compute_layouts(&mut self) -> TcResult<()> {
        let mut struct_state = vec![0u8; self.program.types.structs.len()];
        let mut enum_state = vec![0u8; self.program.types.enums.len()];
        for id in 0..struct_state.len() {
            self.layout_struct(id, &mut struct_state, &mut enum_state)?;
        }
        for id in 0..enum_state.len() {
            self.layout_enum(id, &mut struct_state, &mut enum_state)?;
        }
        Ok(())
    }

    fn layout_type(
        &mut self,
        ty: TypeId,
        struct_state: &mut [u8],
        enum_state: &mut [u8],
    ) -> TcResult<Layout> {
        match self.program.types.types[ty].clone() {
            Type::Struct(id) => {
                self.layout_struct(id, struct_state, enum_state)?;
                Ok(self.program.types.structs[id].layout)
            }
            Type::Enum(id) => {
                self.layout_enum(id, struct_state, enum_state)?;
                Ok(self.program.types.enums[id].layout)
            }
            Type::Array { element, len } => {
                let element = self.layout_type(element, struct_state, enum_state)?;
                let size = element
                    .size
                    .checked_mul(len)
                    .ok_or_else(|| self.layout_diag(ty, "array layout overflows u64"))?;
                Ok(Layout {
                    size,
                    align: element.align,
                })
            }
            _ => Ok(self.program.types.layout(ty)),
        }
    }

    fn layout_struct(
        &mut self,
        id: StructId,
        struct_state: &mut [u8],
        enum_state: &mut [u8],
    ) -> TcResult<()> {
        if struct_state[id] == 2 {
            return Ok(());
        }
        if struct_state[id] == 1 {
            return Err(self.layout_diag(
                self.program.types.interned_type_for_struct(id).unwrap_or(0),
                "recursive type has infinite size; use a pointer",
            ));
        }
        struct_state[id] = 1;
        let packed = self.program.types.structs[id].packed;
        let requested_align = self.program.types.structs[id].requested_align;
        let field_types: Vec<_> = self.program.types.structs[id]
            .fields
            .iter()
            .map(|f| f.ty)
            .collect();
        let mut offset = 0u64;
        let mut max_align = 1u64;
        let mut offsets = Vec::with_capacity(field_types.len());
        for ty in field_types {
            let layout = self.layout_type(ty, struct_state, enum_state)?;
            let field_align = if packed { 1 } else { layout.align };
            offset = align_up(offset, field_align);
            offsets.push(offset);
            offset = offset
                .checked_add(layout.size)
                .ok_or_else(|| self.layout_diag(ty, "struct layout overflows u64"))?;
            max_align = max_align.max(field_align);
        }
        if let Some(align) = requested_align {
            max_align = max_align.max(align);
        }
        for (field, offset) in self.program.types.structs[id]
            .fields
            .iter_mut()
            .zip(offsets)
        {
            field.offset = offset;
        }
        self.program.types.structs[id].layout = Layout {
            size: align_up(offset, max_align),
            align: max_align,
        };
        struct_state[id] = 2;
        Ok(())
    }

    fn layout_enum(
        &mut self,
        id: EnumId,
        struct_state: &mut [u8],
        enum_state: &mut [u8],
    ) -> TcResult<()> {
        if enum_state[id] == 2 {
            return Ok(());
        }
        if enum_state[id] == 1 {
            let name = self.program.types.enums[id].name.clone();
            return Err(diag(
                "<type>",
                Span::default(),
                format!("recursive enum `{name}` has infinite size"),
            ));
        }
        enum_state[id] = 1;
        let count = self.program.types.enums[id].variants.len();
        let tag_size = if count <= 256 {
            1
        } else if count <= 65_536 {
            2
        } else {
            4
        };
        let payloads: Vec<Vec<TypeId>> = self.program.types.enums[id]
            .variants
            .iter()
            .map(|v| v.payload.clone())
            .collect();
        let mut layouts = Vec::new();
        let mut max_payload_size = 0u64;
        let mut max_payload_align = 1u64;
        for payload in payloads {
            let mut offset = 0u64;
            let mut align = 1u64;
            for ty in payload {
                let layout = self.layout_type(ty, struct_state, enum_state)?;
                offset = align_up(offset, layout.align);
                offset += layout.size;
                align = align.max(layout.align);
            }
            let layout = Layout {
                size: align_up(offset, align),
                align,
            };
            max_payload_size = max_payload_size.max(layout.size);
            max_payload_align = max_payload_align.max(layout.align);
            layouts.push(layout);
        }
        let payload_offset = align_up(tag_size, max_payload_align);
        let overall_align = max_payload_align.max(tag_size);
        let overall_size = align_up(payload_offset + max_payload_size, overall_align);
        for (variant, layout) in self.program.types.enums[id]
            .variants
            .iter_mut()
            .zip(layouts)
        {
            variant.payload_layout = layout;
        }
        let entry = &mut self.program.types.enums[id];
        entry.tag_size = tag_size;
        entry.payload_offset = payload_offset;
        entry.layout = Layout {
            size: overall_size,
            align: overall_align,
        };
        enum_state[id] = 2;
        Ok(())
    }

    fn collect_value_signatures(&mut self) -> TcResult<()> {
        for (module_index, (_, module)) in self.modules.iter().enumerate() {
            for (item_index, item) in module.items.iter().enumerate() {
                match &item.kind {
                    ast::ItemKind::Function(def) => {
                        let qn = qualify(&self.scopes[module_index].name, &def.name);
                        self.ensure_name_free(&qn, module_index, item.span)?;
                        let attrs = parse_attrs(&self.scopes[module_index].file, item)?;
                        let mut abi = def.abi.clone();
                        if attrs.interrupt {
                            abi = ast::Abi::Interrupt;
                        }
                        let mut locals = Vec::new();
                        let mut params = Vec::new();
                        for param in &def.params {
                            let ty = self.resolve_type(module_index, &param.ty)?;
                            let id = locals.len();
                            locals.push(HirLocal {
                                name: param.name.clone(),
                                ty,
                                mutable: false,
                                parameter: true,
                                span: param.span,
                            });
                            params.push(id);
                        }
                        let result = self.resolve_type(module_index, &def.return_type)?;
                        let param_types =
                            params.iter().map(|id| locals[*id].ty).collect::<Vec<_>>();
                        validate_function_attrs(
                            &self.scopes[module_index].file,
                            item.span,
                            &attrs,
                            FunctionAttrContext {
                                abi: &abi,
                                params: &param_types,
                                result,
                                unsafe_: def.unsafe_,
                            },
                            &self.program.types,
                        )?;
                        let symbol = item_symbol(
                            &self.scopes[module_index].name,
                            &def.name,
                            &attrs,
                            def.body.is_none(),
                        );
                        let id = self.program.functions.len();
                        self.program.functions.push(HirFunction {
                            qualified_name: qn.clone(),
                            symbol,
                            attrs,
                            public: item.public,
                            unsafe_: def.unsafe_,
                            abi,
                            params,
                            result,
                            locals,
                            body: None,
                            span: item.span,
                        });
                        self.program.symbols.insert(qn, Symbol::Function(id));
                        self.function_defs.push((module_index, item_index));
                    }
                    ast::ItemKind::Static(def) => {
                        let qn = qualify(&self.scopes[module_index].name, &def.name);
                        self.ensure_name_free(&qn, module_index, item.span)?;
                        let attrs = parse_attrs(&self.scopes[module_index].file, item)?;
                        if def.mutable
                            && attrs
                                .link_section
                                .as_deref()
                                .is_some_and(|s| s.starts_with(".text"))
                            && !attrs.unsafe_linkage
                        {
                            return Err(diag(
                                &self.scopes[module_index].file,
                                item.span,
                                "writable static in executable section requires @unsafe_linkage",
                            ));
                        }
                        let ty = self.resolve_type(module_index, &def.ty)?;
                        let symbol =
                            item_symbol(&self.scopes[module_index].name, &def.name, &attrs, false);
                        let id = self.program.statics.len();
                        self.program.statics.push(HirStatic {
                            qualified_name: qn.clone(),
                            symbol,
                            attrs,
                            public: item.public,
                            mutable: def.mutable,
                            ty,
                            init: HirConstValue::Zero,
                            span: item.span,
                        });
                        self.program.symbols.insert(qn, Symbol::Static(id));
                        self.static_defs.push((module_index, item_index));
                    }
                    ast::ItemKind::Const(def) => {
                        let qn = qualify(&self.scopes[module_index].name, &def.name);
                        self.ensure_name_free(&qn, module_index, item.span)?;
                        let ty = self.resolve_type(module_index, &def.ty)?;
                        let id = self.program.consts.len();
                        self.program.consts.push(HirConst {
                            qualified_name: qn.clone(),
                            ty,
                            value: HirConstValue::Zero,
                            span: item.span,
                        });
                        self.program.symbols.insert(qn, Symbol::Const(id));
                        self.const_defs.push((module_index, item_index));
                    }
                    ast::ItemKind::Struct(_) | ast::ItemKind::Enum(_) => {}
                }
            }
        }
        Ok(())
    }

    fn check_constants_and_statics(&mut self) -> TcResult<()> {
        let const_defs = self.const_defs.clone();
        for (id, (module_index, item_index)) in const_defs.into_iter().enumerate() {
            let item = &self.modules[module_index].1.items[item_index];
            let ast::ItemKind::Const(def) = &item.kind else {
                unreachable!()
            };
            let value = self.const_value(module_index, &def.init, self.program.consts[id].ty)?;
            self.program.consts[id].value = value;
        }
        let static_defs = self.static_defs.clone();
        for (id, (module_index, item_index)) in static_defs.into_iter().enumerate() {
            let item = &self.modules[module_index].1.items[item_index];
            let ast::ItemKind::Static(def) = &item.kind else {
                unreachable!()
            };
            let value = self.const_value(module_index, &def.init, self.program.statics[id].ty)?;
            self.program.statics[id].init = value;
        }
        Ok(())
    }

    fn check_function_bodies(&mut self) -> TcResult<()> {
        let defs = self.function_defs.clone();
        for (function_id, (module_index, item_index)) in defs.into_iter().enumerate() {
            let item = &self.modules[module_index].1.items[item_index];
            let ast::ItemKind::Function(def) = &item.kind else {
                unreachable!()
            };
            let Some(body) = &def.body else { continue };
            let mut function = self.program.functions[function_id].clone();
            let mut body_checker = BodyChecker::new(
                &mut self.program,
                &self.scopes[module_index],
                function_id,
                &mut function.locals,
                function.result,
                function.unsafe_,
            );
            let hir_body = body_checker.check_block(body)?;
            let result_ty = self.program.types.types[function.result].clone();
            if !matches!(result_ty, Type::Void | Type::Never) && !block_terminates(&hir_body) {
                return Err(diag(
                    &self.scopes[module_index].file,
                    body.span,
                    format!(
                        "function `{}` may fall through without returning `{}`",
                        function.qualified_name,
                        self.program.types.name(function.result)
                    ),
                ));
            }
            if matches!(result_ty, Type::Never) && !block_terminates(&hir_body) {
                return Err(diag(
                    &self.scopes[module_index].file,
                    body.span,
                    "function returning `never` must not fall through",
                ));
            }
            function.body = Some(hir_body);
            self.program.functions[function_id] = function;
        }
        Ok(())
    }

    fn const_value(
        &self,
        module_index: usize,
        expr: &ast::Expr,
        expected: TypeId,
    ) -> TcResult<HirConstValue> {
        let file = &self.scopes[module_index].file;
        match (&expr.kind, &self.program.types.types[expected]) {
            (ast::ExprKind::Integer(v), ty) if is_integer_type(ty) => {
                Ok(HirConstValue::Integer(*v))
            }
            (
                ast::ExprKind::Byte(v),
                Type::Int {
                    signed: false,
                    bits: 8,
                },
            ) => Ok(HirConstValue::Integer(*v as u64)),
            (ast::ExprKind::Bool(v), Type::Bool) => Ok(HirConstValue::Bool(*v)),
            (ast::ExprKind::ByteString(v), Type::Array { element, len })
                if matches!(
                    self.program.types.types[*element],
                    Type::Int {
                        signed: false,
                        bits: 8
                    }
                ) && *len == v.len() as u64 =>
            {
                Ok(HirConstValue::Bytes(v.clone()))
            }
            (ast::ExprKind::Array(values), Type::Array { element, len })
                if *len == values.len() as u64 =>
            {
                Ok(HirConstValue::Array(
                    values
                        .iter()
                        .map(|v| self.const_value(module_index, v, *element))
                        .collect::<TcResult<Vec<_>>>()?,
                ))
            }
            (
                ast::ExprKind::ArrayRepeat { value, len },
                Type::Array {
                    element,
                    len: expected_len,
                },
            ) if len == expected_len => Ok(HirConstValue::Repeat(
                Box::new(self.const_value(module_index, value, *element)?),
                *len,
            )),
            (ast::ExprKind::Struct { path, fields }, Type::Struct(id)) => {
                let qn = self.resolve_path(module_index, path);
                let expected_name = &self.program.types.structs[*id].name;
                if &qn != expected_name {
                    return Err(diag(
                        file,
                        expr.span,
                        format!("expected `{expected_name}`, found `{qn}`"),
                    ));
                }
                let values =
                    self.order_const_struct_fields(module_index, *id, fields, expr.span)?;
                Ok(HirConstValue::Struct(*id, values))
            }
            (ast::ExprKind::Path(path), _) => {
                let qn = self.resolve_path(module_index, path);
                if let Some(Symbol::Const(id)) = self.program.symbols.get(&qn) {
                    if self.program.consts[*id].ty != expected {
                        return Err(diag(file, expr.span, "constant type mismatch"));
                    }
                    Ok(self.program.consts[*id].value.clone())
                } else if let Some((enum_id, variant)) = self.resolve_enum_variant(&qn) {
                    if expected != self.type_for_enum(enum_id)
                        || !self.program.types.enums[enum_id].variants[variant]
                            .payload
                            .is_empty()
                    {
                        return Err(diag(
                            file,
                            expr.span,
                            "enum constant does not match expected type",
                        ));
                    }
                    Ok(HirConstValue::Enum(enum_id, variant, Vec::new()))
                } else {
                    Err(diag(
                        file,
                        expr.span,
                        "initializer is not a compile-time constant",
                    ))
                }
            }
            _ => Err(diag(
                file,
                expr.span,
                format!(
                    "initializer is not a `{}` compile-time constant",
                    self.program.types.name(expected)
                ),
            )),
        }
    }

    fn order_const_struct_fields(
        &self,
        module_index: usize,
        struct_id: StructId,
        fields: &[(String, ast::Expr)],
        span: Span,
    ) -> TcResult<Vec<HirConstValue>> {
        let def = &self.program.types.structs[struct_id];
        if fields.len() != def.fields.len() {
            return Err(diag(
                &self.scopes[module_index].file,
                span,
                format!("initializer for `{}` must specify every field", def.name),
            ));
        }
        let mut result = Vec::new();
        for field in &def.fields {
            let Some((_, value)) = fields.iter().find(|(name, _)| name == &field.name) else {
                return Err(diag(
                    &self.scopes[module_index].file,
                    span,
                    format!("missing field `{}`", field.name),
                ));
            };
            result.push(self.const_value(module_index, value, field.ty)?);
        }
        Ok(result)
    }

    fn resolve_type(&mut self, module_index: usize, ty: &ast::TypeExpr) -> TcResult<TypeId> {
        let resolved = match &ty.kind {
            ast::TypeExprKind::Named(path) => {
                if path.len() == 1 {
                    if let Some(id) = self.program.types.primitive(&path[0]) {
                        return Ok(id);
                    }
                }
                let qn = self.resolve_path(module_index, path);
                match self.program.symbols.get(&qn) {
                    Some(Symbol::Type(id)) => *id,
                    _ => {
                        return Err(diag(
                            &self.scopes[module_index].file,
                            ty.span,
                            format!("unknown type `{qn}`"),
                        ))
                    }
                }
            }
            ast::TypeExprKind::Pointer { mutable, pointee } => {
                let pointee = self.resolve_type(module_index, pointee)?;
                self.program.types.intern(Type::Pointer {
                    mutable: *mutable,
                    pointee,
                })
            }
            ast::TypeExprKind::Array { element, len } => {
                let element = self.resolve_type(module_index, element)?;
                self.program
                    .types
                    .intern(Type::Array { element, len: *len })
            }
            ast::TypeExprKind::FnPointer { params, result } => {
                let params = params
                    .iter()
                    .map(|p| self.resolve_type(module_index, p))
                    .collect::<TcResult<Vec<_>>>()?;
                let result = self.resolve_type(module_index, result)?;
                self.program.types.intern(Type::Function {
                    params,
                    result,
                    abi: ast::Abi::OCore,
                })
            }
        };
        Ok(resolved)
    }

    fn resolve_path(&self, module_index: usize, path: &[String]) -> String {
        let scope = &self.scopes[module_index];
        if path.len() == 1 {
            if let Some(import) = scope.imports.get(&path[0]) {
                return import.clone();
            }
            qualify(&scope.name, &path[0])
        } else if let Some(import) = scope.imports.get(&path[0]) {
            format!("{}::{}", import, path[1..].join("::"))
        } else {
            let absolute = path_string(path);
            if path_is_known(&self.program, &absolute) {
                absolute
            } else {
                qualify(&scope.name, &absolute)
            }
        }
    }

    fn resolve_enum_variant(&self, qualified: &str) -> Option<(EnumId, usize)> {
        let (type_name, variant_name) = qualified.rsplit_once("::")?;
        let Symbol::Type(ty) = self.program.symbols.get(type_name)? else {
            return None;
        };
        let Type::Enum(id) = self.program.types.types[*ty] else {
            return None;
        };
        let variant = self.program.types.enums[id]
            .variants
            .iter()
            .position(|v| v.name == variant_name)?;
        Some((id, variant))
    }

    fn type_for_enum(&self, enum_id: EnumId) -> TypeId {
        self.program
            .types
            .types
            .iter()
            .position(|t| *t == Type::Enum(enum_id))
            .unwrap()
    }

    fn ensure_name_free(&self, qn: &str, module_index: usize, span: Span) -> TcResult<()> {
        if self.program.symbols.contains_key(qn) {
            Err(diag(
                &self.scopes[module_index].file,
                span,
                format!("duplicate item `{qn}`"),
            ))
        } else {
            Ok(())
        }
    }

    fn layout_diag(&self, ty: TypeId, message: &str) -> Diagnostic {
        diag(
            "<type>",
            Span::default(),
            format!("{}: {message}", self.program.types.name(ty)),
        )
    }
}

// Private helper used only during recursive layout. Kept here rather than in
// the public TypeContext API because placeholder lookup is a checker concern.
trait TypeContextLookup {
    fn interned_type_for_struct(&self, id: StructId) -> Option<TypeId>;
}

impl TypeContextLookup for TypeContext {
    fn interned_type_for_struct(&self, id: StructId) -> Option<TypeId> {
        self.types.iter().position(|t| *t == Type::Struct(id))
    }
}

struct BodyChecker<'a> {
    program: &'a mut HirProgram,
    scope: &'a ModuleScope,
    locals: &'a mut Vec<HirLocal>,
    scopes: Vec<HashMap<String, LocalId>>,
    return_type: TypeId,
    unsafe_depth: usize,
    loop_depth: usize,
}

impl<'a> BodyChecker<'a> {
    fn new(
        program: &'a mut HirProgram,
        scope: &'a ModuleScope,
        _function_id: FunctionId,
        locals: &'a mut Vec<HirLocal>,
        return_type: TypeId,
        unsafe_function: bool,
    ) -> Self {
        let params = locals
            .iter()
            .enumerate()
            .map(|(id, local)| (local.name.clone(), id))
            .collect();
        Self {
            program,
            scope,
            locals,
            scopes: vec![params],
            return_type,
            unsafe_depth: usize::from(unsafe_function),
            loop_depth: 0,
        }
    }

    fn check_block(&mut self, block: &ast::Block) -> TcResult<HirBlock> {
        self.scopes.push(HashMap::new());
        let result = (|| {
            let mut stmts = Vec::new();
            for stmt in &block.stmts {
                stmts.push(self.check_stmt(stmt)?);
            }
            Ok(HirBlock {
                stmts,
                span: block.span,
            })
        })();
        self.scopes.pop();
        result
    }

    fn check_stmt(&mut self, stmt: &ast::Stmt) -> TcResult<HirStmt> {
        let kind = match &stmt.kind {
            ast::StmtKind::Let {
                mutable,
                name,
                ty,
                init,
            } => {
                if self.scopes.last().unwrap().contains_key(name) {
                    return Err(self.error(stmt.span, format!("duplicate local `{name}`")));
                }
                let declared = ty.as_ref().map(|ty| self.resolve_type(ty)).transpose()?;
                let init = self.check_expr(init, declared)?;
                let local_ty = declared.unwrap_or(init.ty);
                self.expect_assignable(init.ty, local_ty, init.span)?;
                let id = self.locals.len();
                self.locals.push(HirLocal {
                    name: name.clone(),
                    ty: local_ty,
                    mutable: *mutable,
                    parameter: false,
                    span: stmt.span,
                });
                self.scopes.last_mut().unwrap().insert(name.clone(), id);
                HirStmtKind::Let { local: id, init }
            }
            ast::StmtKind::Expr(expr) => HirStmtKind::Expr(self.check_expr(expr, None)?),
            ast::StmtKind::If {
                condition,
                then_block,
                else_block,
            } => {
                let bool_ty = self.program.types.primitive("bool").unwrap();
                let condition = self.check_expr(condition, Some(bool_ty))?;
                self.expect_assignable(condition.ty, bool_ty, condition.span)?;
                HirStmtKind::If {
                    condition,
                    then_block: self.check_block(then_block)?,
                    else_block: else_block
                        .as_ref()
                        .map(|b| self.check_block(b))
                        .transpose()?,
                }
            }
            ast::StmtKind::While { condition, body } => {
                let bool_ty = self.program.types.primitive("bool").unwrap();
                let condition = self.check_expr(condition, Some(bool_ty))?;
                self.expect_assignable(condition.ty, bool_ty, condition.span)?;
                self.loop_depth += 1;
                let body = self.check_block(body);
                self.loop_depth -= 1;
                HirStmtKind::While {
                    condition,
                    body: body?,
                }
            }
            ast::StmtKind::Loop(body) => {
                self.loop_depth += 1;
                let body = self.check_block(body);
                self.loop_depth -= 1;
                HirStmtKind::Loop(body?)
            }
            ast::StmtKind::Unsafe(body) => {
                self.unsafe_depth += 1;
                let body = self.check_block(body);
                self.unsafe_depth -= 1;
                HirStmtKind::Unsafe(body?)
            }
            ast::StmtKind::Return(value) => {
                let value = value
                    .as_ref()
                    .map(|v| self.check_expr(v, Some(self.return_type)))
                    .transpose()?;
                let void = self.program.types.primitive("void").unwrap();
                match &value {
                    Some(value) => {
                        self.expect_assignable(value.ty, self.return_type, value.span)?
                    }
                    None if self.return_type != void => {
                        return Err(self.error(
                            stmt.span,
                            format!(
                                "return value of type `{}` required",
                                self.program.types.name(self.return_type)
                            ),
                        ))
                    }
                    None => {}
                }
                HirStmtKind::Return(value)
            }
            ast::StmtKind::Break => {
                if self.loop_depth == 0 {
                    return Err(self.error(stmt.span, "`break` outside a loop"));
                }
                HirStmtKind::Break
            }
            ast::StmtKind::Continue => {
                if self.loop_depth == 0 {
                    return Err(self.error(stmt.span, "`continue` outside a loop"));
                }
                HirStmtKind::Continue
            }
        };
        Ok(HirStmt {
            kind,
            span: stmt.span,
        })
    }

    fn check_expr(&mut self, expr: &ast::Expr, expected: Option<TypeId>) -> TcResult<HirExpr> {
        let (kind, ty) = match &expr.kind {
            ast::ExprKind::Integer(value) => {
                let ty = expected
                    .filter(|ty| self.program.types.is_integer(*ty))
                    .unwrap_or_else(|| self.program.types.primitive("u64").unwrap());
                self.ensure_integer_fits(*value, ty, expr.span)?;
                (HirExprKind::Constant(HirConstValue::Integer(*value)), ty)
            }
            ast::ExprKind::Byte(value) => (
                HirExprKind::Constant(HirConstValue::Integer(*value as u64)),
                self.program.types.primitive("u8").unwrap(),
            ),
            ast::ExprKind::Bool(value) => (
                HirExprKind::Constant(HirConstValue::Bool(*value)),
                self.program.types.primitive("bool").unwrap(),
            ),
            ast::ExprKind::String(_) => {
                return Err(self.error(
                    expr.span,
                    "runtime strings are not implicit in freestanding O-core; use a byte string",
                ))
            }
            ast::ExprKind::ByteString(bytes) => {
                let u8_ty = self.program.types.primitive("u8").unwrap();
                let ty = self.program.types.intern(Type::Array {
                    element: u8_ty,
                    len: bytes.len() as u64,
                });
                (
                    HirExprKind::Constant(HirConstValue::Bytes(bytes.clone())),
                    ty,
                )
            }
            ast::ExprKind::Path(path) => return self.check_path(expr.span, path),
            ast::ExprKind::Array(values) => {
                let expected_element = expected.and_then(|ty| match self.program.types.types[ty] {
                    Type::Array { element, len } if len == values.len() as u64 => Some(element),
                    _ => None,
                });
                let mut checked = Vec::new();
                let mut element_ty = expected_element;
                for value in values {
                    let value = self.check_expr(value, element_ty)?;
                    if let Some(ty) = element_ty {
                        self.expect_assignable(value.ty, ty, value.span)?;
                    } else {
                        element_ty = Some(value.ty);
                    }
                    checked.push(value);
                }
                let element = element_ty
                    .ok_or_else(|| self.error(expr.span, "cannot infer type of empty array"))?;
                let ty = self.program.types.intern(Type::Array {
                    element,
                    len: checked.len() as u64,
                });
                (HirExprKind::Array(checked), ty)
            }
            ast::ExprKind::ArrayRepeat { value, len } => {
                let expected_element = expected.and_then(|ty| match self.program.types.types[ty] {
                    Type::Array {
                        element,
                        len: expected_len,
                    } if expected_len == *len => Some(element),
                    _ => None,
                });
                let value = self.check_expr(value, expected_element)?;
                let ty = self.program.types.intern(Type::Array {
                    element: value.ty,
                    len: *len,
                });
                (
                    HirExprKind::ArrayRepeat {
                        value: Box::new(value),
                        len: *len,
                    },
                    ty,
                )
            }
            ast::ExprKind::Struct { path, fields } => {
                let qn = self.resolve_path(path);
                let Some(Symbol::Type(ty)) = self.program.symbols.get(&qn).copied() else {
                    return Err(self.error(expr.span, format!("unknown struct `{qn}`")));
                };
                let Type::Struct(struct_id) = self.program.types.types[ty] else {
                    return Err(self.error(expr.span, format!("`{qn}` is not a struct")));
                };
                let def = self.program.types.structs[struct_id].clone();
                if fields.len() != def.fields.len() {
                    return Err(
                        self.error(expr.span, "struct initializer must specify every field")
                    );
                }
                let mut ordered = Vec::new();
                for field in &def.fields {
                    let Some((_, value)) = fields.iter().find(|(n, _)| n == &field.name) else {
                        return Err(
                            self.error(expr.span, format!("missing field `{}`", field.name))
                        );
                    };
                    let value = self.check_expr(value, Some(field.ty))?;
                    self.expect_assignable(value.ty, field.ty, value.span)?;
                    ordered.push(value);
                }
                (
                    HirExprKind::Struct {
                        struct_id,
                        fields: ordered,
                    },
                    ty,
                )
            }
            ast::ExprKind::Unary { op, operand } => {
                let operand = self.check_expr(operand, None)?;
                let ty = match op {
                    ast::UnaryOp::Neg => {
                        if !self.program.types.is_integer(operand.ty) {
                            return Err(self.error(expr.span, "unary `-` requires an integer"));
                        }
                        operand.ty
                    }
                    ast::UnaryOp::Not => {
                        let bool_ty = self.program.types.primitive("bool").unwrap();
                        self.expect_assignable(operand.ty, bool_ty, operand.span)?;
                        bool_ty
                    }
                    ast::UnaryOp::BitNot => {
                        if !self.program.types.is_integer(operand.ty) {
                            return Err(self.error(expr.span, "unary `~` requires an integer"));
                        }
                        operand.ty
                    }
                    ast::UnaryOp::Deref => {
                        self.require_unsafe(expr.span, "raw pointer dereference")?;
                        match self.program.types.types[operand.ty] {
                            Type::Pointer { pointee, .. } => pointee,
                            _ => {
                                return Err(self.error(expr.span, "cannot dereference non-pointer"))
                            }
                        }
                    }
                    ast::UnaryOp::AddressOf { mutable } => {
                        self.ensure_place(&operand, *mutable)?;
                        self.program.types.intern(Type::Pointer {
                            mutable: *mutable,
                            pointee: operand.ty,
                        })
                    }
                };
                (
                    HirExprKind::Unary {
                        op: *op,
                        operand: Box::new(operand),
                    },
                    ty,
                )
            }
            ast::ExprKind::Binary { op, lhs, rhs } => {
                let comparison_or_logic = matches!(
                    op,
                    ast::BinaryOp::LogicalAnd
                        | ast::BinaryOp::LogicalOr
                        | ast::BinaryOp::Eq
                        | ast::BinaryOp::NotEq
                        | ast::BinaryOp::Less
                        | ast::BinaryOp::LessEq
                        | ast::BinaryOp::Greater
                        | ast::BinaryOp::GreaterEq
                );
                let lhs =
                    self.check_expr(lhs, (!comparison_or_logic).then_some(expected).flatten())?;
                let rhs_expected =
                    if matches!(self.program.types.types[lhs.ty], Type::Pointer { .. })
                        && matches!(op, ast::BinaryOp::Add | ast::BinaryOp::Sub)
                    {
                        Some(self.program.types.primitive("usize").unwrap())
                    } else {
                        Some(lhs.ty)
                    };
                let rhs = self.check_expr(rhs, rhs_expected)?;
                let bool_ty = self.program.types.primitive("bool").unwrap();
                let result_ty = match op {
                    ast::BinaryOp::LogicalAnd | ast::BinaryOp::LogicalOr => {
                        self.expect_assignable(lhs.ty, bool_ty, lhs.span)?;
                        self.expect_assignable(rhs.ty, bool_ty, rhs.span)?;
                        bool_ty
                    }
                    ast::BinaryOp::Eq
                    | ast::BinaryOp::NotEq
                    | ast::BinaryOp::Less
                    | ast::BinaryOp::LessEq
                    | ast::BinaryOp::Greater
                    | ast::BinaryOp::GreaterEq => {
                        self.expect_assignable(rhs.ty, lhs.ty, rhs.span)?;
                        if !self.program.types.is_scalar(lhs.ty) {
                            return Err(
                                self.error(expr.span, "comparison requires scalar operands")
                            );
                        }
                        bool_ty
                    }
                    _ => {
                        if matches!(self.program.types.types[lhs.ty], Type::Pointer { .. })
                            && matches!(op, ast::BinaryOp::Add | ast::BinaryOp::Sub)
                            && self.program.types.is_integer(rhs.ty)
                        {
                            self.require_unsafe(expr.span, "raw pointer arithmetic")?;
                            lhs.ty
                        } else {
                            self.expect_assignable(rhs.ty, lhs.ty, rhs.span)?;
                            if !self.program.types.is_integer(lhs.ty) {
                                return Err(
                                    self.error(expr.span, "operator requires integer operands")
                                );
                            }
                            lhs.ty
                        }
                    }
                };
                (
                    HirExprKind::Binary {
                        op: *op,
                        lhs: Box::new(lhs),
                        rhs: Box::new(rhs),
                    },
                    result_ty,
                )
            }
            ast::ExprKind::Assign { op, target, value } => {
                let target = self.check_expr(target, None)?;
                self.ensure_place(&target, true)?;
                let value = self.check_expr(value, Some(target.ty))?;
                self.expect_assignable(value.ty, target.ty, value.span)?;
                if let Some(op) = op {
                    if !self.program.types.is_integer(target.ty) {
                        return Err(self.error(
                            expr.span,
                            format!("compound operator `{op:?}` requires integer target"),
                        ));
                    }
                }
                let void = self.program.types.primitive("void").unwrap();
                (
                    HirExprKind::Assign {
                        op: *op,
                        target: Box::new(target),
                        value: Box::new(value),
                    },
                    void,
                )
            }
            ast::ExprKind::Call { callee, args } => {
                return self.check_call(expr.span, callee, args)
            }
            ast::ExprKind::Field { base, name } => {
                let base = self.check_expr(base, None)?;
                let Type::Struct(id) = self.program.types.types[base.ty] else {
                    return Err(self.error(expr.span, "field access requires a struct value"));
                };
                let Some((field, def)) = self.program.types.structs[id]
                    .fields
                    .iter()
                    .enumerate()
                    .find(|(_, f)| f.name == *name)
                else {
                    return Err(self.error(expr.span, format!("unknown field `{name}`")));
                };
                let (field_ty, offset) = (def.ty, def.offset);
                (
                    HirExprKind::Field {
                        base: Box::new(base),
                        field,
                        offset,
                    },
                    field_ty,
                )
            }
            ast::ExprKind::Index { base, index } => {
                let base = self.check_expr(base, None)?;
                let usize_ty = self.program.types.primitive("usize").unwrap();
                let index = self.check_expr(index, Some(usize_ty))?;
                if !self.program.types.is_integer(index.ty) {
                    return Err(self.error(index.span, "array index must be an integer"));
                }
                let ty = match self.program.types.types[base.ty] {
                    Type::Array { element, .. } => element,
                    Type::Pointer { pointee, .. } => {
                        self.require_unsafe(expr.span, "raw pointer indexing")?;
                        pointee
                    }
                    _ => return Err(self.error(base.span, "indexing requires an array or pointer")),
                };
                (
                    HirExprKind::Index {
                        base: Box::new(base),
                        index: Box::new(index),
                    },
                    ty,
                )
            }
            ast::ExprKind::Cast { value, ty } => {
                let value = self.check_expr(value, None)?;
                let to = self.resolve_type(ty)?;
                if !self.program.types.is_scalar(value.ty) || !self.program.types.is_scalar(to) {
                    return Err(self.error(expr.span, "casts are limited to scalar types"));
                }
                let pointer_crossing =
                    matches!(self.program.types.types[value.ty], Type::Pointer { .. })
                        != matches!(self.program.types.types[to], Type::Pointer { .. });
                if pointer_crossing {
                    self.require_unsafe(expr.span, "pointer/integer cast")?;
                }
                (
                    HirExprKind::Cast {
                        value: Box::new(value),
                        to,
                    },
                    to,
                )
            }
            ast::ExprKind::Asm(asm) => {
                self.require_unsafe(expr.span, "inline assembly")?;
                let mut operands = Vec::new();
                for operand in &asm.operands {
                    operands.push(match operand {
                        ast::AsmOperand::In { register, value } => HirAsmOperand::In {
                            register: register.clone(),
                            value: self.check_expr(value, None)?,
                        },
                        ast::AsmOperand::Out { register, target } => {
                            let target = self.check_expr(target, None)?;
                            self.ensure_place(&target, true)?;
                            HirAsmOperand::Out {
                                register: register.clone(),
                                target,
                            }
                        }
                        ast::AsmOperand::InOut {
                            register,
                            input,
                            output,
                        } => {
                            let input = self.check_expr(input, None)?;
                            let output = self.check_expr(output, Some(input.ty))?;
                            self.ensure_place(&output, true)?;
                            self.expect_assignable(output.ty, input.ty, output.span)?;
                            HirAsmOperand::InOut {
                                register: register.clone(),
                                input,
                                output,
                            }
                        }
                    });
                }
                validate_asm_options(&asm.options).map_err(|m| self.error(expr.span, m))?;
                (
                    HirExprKind::Asm(HirAsm {
                        template: asm.template.clone(),
                        operands,
                        options: asm.options.clone(),
                    }),
                    self.program.types.primitive("void").unwrap(),
                )
            }
        };
        if let Some(expected) = expected {
            self.expect_assignable(ty, expected, expr.span)?;
        }
        Ok(HirExpr {
            kind,
            ty,
            span: expr.span,
        })
    }

    fn check_path(&mut self, span: Span, path: &[String]) -> TcResult<HirExpr> {
        if path.len() == 1 {
            if let Some(local) = self.lookup_local(&path[0]) {
                return Ok(HirExpr {
                    kind: HirExprKind::Local(local),
                    ty: self.locals[local].ty,
                    span,
                });
            }
            if let Some(order) = memory_order(&path[0]) {
                return Ok(HirExpr {
                    kind: HirExprKind::Constant(HirConstValue::Integer(order as u64)),
                    ty: self.program.types.primitive("u8").unwrap(),
                    span,
                });
            }
        }
        let qn = self.resolve_path(path);
        match self.program.symbols.get(&qn).copied() {
            Some(Symbol::Function(id)) => {
                let function = &self.program.functions[id];
                let ty = self.program.types.intern(Type::Function {
                    params: function
                        .params
                        .iter()
                        .map(|p| function.locals[*p].ty)
                        .collect(),
                    result: function.result,
                    abi: function.abi.clone(),
                });
                Ok(HirExpr {
                    kind: HirExprKind::Function(id),
                    ty,
                    span,
                })
            }
            Some(Symbol::Static(id)) => {
                if self.program.statics[id].mutable {
                    self.require_unsafe(span, "access to mutable static")?;
                }
                Ok(HirExpr {
                    kind: HirExprKind::Static(id),
                    ty: self.program.statics[id].ty,
                    span,
                })
            }
            Some(Symbol::Const(id)) => Ok(HirExpr {
                kind: HirExprKind::Constant(self.program.consts[id].value.clone()),
                ty: self.program.consts[id].ty,
                span,
            }),
            Some(Symbol::Type(_)) => Err(self.error(span, format!("type `{qn}` is not a value"))),
            None => {
                if let Some((enum_id, variant)) = resolve_enum_variant(self.program, &qn) {
                    let def = &self.program.types.enums[enum_id].variants[variant];
                    if !def.payload.is_empty() {
                        return Err(self.error(span, "payload enum variant must be called"));
                    }
                    let ty = type_for_enum(self.program, enum_id);
                    Ok(HirExpr {
                        kind: HirExprKind::EnumVariant {
                            enum_id,
                            variant,
                            args: Vec::new(),
                        },
                        ty,
                        span,
                    })
                } else {
                    Err(self.error(span, format!("unknown value `{qn}`")))
                }
            }
        }
    }

    fn check_call(
        &mut self,
        span: Span,
        callee: &ast::Expr,
        args: &[ast::Expr],
    ) -> TcResult<HirExpr> {
        let path = match &callee.kind {
            ast::ExprKind::Path(path) => Some(path.as_slice()),
            _ => None,
        };
        if let Some(path) = path {
            if path.len() == 1 {
                if let Some(intrinsic) = intrinsic(&path[0]) {
                    return self.check_intrinsic(span, intrinsic, args);
                }
            }
            let qn = self.resolve_path(path);
            if let Some((enum_id, variant)) = resolve_enum_variant(self.program, &qn) {
                let payload = self.program.types.enums[enum_id].variants[variant]
                    .payload
                    .clone();
                if payload.len() != args.len() {
                    return Err(self.error(span, "wrong number of enum payload arguments"));
                }
                let mut checked = Vec::new();
                for (arg, ty) in args.iter().zip(payload) {
                    let arg = self.check_expr(arg, Some(ty))?;
                    self.expect_assignable(arg.ty, ty, arg.span)?;
                    checked.push(arg);
                }
                return Ok(HirExpr {
                    kind: HirExprKind::EnumVariant {
                        enum_id,
                        variant,
                        args: checked,
                    },
                    ty: type_for_enum(self.program, enum_id),
                    span,
                });
            }
        }
        let callee = self.check_expr(callee, None)?;
        let HirExprKind::Function(function_id) = callee.kind else {
            return Err(self.error(span, "v0.1 supports direct function calls only"));
        };
        let function = self.program.functions[function_id].clone();
        if function.unsafe_ {
            self.require_unsafe(
                span,
                format!("call to unsafe function `{}`", function.qualified_name),
            )?;
        }
        if function.params.len() != args.len() {
            return Err(self.error(
                span,
                format!(
                    "function `{}` takes {} arguments, got {}",
                    function.qualified_name,
                    function.params.len(),
                    args.len()
                ),
            ));
        }
        let mut checked = Vec::new();
        for (arg, local) in args.iter().zip(&function.params) {
            let expected = function.locals[*local].ty;
            let arg = self.check_expr(arg, Some(expected))?;
            self.expect_assignable(arg.ty, expected, arg.span)?;
            checked.push(arg);
        }
        Ok(HirExpr {
            kind: HirExprKind::Call {
                function: function_id,
                args: checked,
            },
            ty: function.result,
            span,
        })
    }

    fn check_intrinsic(
        &mut self,
        span: Span,
        intrinsic: Intrinsic,
        args: &[ast::Expr],
    ) -> TcResult<HirExpr> {
        self.require_unsafe(span, "hardware/runtime intrinsic")?;
        let u8_ty = self.program.types.primitive("u8").unwrap();
        let u16_ty = self.program.types.primitive("u16").unwrap();
        let u32_ty = self.program.types.primitive("u32").unwrap();
        let u64_ty = self.program.types.primitive("u64").unwrap();
        let usize_ty = self.program.types.primitive("usize").unwrap();
        let void_ty = self.program.types.primitive("void").unwrap();
        let (arg_types, result) = match intrinsic {
            Intrinsic::In8 => (vec![u16_ty], u8_ty),
            Intrinsic::In16 => (vec![u16_ty], u16_ty),
            Intrinsic::In32 => (vec![u16_ty], u32_ty),
            Intrinsic::Out8 => (vec![u16_ty, u8_ty], void_ty),
            Intrinsic::Out16 => (vec![u16_ty, u16_ty], void_ty),
            Intrinsic::Out32 => (vec![u16_ty, u32_ty], void_ty),
            Intrinsic::EnableInterrupts | Intrinsic::DisableInterrupts | Intrinsic::Halt => {
                (vec![], void_ty)
            }
            Intrinsic::InvalidatePage => (vec![usize_ty], void_ty),
            Intrinsic::Syscall(n) => (vec![u64_ty; n as usize + 1], u64_ty),
            Intrinsic::VolatileLoad => {
                if args.len() != 1 {
                    return Err(self.error(span, "volatile_load takes one pointer"));
                }
                let arg = self.check_expr(&args[0], None)?;
                let Type::Pointer { pointee, .. } = self.program.types.types[arg.ty] else {
                    return Err(self.error(arg.span, "volatile_load requires a pointer"));
                };
                return Ok(HirExpr {
                    kind: HirExprKind::Intrinsic {
                        intrinsic,
                        args: vec![arg],
                    },
                    ty: pointee,
                    span,
                });
            }
            Intrinsic::VolatileStore => {
                if args.len() != 2 {
                    return Err(self.error(span, "volatile_store takes a pointer and value"));
                }
                let ptr = self.check_expr(&args[0], None)?;
                let Type::Pointer {
                    mutable: true,
                    pointee,
                } = self.program.types.types[ptr.ty]
                else {
                    return Err(self.error(ptr.span, "volatile_store requires a mutable pointer"));
                };
                let value = self.check_expr(&args[1], Some(pointee))?;
                self.expect_assignable(value.ty, pointee, value.span)?;
                return Ok(HirExpr {
                    kind: HirExprKind::Intrinsic {
                        intrinsic,
                        args: vec![ptr, value],
                    },
                    ty: void_ty,
                    span,
                });
            }
            Intrinsic::AtomicLoad
            | Intrinsic::AtomicStore
            | Intrinsic::AtomicExchange
            | Intrinsic::AtomicCompareExchange
            | Intrinsic::AtomicFetchAdd => return self.check_atomic(span, intrinsic, args),
        };
        if arg_types.len() != args.len() {
            return Err(self.error(
                span,
                format!(
                    "intrinsic takes {} arguments, got {}",
                    arg_types.len(),
                    args.len()
                ),
            ));
        }
        let mut checked = Vec::new();
        for (arg, expected) in args.iter().zip(arg_types) {
            let arg = self.check_expr(arg, Some(expected))?;
            self.expect_assignable(arg.ty, expected, arg.span)?;
            checked.push(arg);
        }
        Ok(HirExpr {
            kind: HirExprKind::Intrinsic {
                intrinsic,
                args: checked,
            },
            ty: result,
            span,
        })
    }

    fn check_atomic(
        &mut self,
        span: Span,
        intrinsic: Intrinsic,
        args: &[ast::Expr],
    ) -> TcResult<HirExpr> {
        let (value_count, order_count) = match intrinsic {
            Intrinsic::AtomicLoad => (0, 1),
            Intrinsic::AtomicStore | Intrinsic::AtomicExchange | Intrinsic::AtomicFetchAdd => {
                (1, 1)
            }
            Intrinsic::AtomicCompareExchange => (2, 2),
            _ => unreachable!(),
        };
        let expected_count = 1 + value_count + order_count;
        if args.len() != expected_count {
            return Err(self.error(
                span,
                format!("atomic intrinsic takes {expected_count} arguments"),
            ));
        }
        let ptr = self.check_expr(&args[0], None)?;
        let Type::Pointer { mutable, pointee } = self.program.types.types[ptr.ty] else {
            return Err(self.error(ptr.span, "atomic operation requires a pointer"));
        };
        if !self.program.types.is_integer(pointee)
            || !matches!(self.program.types.layout(pointee).size, 1 | 2 | 4 | 8)
        {
            return Err(self.error(ptr.span, "atomic pointee must be a 1/2/4/8-byte integer"));
        }
        if !mutable && !matches!(intrinsic, Intrinsic::AtomicLoad) {
            return Err(self.error(ptr.span, "mutating atomic operation requires `*mut`"));
        }
        let mut checked = vec![ptr];
        for arg in &args[1..1 + value_count] {
            checked.push(self.check_expr(arg, Some(pointee))?);
        }
        for arg in &args[1 + value_count..] {
            let order = self.check_expr(arg, Some(self.program.types.primitive("u8").unwrap()))?;
            let HirExprKind::Constant(HirConstValue::Integer(order_value)) = order.kind else {
                return Err(self.error(
                    order.span,
                    "memory ordering must be a named ordering constant",
                ));
            };
            validate_memory_order(intrinsic, order_value as u8)
                .map_err(|m| self.error(order.span, m))?;
            checked.push(HirExpr {
                kind: HirExprKind::Constant(HirConstValue::Integer(order_value)),
                ..order
            });
        }
        let result = if matches!(intrinsic, Intrinsic::AtomicStore) {
            self.program.types.primitive("void").unwrap()
        } else {
            pointee
        };
        Ok(HirExpr {
            kind: HirExprKind::Intrinsic {
                intrinsic,
                args: checked,
            },
            ty: result,
            span,
        })
    }

    fn ensure_place(&self, expr: &HirExpr, mutable: bool) -> TcResult<()> {
        let is_mutable = match &expr.kind {
            HirExprKind::Local(id) => self.locals[*id].mutable,
            HirExprKind::Static(id) => self.program.statics[*id].mutable,
            HirExprKind::Unary {
                op: ast::UnaryOp::Deref,
                operand,
            } => matches!(
                self.program.types.types[operand.ty],
                Type::Pointer { mutable: true, .. }
            ),
            HirExprKind::Field { base, .. } | HirExprKind::Index { base, .. } => {
                self.ensure_place(base, mutable).is_ok()
            }
            _ => false,
        };
        if mutable && !is_mutable {
            Err(self.error(expr.span, "target is not a mutable place"))
        } else if !mutable
            && !matches!(
                expr.kind,
                HirExprKind::Local(_)
                    | HirExprKind::Static(_)
                    | HirExprKind::Unary {
                        op: ast::UnaryOp::Deref,
                        ..
                    }
                    | HirExprKind::Field { .. }
                    | HirExprKind::Index { .. }
            )
        {
            Err(self.error(expr.span, "expression is not addressable"))
        } else {
            Ok(())
        }
    }

    fn lookup_local(&self, name: &str) -> Option<LocalId> {
        self.scopes
            .iter()
            .rev()
            .find_map(|scope| scope.get(name).copied())
    }

    fn resolve_type(&mut self, ty: &ast::TypeExpr) -> TcResult<TypeId> {
        match &ty.kind {
            ast::TypeExprKind::Named(path) => {
                if path.len() == 1 {
                    if let Some(ty) = self.program.types.primitive(&path[0]) {
                        return Ok(ty);
                    }
                }
                let qn = self.resolve_path(path);
                match self.program.symbols.get(&qn) {
                    Some(Symbol::Type(ty)) => Ok(*ty),
                    _ => Err(self.error(ty.span, format!("unknown type `{qn}`"))),
                }
            }
            ast::TypeExprKind::Pointer { mutable, pointee } => {
                let pointee = self.resolve_type(pointee)?;
                Ok(self.program.types.intern(Type::Pointer {
                    mutable: *mutable,
                    pointee,
                }))
            }
            ast::TypeExprKind::Array { element, len } => {
                let element = self.resolve_type(element)?;
                Ok(self
                    .program
                    .types
                    .intern(Type::Array { element, len: *len }))
            }
            ast::TypeExprKind::FnPointer { params, result } => {
                let params = params
                    .iter()
                    .map(|p| self.resolve_type(p))
                    .collect::<TcResult<Vec<_>>>()?;
                let result = self.resolve_type(result)?;
                Ok(self.program.types.intern(Type::Function {
                    params,
                    result,
                    abi: ast::Abi::OCore,
                }))
            }
        }
    }

    fn resolve_path(&self, path: &[String]) -> String {
        if path.len() == 1 {
            if let Some(import) = self.scope.imports.get(&path[0]) {
                import.clone()
            } else {
                qualify(&self.scope.name, &path[0])
            }
        } else if let Some(import) = self.scope.imports.get(&path[0]) {
            format!("{}::{}", import, path[1..].join("::"))
        } else {
            let absolute = path_string(path);
            if path_is_known(self.program, &absolute) {
                absolute
            } else {
                qualify(&self.scope.name, &absolute)
            }
        }
    }

    fn expect_assignable(&self, actual: TypeId, expected: TypeId, span: Span) -> TcResult<()> {
        if actual == expected || matches!(self.program.types.types[actual], Type::Never) {
            Ok(())
        } else {
            Err(self.error(
                span,
                format!(
                    "expected `{}`, found `{}`",
                    self.program.types.name(expected),
                    self.program.types.name(actual)
                ),
            ))
        }
    }

    fn ensure_integer_fits(&self, value: u64, ty: TypeId, span: Span) -> TcResult<()> {
        let max = match self.program.types.types[ty] {
            Type::Int {
                signed: false,
                bits: 64,
            }
            | Type::Usize
            | Type::Isize
            | Type::Int {
                signed: true,
                bits: 64,
            } => u64::MAX,
            Type::Int {
                signed: false,
                bits,
            } => (1u64 << bits) - 1,
            Type::Int { signed: true, bits } => (1u64 << (bits - 1)) - 1,
            _ => return Err(self.error(span, "integer literal used with non-integer type")),
        };
        if value > max {
            Err(self.error(
                span,
                format!(
                    "literal `{value}` does not fit `{}`",
                    self.program.types.name(ty)
                ),
            ))
        } else {
            Ok(())
        }
    }

    fn require_unsafe(&self, span: Span, operation: impl AsRef<str>) -> TcResult<()> {
        if self.unsafe_depth == 0 {
            Err(self.error(
                span,
                format!("{} requires an explicit unsafe block", operation.as_ref()),
            ))
        } else {
            Ok(())
        }
    }

    fn error(&self, span: Span, message: impl Into<String>) -> Diagnostic {
        diag(&self.scope.file, span, message)
    }
}

fn parse_attrs(file: &str, item: &ast::Item) -> TcResult<ItemAttrs> {
    let mut out = ItemAttrs::default();
    let mut seen = HashSet::new();
    for attr in &item.attrs {
        if !seen.insert(attr.name.clone()) {
            return Err(diag(
                file,
                attr.span,
                format!("duplicate attribute `@{}`", attr.name),
            ));
        }
        match attr.name.as_str() {
            "export" => no_args(file, attr, &mut out.export)?,
            "no_mangle" => no_args(file, attr, &mut out.no_mangle)?,
            "used" => no_args(file, attr, &mut out.used)?,
            "packed" => no_args(file, attr, &mut out.packed)?,
            "interrupt" => no_args(file, attr, &mut out.interrupt)?,
            "naked" => no_args(file, attr, &mut out.naked)?,
            "unsafe_linkage" => no_args(file, attr, &mut out.unsafe_linkage)?,
            "link_section" => match attr.args.as_slice() {
                [ast::AttrArg::String(section)] if !section.is_empty() => {
                    out.link_section = Some(section.clone())
                }
                _ => {
                    return Err(diag(
                        file,
                        attr.span,
                        "@link_section requires one nonempty string",
                    ))
                }
            },
            "align" => match attr.args.as_slice() {
                [ast::AttrArg::Integer(value)] if value.is_power_of_two() && *value <= 4096 => {
                    out.align = Some(*value)
                }
                _ => {
                    return Err(diag(
                        file,
                        attr.span,
                        "@align requires a power of two <= 4096",
                    ))
                }
            },
            other => {
                return Err(diag(
                    file,
                    attr.span,
                    format!("unknown attribute `@{other}`"),
                ))
            }
        }
    }
    Ok(out)
}

fn no_args(file: &str, attr: &ast::Attribute, target: &mut bool) -> TcResult<()> {
    if !attr.args.is_empty() {
        Err(diag(
            file,
            attr.span,
            format!("@{} takes no arguments", attr.name),
        ))
    } else {
        *target = true;
        Ok(())
    }
}

struct FunctionAttrContext<'a> {
    abi: &'a ast::Abi,
    params: &'a [TypeId],
    result: TypeId,
    unsafe_: bool,
}

fn validate_function_attrs(
    file: &str,
    span: Span,
    attrs: &ItemAttrs,
    function: FunctionAttrContext<'_>,
    types: &TypeContext,
) -> TcResult<()> {
    if attrs.packed || attrs.used || attrs.unsafe_linkage {
        return Err(diag(
            file,
            span,
            "struct/static-only attribute applied to function",
        ));
    }
    if attrs.interrupt {
        if !function.unsafe_ {
            return Err(diag(
                file,
                span,
                "@interrupt function must be declared unsafe",
            ));
        }
        if !function.params.is_empty() || !matches!(types.types[function.result], Type::Void) {
            return Err(diag(
                file,
                span,
                "@interrupt function must be `fn() -> void`",
            ));
        }
        if *function.abi != ast::Abi::Interrupt {
            return Err(diag(file, span, "internal interrupt ABI mismatch"));
        }
    }
    if attrs.naked {
        if !function.unsafe_ {
            return Err(diag(file, span, "@naked function must be declared unsafe"));
        }
        if !attrs.no_mangle {
            return Err(diag(
                file,
                span,
                "@naked requires @no_mangle for explicit entry linkage",
            ));
        }
    }
    if *function.abi == ast::Abi::SysV64
        && (function.params.iter().any(|ty| !types.is_scalar(*ty))
            || !matches!(types.types[function.result], Type::Void | Type::Never)
                && !types.is_scalar(function.result))
    {
        return Err(diag(
            file,
            span,
            "aggregate values are forbidden across the v0.1 sysv64 ABI; pass pointers",
        ));
    }
    Ok(())
}

fn validate_asm_options(options: &[String]) -> Result<(), String> {
    let allowed = ["nomem", "readonly", "nostack", "preserves_flags"];
    let mut seen = HashSet::new();
    for option in options {
        if !allowed.contains(&option.as_str()) {
            return Err(format!("unknown assembly option `{option}`"));
        }
        if !seen.insert(option) {
            return Err(format!("duplicate assembly option `{option}`"));
        }
    }
    if seen.contains(&"nomem".to_string()) && seen.contains(&"readonly".to_string()) {
        return Err("asm options `nomem` and `readonly` are redundant".into());
    }
    Ok(())
}

fn validate_memory_order(intrinsic: Intrinsic, order: u8) -> Result<(), &'static str> {
    if order > MemoryOrder::SeqCst as u8 {
        return Err("invalid memory ordering");
    }
    if intrinsic == Intrinsic::AtomicLoad
        && matches!(order, x if x == MemoryOrder::Release as u8 || x == MemoryOrder::AcqRel as u8)
    {
        return Err("atomic load cannot use release or acq_rel ordering");
    }
    if intrinsic == Intrinsic::AtomicStore
        && matches!(order, x if x == MemoryOrder::Acquire as u8 || x == MemoryOrder::AcqRel as u8)
    {
        return Err("atomic store cannot use acquire or acq_rel ordering");
    }
    Ok(())
}

fn memory_order(name: &str) -> Option<MemoryOrder> {
    Some(match name {
        "relaxed" => MemoryOrder::Relaxed,
        "acquire" => MemoryOrder::Acquire,
        "release" => MemoryOrder::Release,
        "acq_rel" => MemoryOrder::AcqRel,
        "seq_cst" => MemoryOrder::SeqCst,
        _ => return None,
    })
}

fn intrinsic(name: &str) -> Option<Intrinsic> {
    Some(match name {
        "volatile_load" => Intrinsic::VolatileLoad,
        "volatile_store" => Intrinsic::VolatileStore,
        "atomic_load" => Intrinsic::AtomicLoad,
        "atomic_store" => Intrinsic::AtomicStore,
        "atomic_exchange" => Intrinsic::AtomicExchange,
        "atomic_compare_exchange" => Intrinsic::AtomicCompareExchange,
        "atomic_fetch_add" => Intrinsic::AtomicFetchAdd,
        "inb" => Intrinsic::In8,
        "inw" => Intrinsic::In16,
        "inl" => Intrinsic::In32,
        "outb" => Intrinsic::Out8,
        "outw" => Intrinsic::Out16,
        "outl" => Intrinsic::Out32,
        "enable_interrupts" => Intrinsic::EnableInterrupts,
        "disable_interrupts" => Intrinsic::DisableInterrupts,
        "halt" => Intrinsic::Halt,
        "invalidate_page" => Intrinsic::InvalidatePage,
        "syscall0" => Intrinsic::Syscall(0),
        "syscall1" => Intrinsic::Syscall(1),
        "syscall2" => Intrinsic::Syscall(2),
        "syscall3" => Intrinsic::Syscall(3),
        "syscall4" => Intrinsic::Syscall(4),
        "syscall5" => Intrinsic::Syscall(5),
        "syscall6" => Intrinsic::Syscall(6),
        _ => return None,
    })
}

fn resolve_enum_variant(program: &HirProgram, qn: &str) -> Option<(EnumId, usize)> {
    let (enum_name, variant_name) = qn.rsplit_once("::")?;
    let Symbol::Type(ty) = program.symbols.get(enum_name)? else {
        return None;
    };
    let Type::Enum(enum_id) = program.types.types[*ty] else {
        return None;
    };
    let variant = program.types.enums[enum_id]
        .variants
        .iter()
        .position(|v| v.name == variant_name)?;
    Some((enum_id, variant))
}

fn path_is_known(program: &HirProgram, path: &str) -> bool {
    if program.symbols.contains_key(path) {
        return true;
    }
    path.rsplit_once("::")
        .and_then(|(parent, _)| program.symbols.get(parent))
        .is_some_and(|symbol| matches!(symbol, Symbol::Type(_)))
}

fn type_for_enum(program: &HirProgram, enum_id: EnumId) -> TypeId {
    program
        .types
        .types
        .iter()
        .position(|t| *t == Type::Enum(enum_id))
        .unwrap()
}

fn is_integer_type(ty: &Type) -> bool {
    matches!(ty, Type::Int { .. } | Type::Usize | Type::Isize)
}

fn item_symbol(module: &str, name: &str, attrs: &ItemAttrs, extern_: bool) -> String {
    if attrs.no_mangle || extern_ {
        name.to_string()
    } else {
        format!("_O_{}__{}", module.replace("::", "__"), name)
    }
}

fn block_terminates(block: &HirBlock) -> bool {
    let Some(last) = block.stmts.last() else {
        return false;
    };
    match &last.kind {
        HirStmtKind::Return(_) | HirStmtKind::Loop(_) => true,
        HirStmtKind::If {
            then_block,
            else_block: Some(else_block),
            ..
        } => block_terminates(then_block) && block_terminates(else_block),
        HirStmtKind::Unsafe(block) => block_terminates(block),
        _ => false,
    }
}

fn path_string(path: &[String]) -> String {
    path.join("::")
}

fn qualify(module: &str, name: &str) -> String {
    format!("{module}::{name}")
}

fn diag(file: &str, span: Span, message: impl Into<String>) -> Diagnostic {
    Diagnostic {
        file: file.to_string(),
        span,
        message: message.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ocore::parser;

    fn checked(source: &str) -> HirProgram {
        let ast = parser::parse("test.oc", source).unwrap();
        check(&[("test.oc".into(), ast)]).unwrap()
    }

    #[test]
    fn computes_struct_and_enum_layouts() {
        let program = checked(
            r#"
module layout;
struct S { a: u8, b: u64, c: u16 }
enum E { none, one(u64), pair(u32, u32) }
"#,
        );
        let s = &program.types.structs[0];
        assert_eq!(
            s.fields.iter().map(|f| f.offset).collect::<Vec<_>>(),
            vec![0, 8, 16]
        );
        assert_eq!(s.layout, Layout { size: 24, align: 8 });
        let e = &program.types.enums[0];
        assert_eq!(e.payload_offset, 8);
        assert_eq!(e.layout, Layout { size: 16, align: 8 });
    }

    #[test]
    fn unsafe_is_enforced() {
        let ast = parser::parse(
            "test.oc",
            r#"
module safety;
fn bad(p: *mut u64) -> u64 { return *p; }
"#,
        )
        .unwrap();
        let err = check(&[("test.oc".into(), ast)]).unwrap_err();
        assert!(err.message.contains("unsafe"));
    }

    #[test]
    fn statically_checks_functions_and_control_flow() {
        let program = checked(
            r#"
module math;
fn add(a: u64, b: u64) -> u64 { return a + b; }
fn sum(n: u64) -> u64 {
    let mut i: u64 = 0;
    let mut total: u64 = 0;
    while i < n { total += i; i += 1; }
    return total;
}
"#,
        );
        assert_eq!(program.functions.len(), 2);
        assert_eq!(program.functions[1].locals.len(), 3);
    }
}
