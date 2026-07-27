//! Agent endowment: how to build with O-lang using this MCP.

pub const AGENT_GUIDE: &str = r#"# O-lang / Ostadix-lang — Agent Build Playbook

You have the **olang** MCP (Rust `ostadix-mcp`). Prefer these tools over raw shell.

## Mental model (30 seconds)

1. **Language is syntax**, not file type: `python^(…)_python`, `bash^(…)_bash`, nest freely.
2. **`$name` is O splice** — host shell vars need `\$PATH` or avoid `$` inside blocks.
3. **Backends path must be absolute** when cwd is not the lang root. This MCP always injects absolute backends.
4. **Pipeline**: draft → `o_eval`/`o_run` → debug with `o_ir` → multi-file via `o_link` → ship with `o_aot`.

## First actions every session

```
1. o_env          # confirm paths
2. o_smoke        # expect SMOKE_OK and 2
3. o_guide topic=quick | o_cheatsheet
```

## Tool map (what to call when)

| Goal | Tool | Notes |
|------|------|-------|
| Learn syntax / pitfalls | `o_cheatsheet`, `o_guide` | topics: quick, syntax, pipeline, pitfalls, a18re |
| Check install | `o_env`, `o_doctor`, `o_smoke` | doctor lists shims |
| Snippet without a file | `o_eval` | writes temp .O, runs, cleans up |
| Write a program | `o_write` or `o_scaffold` | scaffold templates are battle-tested |
| Run a .O file | `o_run` | always absolute backends |
| Debug plan / graph | `o_ir`, `o_dot` | ir = OIR+plan; dot = Graphviz |
| Compile native binary | `o_aot` | only after script/run works |
| olangc any target | `o_olangc` | ir\|dot\|script\|wasm\|binary |
| Multi-file project | `o_link` | then o_run the combined .O |
| Restore linked sources | `o_unlink` | lossless |
| Discover examples | `o_examples`, `o_read_example` | copy patterns, don't invent |
| List host languages | `o_backends` | *_shim.py |
| a18re research tools | `o_list_search`, `o_search_run` | search/*.O |
| Plan a feature | `o_plan` | returns ordered tool steps |
| Explain a failure | `o_diagnose` | paste error text |

## Recommended agent loop

1. **Clarify**: hosted `.O` (this MCP) vs freestanding `.oc` (ocorec / olang-ocore skill — different).
2. **Scaffold** closest template: `hello`, `python`, `nested`, `polyglot`, `html_py`, `bash`, `search_tool`.
3. **`o_eval` or `o_run`** — show real exit + stdout. Never claim success without output.
4. If multi-file: put sources in a dir → `o_link` → `o_run` combined.
5. Stuck on order/bindings: `o_ir` then fix.
6. Ship: `o_aot` with `-o` path (AOT embeds runtime; host still needs Python etc. for those blocks).

## Minimal syntax

```O
python^(
__oval_result__ = 1 + 1
)_python
```

```O
let n = python^(
__oval_result__ = 21
)_python
javascript^(
console.log(n * 2)
)_javascript
```

```O
html^(
  <p>python^(
__oval_result__ = sum(range(10))
)_python</p>
)_html
```

- Opener/closer IDENT must match exactly (`python`…`_python`).
- Persistent env: `python[1]^(…)_python[1]`.
- Python return value: set `__oval_result__` (else stdout → text).
- Aliases: py→python, md→markdown, tex→latex, plain→text, o→O.
- Shebang: `#!/usr/bin/env o`

## Hard anti-patterns (will break you)

| Don't | Do |
|-------|-----|
| Pass bare `backends` from random cwd | Use `o_run` / `o_eval` (absolute) |
| Put `$O_BACKENDS_DIR` or `$HOME` inside .O | O splices `$IDENT` — use paths outside or escape `\$` |
| Pass literal string `O_BACKENDS_DIR` as argv | Absolute path only |
| Invent FFI / one-lang-per-file rules | Nest typed parens / o_link |
| Use olangc for `.oc` kernel code | Different pipeline (ocorec) |
| Link whole repo incl. target/ | `.olinkignore` + o_link skips |
| Claim success without command output | Always return exit + stdout |

## Scaffold templates (`o_scaffold`)

- `hello` — smallest smoke
- `python` — pure python result
- `nested` — binding across langs
- `polyglot` — python + bash + js
- `html_py` — html wrapping python
- `bash` — shell block
- `search_tool` — a18re-style research tool skeleton
- `blank` — empty with comment header

## Rebuild toolchain (if bins missing)

Agents should tell the user (or run via shell if permitted):

```bash
export O_LANG_ROOT=~/Ostadix-lang
o pull          # or: cd $O_LANG_ROOT && bash ./setup.sh -y --minimal
```

Prefer setup.sh over bare `cargo build` for full install.

## a18re search tools

Work root: `$A18_WORK` or `~/a18re`. Programs live in `search/*.O`.
Call `o_list_search` then `o_search_run` with name like `sptm_retype_catalog`.

## Success criteria

- `o_smoke` → SMOKE_OK
- Your program → exit=0 and expected values in stdout
- Multi-file → o_link validates, o_run succeeds
- Shipped binary → o_aot exit=0 and binary runs
"#;

pub const CHEATSHEET: &str = r#"# O-lang cheatsheet (agent pocket card)

## Blocks
  LANG^( body )_LANG
  LANG[n]^( body )_LANG[n]     # persistent env n
  let x = LANG^(…)_LANG         # bind O value

## Python
  __oval_result__ = value       # return to O (preferred)

## Splice
  $name     → O binding (DANGER for shell: use \$PATH)
  Never put $O_BACKENDS_DIR inside sources

## Run (via MCP)
  o_eval { source }             # snippet
  o_run  { path }               # file
  o_ir   { path }               # OIR + plan + HGraph
  o_aot  { path, output }       # native binary

## Link
  o_link { inputs: ["src/"], output: "app.O" }
  o_unlink { path, output_dir }

## Pick tool
  run one file     → o_run
  try idea fast    → o_eval / o_scaffold
  multi-file       → o_link then o_run
  debug order      → o_ir
  visualize        → o_dot
  ship binary      → o_aot
  learn            → o_guide / o_examples / o_read_example
"#;

pub fn topic_guide(topic: &str) -> String {
    let t = topic.trim().to_lowercase();
    match t.as_str() {
        "quick" | "" => format!(
            "{}\n\n--- full guide available via o_guide topic=full ---\n",
            CHEATSHEET
        ),
        "full" | "all" | "playbook" => AGENT_GUIDE.to_string(),
        "syntax" => r#"# Syntax

Form: IDENT^( body )_IDENT  or IDENT[n]^( body )_IDENT[n]

Rules:
- Opener and closer IDENT must match exactly.
- Unregistered IDENTs are never openers (so Python `2 ** (x)` stays safe).
- Nesting is the composition model — no pairwise FFIs.
- `$name` splices O bindings into host source before execution.
- Escape host dollars: `\$PATH`, `\$HOME`.
- Python: set `__oval_result__` to return a structured value.
- Aliases: py→python, md→markdown, tex→latex, plain→text, shell/bash/sh share shell backend.

Example nested:
```
let n = python^(
__oval_result__ = 40 + 2
)_python
bash^(
echo "answer is $n"
)_bash
```
"#
        .into(),
        "pipeline" | "workflow" => r#"# Build pipeline

1. o_env + o_smoke
2. o_scaffold template=… path=…
3. o_run path=…  (or o_eval for snippets)
4. On failure: o_diagnose error=…  and o_ir path=…
5. Multi-file: o_link inputs=[…] output=app.O → o_run app.O
6. Ship: o_aot path=app.O output=./bin
7. Optional: o_dot path=… → save DOT, render with graphviz if needed

Never skip step 3 verification.
"#
        .into(),
        "pitfalls" | "errors" | "antipatterns" => r#"# Pitfalls

1. relative backends: `O prog.O backends` from wrong cwd → shim not found
   Fix: use o_run / o_eval

2. $VAR inside .O: O splices $IDENT before host runs
   Fix: absolute paths outside sources, or \$escape

3. literal backends argv "O_BACKENDS_DIR" without expansion
   Fix: never type that string as path

4. linking tests/setup into app.O
   Fix: .olinkignore; o_link --verbose-skips

5. AOT before script works
   Fix: o_run or o_olangc target=script first

6. using olangc for .oc freestanding code
   Fix: ocorec + olang-ocore skill
"#
        .into(),
        "a18re" | "search" => r#"# a18re research tools

Root: $A18_WORK or ~/a18re
Programs: search/<name>.O

MCP:
  o_list_search
  o_search_run name=sptm_retype_catalog
  o_search_run name=nscramble_mine
  o_search_run name=lab_pipeline

Always use o_search_run (injects absolute backends + A18_WORK).
Scaffold new tools with o_scaffold template=search_tool.
"#
        .into(),
        "link" => r#"# o-link / o-unlink

Link codebase → one .O:
  o_link inputs=["src/"] output="app.O"
  o_link inputs=["a.py","b.html"] output="combo.O" shebang=true

Then:
  o_run path=app.O
  o_ir path=app.O
  o_aot path=app.O output=./app

Unlink:
  o_unlink path=app.O output_dir=/tmp/restored

Notes:
- Linked .O executes every selected executable block (literal).
- Exclude tests with .olinkignore.
- Prefer project mode for a single directory.
"#
        .into(),
        other => format!(
            "Unknown topic `{other}`.\n\nValid topics: quick, full, syntax, pipeline, pitfalls, a18re, link\n\n{}",
            CHEATSHEET
        ),
    }
}

pub fn plan_for(task: &str) -> String {
    let t = task.to_lowercase();
    let mut steps: Vec<String> = vec![
        "0. o_env — confirm O_LANG_ROOT / backends / bins".into(),
        "1. o_smoke — must SMOKE_OK".into(),
    ];
    if t.contains("snippet") || t.contains("try") || t.contains("quick") {
        steps.push("2. o_eval source=<your O source>".into());
        steps.push("3. iterate on source until exit=0".into());
    } else if t.contains("link") || t.contains("multi") || t.contains("project") {
        steps.push("2. Arrange sources under a directory (exclude tests)".into());
        steps.push("3. o_link inputs=[dir] output=app.O".into());
        steps.push("4. o_run path=app.O".into());
        steps.push("5. o_ir path=app.O if order wrong".into());
        steps.push("6. o_aot path=app.O output=./app if shipping".into());
    } else if t.contains("aot") || t.contains("binary") || t.contains("ship") {
        steps.push("2. Ensure o_run succeeds first".into());
        steps.push("3. o_aot path=prog.O output=./prog".into());
        steps.push("4. Run ./prog and verify output".into());
    } else if t.contains("a18") || t.contains("search") || t.contains("research") {
        steps.push("2. o_list_search".into());
        steps.push("3. o_search_run name=<tool>".into());
        steps.push("4. Or o_scaffold template=search_tool path=search/new.O".into());
    } else if t.contains("debug") || t.contains("fail") || t.contains("error") {
        steps.push("2. o_diagnose error=<paste>".into());
        steps.push("3. o_ir path=prog.O".into());
        steps.push("4. o_read_example name=nested_splice (or closest)".into());
        steps.push("5. Fix + o_run".into());
    } else {
        steps.push("2. o_examples query=<keyword> — find a pattern".into());
        steps.push("3. o_scaffold template=… path=prog.O".into());
        steps.push("4. o_write / edit source".into());
        steps.push("5. o_run path=prog.O".into());
        steps.push("6. o_ir if needed; o_aot if shipping".into());
    }
    steps.push("N. Always report real exit codes and stdout snippets".into());
    format!(
        "# Plan for: {task}\n\n{}\n\nSee also: o_guide topic=pipeline\n",
        steps
            .iter()
            .map(|s| format!("- {s}"))
            .collect::<Vec<_>>()
            .join("\n")
    )
}

pub fn diagnose(error: &str) -> String {
    let e = error.to_lowercase();
    let mut hits: Vec<&str> = Vec::new();
    if e.contains("backends")
        || e.contains("shim")
        || e.contains("no such file")
        || e.contains("python_shim")
    {
        hits.push(
            "Backends path issue → use o_run/o_eval (absolute backends). Check o_env and o_doctor.",
        );
    }
    if e.contains("splice") || e.contains("$") || e.contains("unbound") {
        hits.push(
            "Possible $IDENT splice → remove `$VAR` from inside .O or escape as `\\$VAR`. Never embed $O_BACKENDS_DIR in sources.",
        );
    }
    if e.contains("parse") || e.contains("opener") || e.contains("closer") {
        hits.push(
            "Parse/opener mismatch → ensure LANG^(…)_LANG match exactly; check nested parens.",
        );
    }
    if e.contains("timeout") {
        hits.push("Timeout → raise timeout_secs; check infinite loops / waiting on network.");
    }
    if e.contains("permission") || e.contains("eacces") {
        hits.push("Permissions → check file modes and sandbox; o_aot output dir must be writable.");
    }
    if e.contains("cargo") || e.contains("link") && e.contains("error") {
        hits.push(
            "AOT/build failure → first o_run or o_olangc target=script; then o_aot with keep_build if debugging.",
        );
    }
    if hits.is_empty() {
        hits.push("No specific rule matched. Do: o_ir on the program, o_read_example of a similar example, re-run with o_run, paste ir + stderr.");
    }
    format!(
        "# Diagnose\n\nInput error (truncated):\n```\n{}\n```\n\nSuggestions:\n{}\n\nNext tools: o_ir, o_doctor, o_cheatsheet, o_guide topic=pitfalls\n",
        truncate_err(error, 1200),
        hits.iter()
            .enumerate()
            .map(|(i, h)| format!("{}. {h}", i + 1))
            .collect::<Vec<_>>()
            .join("\n")
    )
}

fn truncate_err(s: &str, max: usize) -> &str {
    if s.len() <= max {
        s
    } else {
        &s[..max]
    }
}
