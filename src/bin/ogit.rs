use anyhow::{bail, Context, Result};
use clap::{Parser as ClapParser, Subcommand};
use serde::Serialize;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

const DEMO_SOURCE: &str = r#"#!/usr/bin/env O
# O-Git semantic receipt demo: What Compilation Forgets.
# This file is intentionally tiny. O-Git reads the `# ogit:` declarations
# below as semantic source facts, then lowers the executable part to plain C.
#
# ogit: group normalize_pipeline
# ogit: policy lazy
# ogit: invariant output_count == input_count
# ogit: effects read examples/group_pipeline/input.txt write examples/group_pipeline/output.txt
# ogit: steps load -> normalize -> emit
# ogit: intent normalize only when requested

let load_data = python{defer,fs_read}^(
with open("examples/group_pipeline/input.txt", "r", encoding="utf-8") as f:
    __oval_result__ = [float(x) for x in f.read().split()]
)_python{defer,fs_read}

let normalize_data = python{defer}^(
values = [1, 2, 3, 4, 5]
scale = max(values)
__oval_result__ = [round(v / scale, 2) for v in values]
)_python{defer}

let emit_result = python{defer,fs_write}^(
values = [0.20, 0.40, 0.60, 0.80, 1.00]
with open("examples/group_pipeline/output.txt", "w", encoding="utf-8") as f:
    f.write("normalized: " + str(values) + "\n")
__oval_result__ = "normalized: " + str(values)
)_python{defer,fs_write}

let normalize_pipeline = lazy(batch($load_data, $normalize_data, $emit_result))

markdown^(
# normalize_pipeline

- policy: lazy
- invariant: output_count == input_count
- effects: read input.txt, write output.txt
- steps: load -> normalize -> emit
- group value: `$normalize_pipeline`
)_markdown
"#;

const DEMO_EAGER_SOURCE: &str = r#"#!/usr/bin/env O
# O-Git semantic receipt demo: What Compilation Forgets.
# This variant changes exactly the semantic policy line.
#
# ogit: group normalize_pipeline
# ogit: policy eager
# ogit: invariant output_count == input_count
# ogit: effects read examples/group_pipeline/input.txt write examples/group_pipeline/output.txt
# ogit: steps load -> normalize -> emit
# ogit: intent normalize during construction rather than on demand

let load_data = python{defer,fs_read}^(
with open("examples/group_pipeline/input.txt", "r", encoding="utf-8") as f:
    __oval_result__ = [float(x) for x in f.read().split()]
)_python{defer,fs_read}

let normalize_data = python{defer}^(
values = [1, 2, 3, 4, 5]
scale = max(values)
__oval_result__ = [round(v / scale, 2) for v in values]
)_python{defer}

let emit_result = python{defer,fs_write}^(
values = [0.20, 0.40, 0.60, 0.80, 1.00]
with open("examples/group_pipeline/output.txt", "w", encoding="utf-8") as f:
    f.write("normalized: " + str(values) + "\n")
__oval_result__ = "normalized: " + str(values)
)_python{defer,fs_write}

let normalize_pipeline = batch($load_data, $normalize_data, $emit_result)

markdown^(
# normalize_pipeline

- policy: eager
- invariant: output_count == input_count
- effects: read input.txt, write output.txt
- steps: load -> normalize -> emit
- group value: `$normalize_pipeline`
)_markdown
"#;

const DEMO_INPUT: &str = "1 2 3 4 5\n";

#[derive(ClapParser, Debug)]
#[command(
    name = "ogit",
    about = "Semantic receipt tools for Ostadix-lang transformations",
    long_about = "\
O-Git is a small semantic-ledger companion for Ostadix-lang demos. The current \
surface is intentionally narrow: it can run the semantic receipt demo and \
compare two demo sources for meaning-level changes that ordinary textual diff \
does not explain."
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand, Debug)]
enum Commands {
    /// Run a built-in O-Git demo.
    Demo {
        #[command(subcommand)]
        command: DemoCommand,
    },
    /// Compare two Ostadix-lang demo sources as semantic declarations, not text.
    DiffSemantic {
        /// Earlier .O source.
        old: PathBuf,
        /// Later .O source.
        new: PathBuf,
    },
}

#[derive(Subcommand, Debug)]
enum DemoCommand {
    /// Build, run, and receipt the tiny group pipeline demo.
    SemanticReceipt,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct DemoProgram {
    group: String,
    policy: String,
    invariant: String,
    effects: Effects,
    steps: Vec<String>,
    intent: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct Effects {
    read: String,
    write: String,
}

#[derive(Debug, Serialize)]
struct ReceiptGroup<'a> {
    name: &'a str,
    policy: &'a str,
    invariant: &'a str,
    effects: &'a Effects,
    steps: &'a [String],
    intent: &'a str,
}

#[derive(Debug, Serialize)]
struct SemanticCost {
    coordination: &'static str,
    type_shape: &'static str,
    effect_safety: &'static str,
    intent_preservation: &'static str,
}

#[derive(Debug, Serialize)]
struct SemanticReceipt<'a> {
    artifact: &'static str,
    source: String,
    lowered_target: String,
    executable: String,
    graph_html: String,
    graph_dot: String,
    group: ReceiptGroup<'a>,
    program_output: &'a str,
    preserved: Vec<String>,
    lowered: Vec<String>,
    erased: Vec<String>,
    assumed: Vec<String>,
    runtime_implications: Vec<String>,
    semantic_cost: SemanticCost,
}

#[derive(Debug, Clone, Serialize)]
struct SemanticChange {
    category: &'static str,
    subject: String,
    before: String,
    after: String,
    meaning: String,
    implications: Vec<String>,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Commands::Demo {
            command: DemoCommand::SemanticReceipt,
        } => run_semantic_receipt_demo(),
        Commands::DiffSemantic { old, new } => {
            let old_source = fs::read_to_string(&old)
                .with_context(|| format!("failed to read {}", old.display()))?;
            let new_source = fs::read_to_string(&new)
                .with_context(|| format!("failed to read {}", new.display()))?;
            let old_program = parse_demo_program(&old_source)
                .with_context(|| format!("failed to parse {}", old.display()))?;
            let new_program = parse_demo_program(&new_source)
                .with_context(|| format!("failed to parse {}", new.display()))?;
            print_semantic_diff(&old, &new, &old_program, &new_program);
            Ok(())
        }
    }
}

fn run_semantic_receipt_demo() -> Result<()> {
    let root = repo_root()?;
    let paths = DemoPaths::new(&root);
    ensure_demo_files(&paths)?;

    let source = fs::read_to_string(&paths.source)
        .with_context(|| format!("failed to read {}", paths.source.display()))?;
    let program = parse_demo_program(&source)?;

    fs::create_dir_all(&paths.generated_dir)?;
    fs::create_dir_all(&paths.receipt_dir)?;

    println!("O-Git Semantic Receipt Demo");
    println!();
    println!("1. Building {}", display_path(&root, &paths.source));
    println!("2. Lowering O-Lang group -> C target");
    write_c_lowering(&program, &paths.c_target)?;

    println!("3. Running compiled artifact");
    compile_c(&paths.c_target, &paths.executable)?;
    let program_output = run_executable(&root, &paths.executable)?;
    println!("4. Building visible semantic graph");
    write_semantic_graph(&program, &paths.graph_dot, &paths.graph_html)?;

    println!();
    println!("Program output:");
    println!("{}", program_output.trim_end());
    println!();

    let receipt = build_receipt(&root, &paths, &program, program_output.trim_end());
    let receipt_json =
        serde_json::to_string_pretty(&receipt).context("failed to serialize receipt")?;
    fs::write(&paths.receipt, receipt_json)
        .with_context(|| format!("failed to write {}", paths.receipt.display()))?;

    print_receipt(&root, &paths.receipt, &receipt);

    let eager_source = fs::read_to_string(&paths.eager_source)
        .with_context(|| format!("failed to read {}", paths.eager_source.display()))?;
    let eager_program = parse_demo_program(&eager_source)?;

    println!();
    println!("Semantic diff sample:");
    println!(
        "  ogit diff-semantic {} {}",
        display_path(&root, &paths.source),
        display_path(&root, &paths.eager_source)
    );
    println!();
    print_semantic_diff(&paths.source, &paths.eager_source, &program, &eager_program);

    Ok(())
}

struct DemoPaths {
    source: PathBuf,
    eager_source: PathBuf,
    input: PathBuf,
    generated_dir: PathBuf,
    c_target: PathBuf,
    executable: PathBuf,
    graph_html: PathBuf,
    graph_dot: PathBuf,
    receipt_dir: PathBuf,
    receipt: PathBuf,
}

impl DemoPaths {
    fn new(root: &Path) -> Self {
        let example_dir = root.join("examples/group_pipeline");
        let generated_dir = example_dir.join("generated");
        let receipt_dir = root.join(".ogit/receipts");
        Self {
            source: example_dir.join("main.O"),
            eager_source: example_dir.join("main.eager.O"),
            input: example_dir.join("input.txt"),
            generated_dir: generated_dir.clone(),
            c_target: generated_dir.join("normalize_pipeline.c"),
            executable: generated_dir.join(executable_name("normalize_pipeline")),
            graph_html: generated_dir.join("semantic_graph.html"),
            graph_dot: generated_dir.join("semantic_graph.dot"),
            receipt_dir: receipt_dir.clone(),
            receipt: receipt_dir.join("semantic-receipt-001.json"),
        }
    }
}

fn ensure_demo_files(paths: &DemoPaths) -> Result<()> {
    let example_dir = paths
        .source
        .parent()
        .context("demo source path has no parent")?;
    fs::create_dir_all(example_dir)?;
    write_if_missing(&paths.source, DEMO_SOURCE)?;
    write_if_missing(&paths.eager_source, DEMO_EAGER_SOURCE)?;
    write_if_missing(&paths.input, DEMO_INPUT)?;
    Ok(())
}

fn write_if_missing(path: &Path, content: &str) -> Result<()> {
    if !path.exists() {
        fs::write(path, content).with_context(|| format!("failed to write {}", path.display()))?;
    }
    Ok(())
}

fn repo_root() -> Result<PathBuf> {
    let mut dir = env::current_dir().context("failed to read current directory")?;
    loop {
        let cargo_toml = dir.join("Cargo.toml");
        if cargo_toml.exists() && dir.join("examples").exists() && dir.join("src").exists() {
            return Ok(dir);
        }
        if !dir.pop() {
            bail!("could not find Ostadix-lang repository root from current directory");
        }
    }
}

fn parse_demo_program(source: &str) -> Result<DemoProgram> {
    let mut group = None;
    let mut policy = None;
    let mut invariant = None;
    let mut effects = None;
    let mut steps = None;
    let mut intent = None;

    for line in source.lines() {
        let Some(rest) = line.trim().strip_prefix("# ogit: ") else {
            continue;
        };
        if let Some(value) = rest.strip_prefix("group ") {
            group = Some(value.trim().to_string());
        } else if let Some(value) = rest.strip_prefix("policy ") {
            policy = Some(value.trim().to_string());
        } else if let Some(value) = rest.strip_prefix("invariant ") {
            invariant = Some(value.trim().to_string());
        } else if let Some(value) = rest.strip_prefix("effects ") {
            effects = Some(parse_effects(value)?);
        } else if let Some(value) = rest.strip_prefix("steps ") {
            steps = Some(
                value
                    .split("->")
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(ToOwned::to_owned)
                    .collect::<Vec<_>>(),
            );
        } else if let Some(value) = rest.strip_prefix("intent ") {
            intent = Some(value.trim().to_string());
        }
    }

    let program = DemoProgram {
        group: group.context("missing `# ogit: group ...` declaration")?,
        policy: policy.context("missing `# ogit: policy ...` declaration")?,
        invariant: invariant.context("missing `# ogit: invariant ...` declaration")?,
        effects: effects.context("missing `# ogit: effects ...` declaration")?,
        steps: steps.context("missing `# ogit: steps ...` declaration")?,
        intent: intent.context("missing `# ogit: intent ...` declaration")?,
    };

    if program.steps.len() < 2 {
        bail!("semantic receipt demo needs at least two ordered steps");
    }

    Ok(program)
}

fn parse_effects(value: &str) -> Result<Effects> {
    let mut read = None;
    let mut write = None;
    let parts = value.split_whitespace().collect::<Vec<_>>();
    let mut i = 0;
    while i < parts.len() {
        match parts[i] {
            "read" => {
                let path = parts
                    .get(i + 1)
                    .context("effects declaration has `read` without a path")?;
                read = Some((*path).to_string());
                i += 2;
            }
            "write" => {
                let path = parts
                    .get(i + 1)
                    .context("effects declaration has `write` without a path")?;
                write = Some((*path).to_string());
                i += 2;
            }
            _ => i += 1,
        }
    }

    Ok(Effects {
        read: read.context("effects declaration missing `read PATH`")?,
        write: write.context("effects declaration missing `write PATH`")?,
    })
}

fn write_c_lowering(program: &DemoProgram, target: &Path) -> Result<()> {
    let source = format!(
        r#"#include <stdio.h>
#include <stdlib.h>

static void print_values(FILE *stream, const double *values, int count) {{
    fputs("normalized: [", stream);
    for (int i = 0; i < count; ++i) {{
        fprintf(stream, "%.2f", values[i]);
        if (i + 1 < count) {{
            fputs(", ", stream);
        }}
    }}
    fputs("]", stream);
}}

int main(void) {{
    const char *input_path = "{read_path}";
    const char *output_path = "{write_path}";
    FILE *input = fopen(input_path, "r");
    if (!input) {{
        perror(input_path);
        return 1;
    }}

    double input_values[64];
    int input_count = 0;
    while (input_count < 64 && fscanf(input, "%lf", &input_values[input_count]) == 1) {{
        input_count += 1;
    }}
    fclose(input);

    if (input_count == 0) {{
        fprintf(stderr, "no input values found in %s\n", input_path);
        return 1;
    }}

    double max_value = input_values[0];
    for (int i = 1; i < input_count; ++i) {{
        if (input_values[i] > max_value) {{
            max_value = input_values[i];
        }}
    }}
    if (max_value == 0.0) {{
        fprintf(stderr, "cannot normalize by zero\n");
        return 1;
    }}

    double output_values[64];
    for (int i = 0; i < input_count; ++i) {{
        output_values[i] = input_values[i] / max_value;
    }}

    FILE *output = fopen(output_path, "w");
    if (!output) {{
        perror(output_path);
        return 1;
    }}

    print_values(stdout, output_values, input_count);
    fputc('\n', stdout);
    print_values(output, output_values, input_count);
    fputc('\n', output);
    fclose(output);

    return 0;
}}
"#,
        read_path = c_escape(&program.effects.read),
        write_path = c_escape(&program.effects.write),
    );

    fs::write(target, source).with_context(|| format!("failed to write {}", target.display()))?;
    Ok(())
}

fn compile_c(c_target: &Path, executable: &Path) -> Result<()> {
    let compiler = env::var("CC").unwrap_or_else(|_| "cc".to_string());
    let status = Command::new(&compiler)
        .arg("-std=c17")
        .arg("-O2")
        .arg("-Wall")
        .arg("-Wextra")
        .arg(c_target)
        .arg("-o")
        .arg(executable)
        .status()
        .with_context(|| format!("failed to spawn C compiler `{compiler}`"))?;

    if !status.success() {
        bail!("C lowering failed to compile with `{compiler}`");
    }
    Ok(())
}

fn run_executable(root: &Path, executable: &Path) -> Result<String> {
    let output = Command::new(executable)
        .current_dir(root)
        .output()
        .with_context(|| format!("failed to run {}", executable.display()))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("compiled artifact failed: {}", stderr.trim());
    }
    String::from_utf8(output.stdout).context("compiled artifact produced non-UTF-8 stdout")
}

fn build_receipt<'a>(
    root: &Path,
    paths: &DemoPaths,
    program: &'a DemoProgram,
    program_output: &'a str,
) -> SemanticReceipt<'a> {
    SemanticReceipt {
        artifact: "O-Git Semantic Receipt Demo",
        source: display_path(root, &paths.source),
        lowered_target: display_path(root, &paths.c_target),
        executable: display_path(root, &paths.executable),
        graph_html: display_path(root, &paths.graph_html),
        graph_dot: display_path(root, &paths.graph_dot),
        group: ReceiptGroup {
            name: &program.group,
            policy: &program.policy,
            invariant: &program.invariant,
            effects: &program.effects,
            steps: &program.steps,
            intent: &program.intent,
        },
        program_output,
        preserved: vec![
            format!("data dependency order: {}", program.steps.join(" -> ")),
            "input/output numeric type shape".to_string(),
            "deterministic numeric computation".to_string(),
        ],
        lowered: vec![
            format!("group `{}` -> C function sequence", program.group),
            "declared effects -> runtime file calls".to_string(),
        ],
        erased: vec![
            format!("{} execution policy", program.policy),
            "explicit group boundary".to_string(),
            format!("invariant: {}", program.invariant),
            format!("author-level intent: `{}`", program.intent),
        ],
        assumed: vec![
            "filesystem access exists at runtime".to_string(),
            "target runtime does not enforce group policy".to_string(),
            "invariant remains a receipt fact unless checked externally".to_string(),
        ],
        runtime_implications: vec![
            "compiled C preserves the result but cannot natively carry the group contract"
                .to_string(),
            "effect timing is now a property of ordinary file calls".to_string(),
        ],
        semantic_cost: SemanticCost {
            coordination: "high",
            type_shape: "low",
            effect_safety: "medium",
            intent_preservation: "low",
        },
    }
}

fn print_receipt(root: &Path, receipt_path: &Path, receipt: &SemanticReceipt<'_>) {
    println!("O-Git semantic receipt:");
    println!();
    print_section("PRESERVED", &receipt.preserved);
    print_section("LOWERED", &receipt.lowered);
    print_section("ERASED", &receipt.erased);
    print_section("ASSUMED", &receipt.assumed);
    println!("SEMANTIC COST:");
    println!("  coordination: {}", receipt.semantic_cost.coordination);
    println!("  type-shape: {}", receipt.semantic_cost.type_shape);
    println!("  effect-safety: {}", receipt.semantic_cost.effect_safety);
    println!(
        "  intent-preservation: {}",
        receipt.semantic_cost.intent_preservation
    );
    println!();
    println!("Receipt saved:");
    println!("  {}", display_path(root, receipt_path));
    println!("Visible graph saved:");
    println!("  {}", receipt.graph_html);
    println!("DOT graph saved:");
    println!("  {}", receipt.graph_dot);
    println!("Open it with:");
    println!("  open {}", root.join(&receipt.graph_html).display());
}

fn print_section(name: &str, items: &[String]) {
    println!("{name}:");
    for item in items {
        println!("  - {item}");
    }
    println!();
}

fn print_semantic_diff(old_path: &Path, new_path: &Path, old: &DemoProgram, new: &DemoProgram) {
    let changes = semantic_changes(old, new);
    println!("O-Git semantic diff:");
    println!("  old: {}", old_path.display());
    println!("  new: {}", new_path.display());
    println!();

    if changes.is_empty() {
        println!("NO SEMANTIC CHANGE:");
        println!("  The O-Git declarations describe the same group contract.");
        return;
    }

    for change in changes {
        println!("{} CHANGED:", change.category.to_uppercase());
        println!("  subject: {}", change.subject);
        println!("  before: {}", change.before);
        println!("  after:  {}", change.after);
        println!("  meaning: {}", change.meaning);
        println!("  implications:");
        for implication in change.implications {
            println!("    - {implication}");
        }
        println!();
    }
}

fn semantic_changes(old: &DemoProgram, new: &DemoProgram) -> Vec<SemanticChange> {
    let mut changes = Vec::new();

    if old.policy != new.policy {
        changes.push(SemanticChange {
            category: "policy",
            subject: old.group.clone(),
            before: old.policy.clone(),
            after: new.policy.clone(),
            meaning: "runtime demand model changed".to_string(),
            implications: policy_implications(&old.policy, &new.policy),
        });
    }

    if old.invariant != new.invariant {
        changes.push(SemanticChange {
            category: "invariant",
            subject: old.group.clone(),
            before: old.invariant.clone(),
            after: new.invariant.clone(),
            meaning: "the correctness condition attached to the group changed".to_string(),
            implications: vec![
                "receipts generated before and after the change are not directly equivalent"
                    .to_string(),
                "a target that omits runtime checks may silently accept the new condition"
                    .to_string(),
            ],
        });
    }

    if old.effects != new.effects {
        changes.push(SemanticChange {
            category: "effects",
            subject: old.group.clone(),
            before: format!("read {}, write {}", old.effects.read, old.effects.write),
            after: format!("read {}, write {}", new.effects.read, new.effects.write),
            meaning: "the declared IO boundary changed".to_string(),
            implications: vec![
                "runtime authority and reproducibility assumptions changed".to_string(),
                "ordinary Git sees text, but O-Git sees a boundary change".to_string(),
            ],
        });
    }

    if old.steps != new.steps {
        changes.push(SemanticChange {
            category: "dependency order",
            subject: old.group.clone(),
            before: old.steps.join(" -> "),
            after: new.steps.join(" -> "),
            meaning: "the coordinated task order changed".to_string(),
            implications: vec![
                "dataflow may no longer match the original author intent".to_string(),
                "lowered targets may still compile while executing a different pipeline"
                    .to_string(),
            ],
        });
    }

    changes
}

fn policy_implications(old: &str, new: &str) -> Vec<String> {
    match (old, new) {
        ("lazy", "eager") => vec![
            "upfront execution may increase".to_string(),
            "file reads may happen before the result is demanded".to_string(),
            "side-effect timing moves earlier in the program lifecycle".to_string(),
            "author intent changed from deferred coordination to immediate execution".to_string(),
        ],
        ("eager", "lazy") => vec![
            "work may be delayed until a consumer demands the result".to_string(),
            "file reads and writes may occur later than before".to_string(),
            "runtime latency can move from construction time to demand time".to_string(),
        ],
        _ => vec![
            format!("policy changed from `{old}` to `{new}`"),
            "execution timing and side-effect visibility should be reviewed".to_string(),
        ],
    }
}

fn display_path(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .to_string()
}

fn executable_name(name: &str) -> String {
    if cfg!(windows) {
        format!("{name}.exe")
    } else {
        name.to_string()
    }
}

fn c_escape(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
}

fn write_semantic_graph(program: &DemoProgram, dot_path: &Path, html_path: &Path) -> Result<()> {
    let dot = render_semantic_dot(program);
    fs::write(dot_path, &dot).with_context(|| format!("failed to write {}", dot_path.display()))?;
    fs::write(html_path, render_semantic_graph_html(program, &dot))
        .with_context(|| format!("failed to write {}", html_path.display()))?;
    Ok(())
}

fn render_semantic_dot(program: &DemoProgram) -> String {
    let mut dot = String::new();
    dot.push_str("digraph semantic_receipt {\n");
    dot.push_str("  rankdir=LR;\n");
    dot.push_str(
        "  graph [bgcolor=\"#0a0a0b\", pad=\"0.3\", nodesep=\"0.45\", ranksep=\"0.65\"];\n",
    );
    dot.push_str("  node [shape=box, style=\"rounded,filled\", fontname=\"Fira Code\", fontsize=11, color=\"#c48a5a\", fillcolor=\"#161616\", fontcolor=\"#e6e2d6\"];\n");
    dot.push_str("  edge [fontname=\"Fira Code\", fontsize=10, color=\"#5f7f8e\", fontcolor=\"#c48a5a\", arrowsize=0.8];\n");
    dot.push_str(&format!(
        "  source [label=\"O source\\nmain.O\"];\n  contract [label=\"group: {}\\npolicy: {}\\ninvariant: {}\"];\n  lowered [label=\"lowered C\\nnormalize_pipeline.c\"];\n  executable [label=\"compiled artifact\"];\n  output [label=\"runtime output\"];\n  receipt [label=\"semantic receipt\\nJSON + visible graph\"];\n",
        dot_escape(&program.group),
        dot_escape(&program.policy),
        dot_escape(&program.invariant)
    ));
    dot.push_str("  source -> contract -> lowered -> executable -> output -> receipt;\n");
    dot.push_str(&format!(
        "  input [label=\"read\\n{}\"];\n",
        dot_escape(&leaf_name(&program.effects.read))
    ));
    for (index, step) in program.steps.iter().enumerate() {
        dot.push_str(&format!(
            "  step_{index} [label=\"step {}\\n{}\"];\n",
            index + 1,
            dot_escape(step)
        ));
    }
    dot.push_str(&format!(
        "  output_file [label=\"write\\n{}\"];\n",
        dot_escape(&leaf_name(&program.effects.write))
    ));
    if !program.steps.is_empty() {
        dot.push_str("  input -> step_0;\n");
        for index in 0..program.steps.len() - 1 {
            dot.push_str(&format!("  step_{index} -> step_{};\n", index + 1));
        }
        dot.push_str(&format!(
            "  step_{} -> output_file;\n",
            program.steps.len() - 1
        ));
    }
    dot.push_str("  contract -> step_0 [label=\"orders\"];\n");
    dot.push_str("}\n");
    dot
}

fn render_semantic_graph_html(program: &DemoProgram, dot: &str) -> String {
    let step_count = program.steps.len();
    let pipeline_nodes = step_count + 2;
    let pipeline_width = 70 + pipeline_nodes * 178 + 90;
    let width = 1120usize.max(pipeline_width);
    let mut svg = String::new();

    svg.push_str(&svg_defs());
    svg.push_str(&svg_node(55, 70, 165, 78, "O source", "main.O", "source"));
    svg.push_str(&svg_node(
        270,
        70,
        185,
        78,
        "Semantic contract",
        &format!("{} / {}", program.group, program.policy),
        "contract",
    ));
    svg.push_str(&svg_node(
        505,
        70,
        165,
        78,
        "Lowered C",
        "normalize_pipeline.c",
        "lowered",
    ));
    svg.push_str(&svg_node(
        720,
        70,
        165,
        78,
        "Executable",
        "compiled artifact",
        "runtime",
    ));
    svg.push_str(&svg_node(
        935,
        70,
        165,
        78,
        "Receipt",
        "JSON + graph",
        "receipt",
    ));
    svg.push_str(&svg_edge(220, 109, 270, 109, "declares"));
    svg.push_str(&svg_edge(455, 109, 505, 109, "lowers"));
    svg.push_str(&svg_edge(670, 109, 720, 109, "builds"));
    svg.push_str(&svg_edge(885, 109, 935, 109, "records"));

    let y = 270;
    let node_w = 138;
    let gap = 40;
    let mut x = 70;
    svg.push_str(&svg_node(
        x,
        y,
        node_w,
        72,
        "read",
        &leaf_name(&program.effects.read),
        "effect",
    ));
    let mut edge_start = x + node_w;
    x += node_w + gap;
    for (index, step) in program.steps.iter().enumerate() {
        svg.push_str(&svg_node(
            x,
            y,
            node_w,
            72,
            &format!("step {}", index + 1),
            step,
            "step",
        ));
        svg.push_str(&svg_edge(edge_start, y + 36, x, y + 36, ""));
        edge_start = x + node_w;
        x += node_w + gap;
    }
    svg.push_str(&svg_node(
        x,
        y,
        node_w,
        72,
        "write",
        &leaf_name(&program.effects.write),
        "effect",
    ));
    svg.push_str(&svg_edge(edge_start, y + 36, x, y + 36, ""));
    svg.push_str(&svg_edge(362, 148, 362, 270, "orders"));

    let metric_y = 455;
    svg.push_str(&svg_node(
        70,
        metric_y,
        205,
        70,
        "coordination cost",
        "high",
        "cost",
    ));
    svg.push_str(&svg_node(
        320,
        metric_y,
        205,
        70,
        "type shape cost",
        "low",
        "cost",
    ));
    svg.push_str(&svg_node(
        570,
        metric_y,
        205,
        70,
        "effect safety cost",
        "medium",
        "cost",
    ));
    svg.push_str(&svg_node(
        820,
        metric_y,
        205,
        70,
        "intent cost",
        "low",
        "cost",
    ));

    format!(
        r#"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>{title}</title>
<style>
:root {{
  color-scheme: dark;
  --bg: #0a0a0b;
  --panel: #141414;
  --ink: #e6e2d6;
  --muted: #9c9788;
  --copper: #c48a5a;
  --steel: #3f5c6b;
  --green: #3d5f4a;
  --red: #8c2f23;
}}
body {{
  margin: 0;
  background: var(--bg);
  color: var(--ink);
  font: 14px/1.5 "Fira Code", Menlo, Consolas, monospace;
}}
main {{
  max-width: 1180px;
  margin: 0 auto;
  padding: 28px;
}}
h1 {{
  margin: 0 0 8px;
  font-size: 24px;
  font-weight: 700;
}}
p {{
  margin: 0 0 22px;
  color: var(--muted);
}}
svg {{
  width: 100%;
  height: auto;
  display: block;
  background: #101011;
  border: 1px solid #2a2926;
}}
.node rect {{
  fill: #161616;
  stroke: var(--copper);
  stroke-width: 1.5;
  rx: 6;
}}
.node.source rect {{ stroke: #5f7f8e; }}
.node.contract rect {{ stroke: var(--copper); }}
.node.lowered rect {{ stroke: #6c7a4f; }}
.node.runtime rect {{ stroke: #3d5f4a; }}
.node.receipt rect {{ stroke: #8c6dc8; }}
.node.effect rect {{ stroke: #3f5c6b; }}
.node.step rect {{ stroke: #c48a5a; }}
.node.cost rect {{ stroke: #8c2f23; }}
.title {{
  fill: var(--ink);
  font-size: 14px;
  font-weight: 700;
}}
.detail {{
  fill: var(--muted);
  font-size: 12px;
}}
.edge {{
  stroke: #5f7f8e;
  stroke-width: 1.7;
}}
.edge-label {{
  fill: var(--copper);
  font-size: 11px;
}}
pre {{
  margin-top: 20px;
  padding: 16px;
  overflow: auto;
  background: #111;
  border: 1px solid #2a2926;
  color: var(--muted);
}}
</style>
</head>
<body>
<main>
<h1>{heading}</h1>
<p>policy: {policy}; invariant: {invariant}; intent: {intent}</p>
<svg viewBox="0 0 {width} 560" role="img" aria-label="{aria}">
{svg}
</svg>
<pre>{dot}</pre>
</main>
</body>
</html>
"#,
        title = html_escape(&format!("{} semantic graph", program.group)),
        heading = html_escape(&format!("{} semantic graph", program.group)),
        policy = html_escape(&program.policy),
        invariant = html_escape(&program.invariant),
        intent = html_escape(&program.intent),
        width = width,
        aria = html_escape("O-Git semantic receipt graph"),
        svg = svg,
        dot = html_escape(dot),
    )
}

fn svg_defs() -> String {
    r##"<defs>
  <marker id="arrow" markerWidth="10" markerHeight="10" refX="8" refY="3" orient="auto" markerUnits="strokeWidth">
    <path d="M0,0 L0,6 L9,3 z" fill="#5f7f8e" />
  </marker>
</defs>
"##
    .to_string()
}

fn svg_node(
    x: usize,
    y: usize,
    w: usize,
    h: usize,
    title: &str,
    detail: &str,
    class: &str,
) -> String {
    format!(
        r#"<g class="node {class}">
  <rect x="{x}" y="{y}" width="{w}" height="{h}" />
  <text class="title" x="{title_x}" y="{title_y}">{title}</text>
  <text class="detail" x="{title_x}" y="{detail_y}">{detail}</text>
</g>
"#,
        class = html_escape(class),
        x = x,
        y = y,
        w = w,
        h = h,
        title_x = x + 14,
        title_y = y + 29,
        detail_y = y + 53,
        title = html_escape(title),
        detail = html_escape(detail),
    )
}

fn svg_edge(x1: usize, y1: usize, x2: usize, y2: usize, label: &str) -> String {
    let label_text = if label.is_empty() {
        String::new()
    } else {
        format!(
            r#"<text class="edge-label" x="{x}" y="{y}">{label}</text>
"#,
            x = (x1 + x2) / 2 - 18,
            y = y1.saturating_sub(8),
            label = html_escape(label),
        )
    };
    format!(
        r#"<line class="edge" x1="{x1}" y1="{y1}" x2="{x2}" y2="{y2}" marker-end="url(#arrow)" />
{label_text}"#
    )
}

fn leaf_name(path: &str) -> String {
    path.rsplit('/').next().unwrap_or(path).to_string()
}

fn dot_escape(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
}

fn html_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_demo_metadata() {
        let program = parse_demo_program(DEMO_SOURCE).unwrap();
        assert_eq!(program.group, "normalize_pipeline");
        assert_eq!(program.policy, "lazy");
        assert_eq!(program.invariant, "output_count == input_count");
        assert_eq!(program.effects.read, "examples/group_pipeline/input.txt");
        assert_eq!(program.effects.write, "examples/group_pipeline/output.txt");
        assert_eq!(program.steps, vec!["load", "normalize", "emit"]);
    }

    #[test]
    fn lazy_to_eager_policy_change_reports_demand_model() {
        let old = parse_demo_program(DEMO_SOURCE).unwrap();
        let new = parse_demo_program(DEMO_EAGER_SOURCE).unwrap();
        let changes = semantic_changes(&old, &new);
        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].category, "policy");
        assert_eq!(changes[0].meaning, "runtime demand model changed");
        assert!(changes[0]
            .implications
            .iter()
            .any(|s| s.contains("file reads may happen before")));
    }

    #[test]
    fn renders_visible_graph_artifacts() {
        let program = parse_demo_program(DEMO_SOURCE).unwrap();
        let dot = render_semantic_dot(&program);
        assert!(dot.contains("policy: lazy"));
        assert!(dot.contains("step_0 -> step_1"));

        let html = render_semantic_graph_html(&program, &dot);
        assert!(html.contains("normalize_pipeline semantic graph"));
        assert!(html.contains("output_count == input_count"));
        assert!(html.contains("<svg"));
    }
}
