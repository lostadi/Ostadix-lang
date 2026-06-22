use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use super::codegen;
use super::hir::{HirProgram, Type};
use super::{mir, parser, typeck, Diagnostic, Span};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    X86_64UnknownNone,
}

impl Target {
    pub fn triple(self) -> &'static str {
        match self {
            Target::X86_64UnknownNone => "x86_64-unknown-none-elf",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EmitKind {
    Ast,
    Hir,
    Mir,
    Assembly,
    Object,
}

#[derive(Debug, Clone)]
pub struct CompileOptions {
    pub target: Target,
    pub emit: EmitKind,
    pub output: PathBuf,
    pub keep_assembly: bool,
}

#[derive(Debug, Clone)]
pub struct CompileOutput {
    pub output: PathBuf,
    pub assembly: Option<PathBuf>,
}

pub fn compile(inputs: &[PathBuf], options: &CompileOptions) -> Result<CompileOutput, Diagnostic> {
    if inputs.is_empty() {
        return Err(driver_error("at least one .oc input is required"));
    }
    let mut parsed = Vec::new();
    for path in inputs {
        let source = fs::read_to_string(path).map_err(|error| Diagnostic {
            file: path.display().to_string(),
            span: Span::default(),
            message: format!("failed to read source: {error}"),
        })?;
        let file = path.display().to_string();
        parsed.push((file.clone(), parser::parse(&file, &source)?));
    }

    if options.emit == EmitKind::Ast {
        write_text(&options.output, &format!("{parsed:#?}"))?;
        return Ok(CompileOutput {
            output: options.output.clone(),
            assembly: None,
        });
    }

    let hir = typeck::check(&parsed)?;
    if options.emit == EmitKind::Hir {
        write_text(&options.output, &hir_text(&hir))?;
        return Ok(CompileOutput {
            output: options.output.clone(),
            assembly: None,
        });
    }

    let mir = mir::lower(&hir)?;
    if options.emit == EmitKind::Mir {
        write_text(&options.output, &mir.to_text(&hir))?;
        return Ok(CompileOutput {
            output: options.output.clone(),
            assembly: None,
        });
    }

    let assembly = codegen::emit_assembly(&hir, &mir)?;
    if options.emit == EmitKind::Assembly {
        write_text(&options.output, &assembly)?;
        return Ok(CompileOutput {
            output: options.output.clone(),
            assembly: Some(options.output.clone()),
        });
    }

    emit_object(options, &assembly)
}

fn emit_object(options: &CompileOptions, assembly: &str) -> Result<CompileOutput, Diagnostic> {
    if let Some(parent) = options.output.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)
                .map_err(|e| driver_error(format!("failed to create {}: {e}", parent.display())))?;
        }
    }
    let assembly_path = if options.keep_assembly {
        options.output.with_extension("s")
    } else {
        std::env::temp_dir().join(format!(
            "ocorec-{}-{}.s",
            std::process::id(),
            monotonic_nonce()
        ))
    };
    fs::write(&assembly_path, assembly)
        .map_err(|e| driver_error(format!("failed to write {}: {e}", assembly_path.display())))?;

    let clang = which::which("clang")
        .map_err(|_| driver_error("clang is required to assemble x86_64 ELF objects"))?;
    let output = Command::new(&clang)
        .args([
            "-target",
            options.target.triple(),
            "-ffreestanding",
            "-fno-stack-protector",
            "-c",
            "-x",
            "assembler",
        ])
        .arg(&assembly_path)
        .arg("-o")
        .arg(&options.output)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .map_err(|e| driver_error(format!("failed to run {}: {e}", clang.display())))?;

    if !options.keep_assembly {
        let _ = fs::remove_file(&assembly_path);
    }
    if !output.status.success() {
        return Err(driver_error(format!(
            "x86_64 assembler failed:\n{}",
            String::from_utf8_lossy(&output.stderr)
        )));
    }
    Ok(CompileOutput {
        output: options.output.clone(),
        assembly: options.keep_assembly.then_some(assembly_path),
    })
}

fn write_text(path: &Path, text: &str) -> Result<(), Diagnostic> {
    if path == Path::new("-") {
        print!("{text}");
        return Ok(());
    }
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)
                .map_err(|e| driver_error(format!("failed to create {}: {e}", parent.display())))?;
        }
    }
    fs::write(path, text)
        .map_err(|e| driver_error(format!("failed to write {}: {e}", path.display())))
}

fn hir_text(hir: &HirProgram) -> String {
    let mut out = String::from("; O-core resolved typed HIR\n; layouts\n");
    for def in &hir.types.structs {
        out.push_str(&format!(
            "struct {} size={} align={}{}\n",
            def.name,
            def.layout.size,
            def.layout.align,
            if def.packed { " packed" } else { "" }
        ));
        for field in &def.fields {
            out.push_str(&format!(
                "  +{} {}: {}\n",
                field.offset,
                field.name,
                hir.types.name(field.ty)
            ));
        }
    }
    for def in &hir.types.enums {
        out.push_str(&format!(
            "enum {} size={} align={} tag={} payload_offset={}\n",
            def.name, def.layout.size, def.layout.align, def.tag_size, def.payload_offset
        ));
    }
    out.push_str("; functions\n");
    for function in &hir.functions {
        out.push_str(&format!(
            "{}fn {}({}) -> {} abi={:?} symbol={}\n",
            if function.unsafe_ { "unsafe " } else { "" },
            function.qualified_name,
            function
                .params
                .iter()
                .map(|id| {
                    let local = &function.locals[*id];
                    format!("{}: {}", local.name, hir.types.name(local.ty))
                })
                .collect::<Vec<_>>()
                .join(", "),
            hir.types.name(function.result),
            function.abi,
            function.symbol
        ));
    }
    out.push_str("; static types\n");
    for static_ in &hir.statics {
        let class = match hir.types.types[static_.ty] {
            Type::Array { .. } => "aggregate",
            _ => "scalar",
        };
        out.push_str(&format!(
            "static {}: {} ({class}) symbol={}\n",
            static_.qualified_name,
            hir.types.name(static_.ty),
            static_.symbol
        ));
    }
    out
}

fn monotonic_nonce() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0)
}

fn driver_error(message: impl Into<String>) -> Diagnostic {
    Diagnostic {
        file: "<ocorec>".into(),
        span: Span::default(),
        message: message.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compiles_an_elf64_relocatable_object() {
        let dir = std::env::temp_dir().join(format!("ocore-driver-test-{}", monotonic_nonce()));
        fs::create_dir_all(&dir).unwrap();
        let source = dir.join("kernel.oc");
        let object = dir.join("kernel.o");
        fs::write(
            &source,
            r#"
module kernel;
@export @no_mangle
unsafe fn kernel_main() -> never {
    unsafe { outb(0x3f8, b'O'); }
    loop { unsafe { halt(); } }
}
"#,
        )
        .unwrap();
        compile(
            &[source],
            &CompileOptions {
                target: Target::X86_64UnknownNone,
                emit: EmitKind::Object,
                output: object.clone(),
                keep_assembly: false,
            },
        )
        .unwrap();
        let bytes = fs::read(object).unwrap();
        assert_eq!(&bytes[..4], b"\x7fELF");
        assert_eq!(bytes[4], 2); // ELFCLASS64
        assert_eq!(u16::from_le_bytes([bytes[18], bytes[19]]), 62); // EM_X86_64
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn ocore_object_is_byte_reproducible_across_source_directories() {
        let dir = std::env::temp_dir().join(format!("ocore-repro-test-{}", monotonic_nonce()));
        let first_dir = dir.join("first-root");
        let second_dir = dir.join("second-root");
        fs::create_dir_all(&first_dir).unwrap();
        fs::create_dir_all(&second_dir).unwrap();
        let source_text = r#"
module reproducible;
static ANSWER: u64 = 42;
@export @no_mangle
fn answer() -> u64 {
    return ANSWER;
}
"#;
        let first_source = first_dir.join("input.oc");
        let second_source = second_dir.join("renamed.oc");
        let first_object = first_dir.join("first.o");
        let second_object = second_dir.join("second.o");
        fs::write(&first_source, source_text).unwrap();
        fs::write(&second_source, source_text).unwrap();

        for (source, output) in [
            (first_source, first_object.clone()),
            (second_source, second_object.clone()),
        ] {
            compile(
                &[source],
                &CompileOptions {
                    target: Target::X86_64UnknownNone,
                    emit: EmitKind::Object,
                    output,
                    keep_assembly: false,
                },
            )
            .unwrap();
        }

        assert_eq!(
            fs::read(first_object).unwrap(),
            fs::read(second_object).unwrap(),
            "identical O-core modules emitted different object bytes"
        );
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn rejects_float_miscompilation_before_object_emission() {
        let dir =
            std::env::temp_dir().join(format!("ocore-float-regression-test-{}", monotonic_nonce()));
        fs::create_dir_all(&dir).unwrap();
        let source = dir.join("float.oc");
        let object = dir.join("float.o");
        fs::write(
            &source,
            r#"
module floats;
fn compare() -> bool {
    let x: f64 = 1 as f64;
    let y: f64 = 2 as f64;
    return x < y;
}
"#,
        )
        .unwrap();

        let error = compile(
            &[source],
            &CompileOptions {
                target: Target::X86_64UnknownNone,
                emit: EmitKind::Object,
                output: object.clone(),
                keep_assembly: false,
            },
        )
        .unwrap_err();

        assert!(error.message.contains("floating-point cast"));
        assert!(!object.exists(), "invalid float program emitted an object");
        let _ = fs::remove_dir_all(dir);
    }
}
