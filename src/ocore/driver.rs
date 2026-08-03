use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};

use super::hir::{HirProgram, Type};
use super::{codegen, codegen_aarch64};
use super::{mir, parser, typeck, Diagnostic, Span};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    X86_64UnknownNone,
    Aarch64UnknownNone,
}

impl Target {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "x86_64-unknown-none" | "x86_64-unknown-none-elf" => Some(Target::X86_64UnknownNone),
            "aarch64-unknown-none" | "aarch64-unknown-none-elf" => Some(Target::Aarch64UnknownNone),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Target::X86_64UnknownNone => "x86_64-unknown-none",
            Target::Aarch64UnknownNone => "aarch64-unknown-none",
        }
    }

    pub fn triple(self) -> &'static str {
        match self {
            Target::X86_64UnknownNone => "x86_64-unknown-none-elf",
            Target::Aarch64UnknownNone => "aarch64-unknown-none-elf",
        }
    }

    fn architecture(self) -> &'static str {
        match self {
            Target::X86_64UnknownNone => "x86_64",
            Target::Aarch64UnknownNone => "aarch64",
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

    let assembly = match options.target {
        Target::X86_64UnknownNone => codegen::emit_assembly(&hir, &mir)?,
        Target::Aarch64UnknownNone => codegen_aarch64::emit_assembly(&hir, &mir)?,
    };
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

    let clang = which::which("clang").map_err(|_| {
        driver_error(format!(
            "clang is required to assemble {} ELF objects",
            options.target.architecture()
        ))
    })?;
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
            "{} assembler failed:\n{}",
            options.target.architecture(),
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

static TEMP_NONCE: AtomicU64 = AtomicU64::new(0);

fn monotonic_nonce() -> String {
    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let sequence = TEMP_NONCE.fetch_add(1, Ordering::Relaxed);
    format!("{timestamp}-{sequence}")
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
    fn accepts_both_freestanding_target_aliases() {
        assert_eq!(
            Target::parse("x86_64-unknown-none-elf"),
            Some(Target::X86_64UnknownNone)
        );
        assert_eq!(
            Target::parse("aarch64-unknown-none"),
            Some(Target::Aarch64UnknownNone)
        );
        assert_eq!(
            Target::parse("aarch64-unknown-none-elf"),
            Some(Target::Aarch64UnknownNone)
        );
        assert_eq!(Target::parse("aarch64-linux-gnu"), None);
    }

    #[test]
    fn compiles_an_aarch64_elf64_relocatable_object() {
        let dir =
            std::env::temp_dir().join(format!("ocore-aarch64-driver-test-{}", monotonic_nonce()));
        fs::create_dir_all(&dir).unwrap();
        let source = dir.join("answer.oc");
        let object = dir.join("answer.o");
        fs::write(
            &source,
            r#"
module answer;
static ANSWER: u64 = 42;
@export @no_mangle
fn answer() -> u64 {
    return ANSWER;
}
"#,
        )
        .unwrap();
        compile(
            &[source],
            &CompileOptions {
                target: Target::Aarch64UnknownNone,
                emit: EmitKind::Object,
                output: object.clone(),
                keep_assembly: false,
            },
        )
        .unwrap();
        let bytes = fs::read(object).unwrap();
        assert_eq!(&bytes[..4], b"\x7fELF");
        assert_eq!(bytes[4], 2); // ELFCLASS64
        assert_eq!(u16::from_le_bytes([bytes[18], bytes[19]]), 183); // EM_AARCH64
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn assembles_aarch64_calls_control_flow_memory_and_privileged_intrinsics() {
        let dir = std::env::temp_dir().join(format!(
            "ocore-aarch64-corpus-driver-test-{}",
            monotonic_nonce()
        ));
        fs::create_dir_all(&dir).unwrap();
        let source = dir.join("corpus.oc");
        let object = dir.join("corpus.o");
        fs::write(
            &source,
            r#"
module corpus;
static mut CELL: u64 = 7;

fn add(left: u64, right: u64) -> u64 {
    return left + right;
}

@export @no_mangle
unsafe fn exercise(pointer: *mut u64, left: u64, right: u64) -> u64 {
    let current: u64 = volatile_load(pointer);
    volatile_store(pointer, current + 1);
    let result: u64 = syscall1(9, current);
    if left < right {
        return add(result, left);
    }
    return result - right;
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
        compile(
            &[source],
            &CompileOptions {
                target: Target::Aarch64UnknownNone,
                emit: EmitKind::Object,
                output: object.clone(),
                keep_assembly: false,
            },
        )
        .unwrap();
        let bytes = fs::read(object).unwrap();
        assert_eq!(&bytes[..4], b"\x7fELF");
        assert_eq!(bytes[4], 2); // ELFCLASS64
        assert_eq!(u16::from_le_bytes([bytes[18], bytes[19]]), 183); // EM_AARCH64
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn aarch64_object_is_byte_reproducible_across_source_directories() {
        let dir =
            std::env::temp_dir().join(format!("ocore-aarch64-repro-test-{}", monotonic_nonce()));
        let first_dir = dir.join("first-root");
        let second_dir = dir.join("second-root");
        fs::create_dir_all(&first_dir).unwrap();
        fs::create_dir_all(&second_dir).unwrap();
        let source_text = r#"
module reproducible_aarch64;
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
                    target: Target::Aarch64UnknownNone,
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
            "identical O-core modules emitted different AArch64 object bytes"
        );
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
