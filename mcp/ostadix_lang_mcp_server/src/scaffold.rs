//! Program templates so agents start from working shapes.

pub fn list_templates() -> &'static [(&'static str, &'static str)] {
    &[
        ("hello", "Minimal python 1+1 (smoke shape)"),
        ("python", "Python-only with __oval_result__"),
        ("nested", "Bind python value into bash"),
        ("polyglot", "python + bash + javascript"),
        ("html_py", "html wrapping nested python"),
        ("bash", "Shell block"),
        ("search_tool", "a18re-style research tool skeleton"),
        ("blank", "Header-only blank program"),
    ]
}

pub fn render(template: &str, name: Option<&str>) -> Result<String, String> {
    let title = name.unwrap_or("prog");
    let body = match template.trim().to_lowercase().as_str() {
        "hello" => HELLO.into(),
        "python" => PYTHON.replace("{{name}}", title),
        "nested" => NESTED.into(),
        "polyglot" => POLYGLOT.into(),
        "html_py" | "html" => HTML_PY.into(),
        "bash" | "shell" => BASH.into(),
        "search_tool" | "search" => SEARCH_TOOL.replace("{{name}}", title),
        "blank" => format!(
            "# {title}.O — Ostadix-lang program\n# Run via MCP o_run (absolute backends).\n# Never put $VAR inside sources (O splices $IDENT).\n\n"
        ),
        other => {
            return Err(format!(
                "unknown template `{other}`. Valid: {}",
                list_templates()
                    .iter()
                    .map(|(k, _)| *k)
                    .collect::<Vec<_>>()
                    .join(", ")
            ))
        }
    };
    Ok(body)
}

const HELLO: &str = r#"# hello.O — minimal smoke (expect 2)
python^(
__oval_result__ = 1 + 1
)_python
"#;

const PYTHON: &str = r#"# {{name}}.O — python result
python^(
# set the value O should receive
__oval_result__ = {"ok": True, "n": 42}
)_python
"#;

const NESTED: &str = r#"# nested.O — bind across languages
let n = python^(
__oval_result__ = 21
)_python
bash^(
echo "double is $(( n * 2 ))"
)_bash
"#;

const POLYGLOT: &str = r#"# polyglot.O — python + bash + javascript
let msg = python^(
__oval_result__ = "hello from python"
)_python
bash^(
echo "bash saw: $msg"
)_bash
javascript^(
console.log("js saw:", msg)
)_javascript
"#;

const HTML_PY: &str = r#"# html_py.O — html with nested python
python^(
page = html^(
<div>
  <p>The computed number is python^(
__oval_result__ = 20 + 22
)_python.</p>
</div>
)_html
__oval_result__ = page
)_python
"#;

const BASH: &str = r#"# bash.O
bash^(
echo "uname=$(uname -s)"
echo "pwd=$(pwd)"
)_bash
"#;

const SEARCH_TOOL: &str = r#"# {{name}}.O — a18re search tool skeleton
# Run: o_search_run name={{name}}  (or o_run with A18_WORK set)
# Prefer absolute paths; never embed $O_BACKENDS_DIR in this file.

python^(
import os, json, pathlib

work = pathlib.Path(os.environ.get("A18_WORK", os.path.expanduser("~/a18re")))
out = {
    "tool": "{{name}}",
    "work": str(work),
    "exists": work.is_dir(),
    "hint": "implement corpus search / analysis here",
}
print(json.dumps(out, indent=2))
__oval_result__ = out
)_python
"#;
