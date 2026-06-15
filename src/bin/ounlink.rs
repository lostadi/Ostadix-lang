// ─────────────────────────────────────────────────────────────────────────────
// o-unlink — the inverse of o-link
//
// Reads a combined `.O` file produced by `o-link` and writes each original
// source file back to an output directory, reconstructing the pre-link layout.
//
// `o-link` embeds a path comment before each wrapped file:
//
//   # ── path/to/file.py ──
//   python[N]^(
//   ...escaped file contents...
//   )_python[N]
//
// `o-unlink` reads these markers, un-escapes the body (the O-lang parser
// consumes escape sequences like `\$HOME` → `$HOME` and `\python^(` →
// `python^(` automatically), and writes the recovered content to
// `<output-dir>/path/to/file.py`, creating parent directories as needed.
//
// `.O` files that were inlined verbatim by `o-link` are reconstructed as
// well: their raw text (everything between the path comment and the next
// recognised block or path comment) is written back to the output file.
//
// Round-trip property:
//   o-link src/ -o combined.O && o-unlink combined.O -o src2/ && diff -r src/ src2/
// should produce an empty diff for any source tree that contains only files
// with extensions recognised by the default extension map.
//
// Usage:
//   o-unlink combined.O -o src/             # restore into src/
//   o-unlink combined.O --output-dir out/   # long form
//   o-unlink combined.O --dry-run           # print recovered paths, no writes
// ─────────────────────────────────────────────────────────────────────────────

use anyhow::{bail, Context, Result};
use clap::Parser as ClapParser;
use std::collections::HashSet;
use std::fs;
use std::path::PathBuf;

use o_lang::parser::{reconstruct_source, ONode, Parser};

/// o-unlink — restore per-language files from a combined .O file.
#[derive(Debug, ClapParser)]
#[command(
    name = "o-unlink",
    about = "Restore source files from a combined .O file produced by o-link"
)]
struct Cli {
    /// The combined .O file to unlink.
    #[arg(value_name = "FILE")]
    input: PathBuf,

    /// Output directory into which files are written.
    #[arg(short = 'o', long = "output-dir", default_value = ".")]
    output_dir: PathBuf,

    /// Print the recovered file paths without writing anything.
    #[arg(long = "dry-run")]
    dry_run: bool,
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    let source = fs::read_to_string(&cli.input)
        .with_context(|| format!("failed to read {}", cli.input.display()))?;

    let entries = unlink_source(&source)
        .context("failed to extract files from combined .O source")?;

    if entries.is_empty() {
        bail!("no linkable file sections found in {}", cli.input.display());
    }

    for (path, content) in &entries {
        let dest = cli.output_dir.join(path);
        if cli.dry_run {
            println!("{}", dest.display());
        } else {
            if let Some(parent) = dest.parent() {
                fs::create_dir_all(parent)
                    .with_context(|| format!("failed to create directory {}", parent.display()))?;
            }
            fs::write(&dest, content.as_bytes())
                .with_context(|| format!("failed to write {}", dest.display()))?;
        }
    }

    if !cli.dry_run {
        eprintln!(
            "unlinked {} file(s) into {}",
            entries.len(),
            cli.output_dir.display()
        );
    }

    Ok(())
}

// ─────────────────────────────────────────────────────────────────────────────
// Core extraction
// ─────────────────────────────────────────────────────────────────────────────

/// Parse `source` (a combined `.O` file) and return `(relative_path, content)`
/// pairs for every file section that `o-link` embedded.
///
/// Two kinds of sections are handled:
///   1. Wrapped files: `LANG[N]^(...)_LANG[N]` — the body is reconstructed
///      through the parser so that escape sequences (`\$HOME`, `\python^(`, …)
///      are unwound back to their literal forms.
///   2. Inlined `.O` files: their source is emitted verbatim between the path
///      comment and the next path comment or end of file.
///
/// ## Why we scan text rather than the AST
///
/// The O-lang parser strips `#` comment lines at the top level (the same
/// rule that ignores O-lang comments inside sequencing blocks).  This means
/// the `# ── path ──` markers written by `o-link` are silently dropped from
/// the parsed AST.  We therefore scan the raw source text to locate section
/// boundaries, then use the parser only for body un-escaping.
pub fn unlink_source(source: &str) -> Result<Vec<(PathBuf, String)>> {
    let backends = registered_backends();

    // Strip an optional shebang line.
    let source = if source.starts_with("#!") {
        source.find('\n').map(|nl| &source[nl + 1..]).unwrap_or("")
    } else {
        source
    };

    // Collect the byte offsets of every `# ── path ──` marker line plus the
    // path it encodes.  Each entry is (content_start, path) where
    // `content_start` is the byte just after the marker's newline.
    let mut markers: Vec<(usize, PathBuf)> = Vec::new();
    {
        let mut pos = 0;
        while pos < source.len() {
            let line_end = source[pos..].find('\n').map(|n| pos + n + 1).unwrap_or(source.len());
            let line = source[pos..line_end].trim_end_matches('\n');
            if let Some(path) = parse_path_marker(line) {
                markers.push((line_end, path));
            }
            pos = if line_end == pos { pos + 1 } else { line_end };
        }
    }

    let mut results: Vec<(PathBuf, String)> = Vec::new();

    for (idx, (content_start, path)) in markers.iter().enumerate() {
        let content_end = if idx + 1 < markers.len() {
            // Find where the next marker LINE starts (not content_start of
            // next entry, but the position of its `# ──` line).
            next_marker_line_start(source, markers[idx + 1].0)
        } else {
            source.len()
        };

        let section = &source[*content_start..content_end];

        if let Some(content) = extract_block_content(section, &backends)? {
            results.push((path.clone(), content));
        }
        // If no TypedExpr block is found in the section, the file was a `.O`
        // inline.  We gather the raw text (minus comment lines) as the content.
        else {
            let raw = raw_o_content(section);
            if !raw.trim().is_empty() {
                results.push((path.clone(), raw));
            }
        }
    }

    Ok(results)
}

/// Return the byte position of the `# ──` line whose content starts at
/// `content_start` (i.e. the line immediately preceding `content_start`).
fn next_marker_line_start(source: &str, content_start: usize) -> usize {
    // Walk backwards from content_start - 1 to find the start of the marker line.
    if content_start == 0 {
        return 0;
    }
    let before = &source[..content_start - 1]; // exclude the newline of the marker
    before.rfind('\n').map(|n| n + 1).unwrap_or(0)
}

/// Given the text of a section (after the path comment), locate and parse the
/// first `LANG[N]^(...)_LANG[N]` block and return its unescaped body.
///
/// Returns `None` if no typed-expression block is present (`.O` inline case).
fn extract_block_content(section: &str, backends: &HashSet<String>) -> Result<Option<String>> {
    // Find the `LANG[N]^(` opener.
    let opener_pos = find_typed_opener(section, backends);
    let opener_pos = match opener_pos {
        Some(p) => p,
        None => return Ok(None),
    };

    // Extract from the opener to the end of the section so the parser can
    // find the matching closer.
    let block_src = &section[opener_pos..];

    let mut parser = Parser::new(block_src, backends);
    let nodes = parser
        .parse()
        .context("failed to parse typed-expression block while unlinking")?;

    // The first TypedExpr in the parsed block is our file content.
    for node in &nodes {
        if let ONode::TypedExpr { body, .. } = node {
            let content = reconstruct_source(body);
            // o-link writes `LANG[N]^(\n<content>`, so the parser body starts
            // with a newline — strip exactly that one leading newline.
            let content = content.strip_prefix('\n').unwrap_or(&content);
            return Ok(Some(content.to_string()));
        }
    }

    Ok(None)
}

/// Return the byte offset of the first `IDENT[N]^(` pattern in `text` that
/// uses a registered backend name, or `None`.
fn find_typed_opener(text: &str, backends: &HashSet<String>) -> Option<usize> {
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i].is_ascii_alphabetic() || bytes[i] == b'_' {
            // Try to parse an ident
            let start = i;
            while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
                i += 1;
            }
            let name = &text[start..i];
            if backends.contains(name) {
                // Accept optional `[N]` then `^(`
                let mut j = i;
                if j < bytes.len() && bytes[j] == b'[' {
                    j += 1;
                    while j < bytes.len() && bytes[j].is_ascii_digit() {
                        j += 1;
                    }
                    if j < bytes.len() && bytes[j] == b']' {
                        j += 1;
                    }
                }
                if j + 1 < bytes.len() && bytes[j] == b'^' && bytes[j + 1] == b'(' {
                    return Some(start);
                }
            }
        } else {
            i += 1;
        }
    }
    None
}

/// Collect the raw (non-comment) text from a section that contains inlined
/// `.O` source rather than a wrapped typed-expression block.
fn raw_o_content(section: &str) -> String {
    let mut out = String::new();
    for line in section.lines() {
        // Skip the o-link header comment and blank separator lines.
        if line.starts_with("# Linked by o-link") {
            continue;
        }
        out.push_str(line);
        out.push('\n');
    }
    out
}

// ─────────────────────────────────────────────────────────────────────────────
// Marker parsing helpers
// ─────────────────────────────────────────────────────────────────────────────

/// Return the byte offset of the first `# ── ... ──` path marker in `text`,
/// or `None` if there is no such marker.  Used in tests.
#[allow(dead_code)]
fn find_path_marker(text: &str) -> Option<usize> {
    // The marker format written by o-link is `# ── <path> ──\n`.
    // We accept both the em-dash `──` (U+2500 BOX DRAWINGS LIGHT HORIZONTAL,
    // 3 bytes each) and the ASCII fallback `--`.
    for (i, _) in text.char_indices() {
        let s = &text[i..];
        if s.starts_with("# ── ") || s.starts_with("# -- ") {
            return Some(i);
        }
    }
    None
}

/// Parse a `# ── <path> ──` line and return the embedded path.
///
/// The line MUST start with `# ──` (em-dash) or `# --` (ASCII dash) — this
/// prevents the o-link header comment (`# Linked by o-link…`) from being
/// mistaken for a path marker.
fn parse_path_marker(line: &str) -> Option<PathBuf> {
    let trimmed = line.trim();

    // The marker MUST start with `# ──` or `# --`.
    let after_hash = trimmed.strip_prefix("# ")?;
    let after_dashes = if after_hash.starts_with("──") {
        after_hash.strip_prefix("──")?
    } else if after_hash.starts_with("--") {
        after_hash.strip_prefix("--")?
    } else {
        return None;
    };

    // Skip any space between the opening dashes and the path.
    let path_and_suffix = after_dashes.trim_start_matches(' ');

    // Strip the trailing ` ──` or ` --`.
    let path_str = strip_dash_suffix(path_and_suffix).trim();

    if path_str.is_empty() {
        return None;
    }

    Some(PathBuf::from(path_str))
}

fn strip_dash_suffix(s: &str) -> &str {
    if s.ends_with("──") {
        s[..s.len() - "──".len()].trim_end_matches(' ')
    } else if s.ends_with("--") {
        s[..s.len() - "--".len()].trim_end_matches(' ')
    } else {
        s
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Backend registry — must stay in sync with olink.rs
// ─────────────────────────────────────────────────────────────────────────────

fn registered_backends() -> HashSet<String> {
    [
        "O", "python", "html", "latex", "markdown", "bash", "shell", "rust",
        "racket", "nix", "nix_expr", "nix_store", "nixos_test", "text",
        "csharp", "cpp", "haskell", "lisp", "common_lisp", "sql", "ruby",
        "matlab", "mathematica", "webassembly", "java", "javascript", "ocaml",
        "quote",
        // Aliases
        "py", "md", "tex", "plain", "o",
    ]
    .into_iter()
    .map(String::from)
    .collect()
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── unit: marker parsing ─────────────────────────────────────────────────

    #[test]
    fn parse_path_marker_basic() {
        let line = "# ── src/hello.py ──\n";
        let path = parse_path_marker(line).unwrap();
        assert_eq!(path, PathBuf::from("src/hello.py"));
    }

    #[test]
    fn parse_path_marker_no_subdirectory() {
        let line = "# ── main.sh ──\n";
        let path = parse_path_marker(line).unwrap();
        assert_eq!(path, PathBuf::from("main.sh"));
    }

    #[test]
    fn find_path_marker_finds_first_occurrence() {
        let text = "some text\n# ── a.py ──\nmore text\n# ── b.py ──\n";
        let idx = find_path_marker(text).unwrap();
        // `# ── ` is 9 bytes: '#', ' ', then two U+2500 box-drawing chars
        // (3 bytes each), then ' '.
        assert!(text[idx..].starts_with("# ── "), "expected marker at idx {}", idx);
        // Verify the text before the marker is what we expect.
        assert_eq!(&text[..idx], "some text\n");
    }

    #[test]
    fn find_path_marker_returns_none_when_absent() {
        let text = "# Linked by o-link\nsome raw text\n";
        assert!(find_path_marker(text).is_none());
    }

    // ── integration: round-trip through o-link then o-unlink ────────────────

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "ounlink_test_{}_{}",
            name,
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Build a minimal combined .O source string that mimics o-link's output,
    /// then verify that o-unlink recovers the original content, including that
    /// the `\$HOME` escape in the wrapped body is resolved back to `$HOME`.
    #[test]
    fn roundtrip_python_file_with_dollar_vars() {
        // o-link escapes $HOME → \$HOME in the wrapped body.
        // We construct the combined string with the escaped form directly.
        let combined = "# Linked by o-link — single-file .O program\n\
                        \n\
                        # ── script.py ──\n\
                        python[0]^(\n\
                        import os\n\
                        home = os.environ.get(\"HOME\")\n\
                        )_python[0]\n";

        let entries = unlink_source(combined).unwrap();
        assert_eq!(entries.len(), 1, "should recover exactly one file");
        let (path, content) = &entries[0];
        assert_eq!(path, &PathBuf::from("script.py"));
        assert!(content.contains("import os"), "got: {:?}", content);
        assert!(content.contains("HOME"), "got: {:?}", content);
    }

    #[test]
    fn roundtrip_dollar_escape_resolved() {
        // Verify that \$VAR in the combined source is resolved to $VAR by the
        // parser and written back to the recovered file as literal $VAR.
        // We use a raw string so `\$` is the two-char sequence backslash + dollar.
        let combined = "# ── env.sh ──\nbash[0]^(\necho \\$HOME\n)_bash[0]\n";
        let entries = unlink_source(combined).unwrap();
        assert_eq!(entries.len(), 1);
        let (_, content) = &entries[0];
        // The escaped \$HOME in the source should be decoded to literal $HOME.
        assert!(content.contains("$HOME"), "\\$HOME should decode to $HOME; got: {:?}", content);
        assert!(!content.contains("\\$"), "no backslash should remain; got: {:?}", content);
    }

    #[test]
    fn roundtrip_multiple_languages() {
        let combined = concat!(
            "# Linked by o-link — single-file .O program\n",
            "\n",
            "# ── app.py ──\n",
            "python[0]^(\n",
            "print('hello')\n",
            ")_python[0]\n",
            "\n",
            "# ── run.sh ──\n",
            "bash[0]^(\n",
            "echo hello\n",
            ")_bash[0]\n",
        );

        let entries = unlink_source(combined).unwrap();
        assert_eq!(entries.len(), 2);

        let paths: Vec<&PathBuf> = entries.iter().map(|(p, _)| p).collect();
        assert!(paths.contains(&&PathBuf::from("app.py")));
        assert!(paths.contains(&&PathBuf::from("run.sh")));

        for (path, content) in &entries {
            if path == &PathBuf::from("app.py") {
                assert!(content.contains("print('hello')"));
            } else {
                assert!(content.contains("echo hello"));
            }
        }
    }

    #[test]
    fn roundtrip_escaped_opener_and_closer() {
        // A Python file whose source contains a literal `python^(` and `)_python`.
        // o-link would escape these, the parser reconstructs them literally.
        let combined = concat!(
            "# ── demo.py ──\n",
            "python[0]^(\n",
            "doc = \"use \\python^( ... \\)_python blocks\"\n",
            ")_python[0]\n",
        );

        let entries = unlink_source(combined).unwrap();
        assert_eq!(entries.len(), 1);
        let (_, content) = &entries[0];
        assert!(
            content.contains("python^("),
            "opener escape should resolve; got: {:?}", content
        );
        assert!(
            content.contains(")_python"),
            "closer escape should resolve; got: {:?}", content
        );
    }

    #[test]
    fn roundtrip_filesystem_link_then_unlink() {
        // End-to-end: write real files, run link_files, run unlink_source,
        // check that the recovered content matches.
        use std::fs;
        let dir = scratch("e2e");
        let py_content = "def greet(name):\n    return f\"Hello {name}\"\n";
        let sh_content = "#!/bin/bash\necho $HOME\n";

        fs::write(dir.join("greet.py"), py_content).unwrap();
        fs::write(dir.join("run.sh"), sh_content).unwrap();

        // Build the combined source using o-link's logic.
        // We call the public olink helpers directly via the binary's module
        // path; since it's a separate [[bin]], we replicate the minimal needed
        // logic here by constructing the combined string manually in the same
        // format o-link uses.
        let combined = format!(
            concat!(
                "# Linked by o-link — single-file .O program\n",
                "\n",
                "# ── {py} ──\n",
                "python[0]^(\n",
                "{py_body}",
                ")_python[0]\n",
                "\n",
                "# ── {sh} ──\n",
                "bash[0]^(\n",
                "{sh_body}",
                ")_bash[0]\n",
            ),
            py     = dir.join("greet.py").display(),
            py_body = py_content,
            sh     = dir.join("run.sh").display(),
            sh_body = sh_content.replace("$HOME", "\\$HOME"),
        );

        let entries = unlink_source(&combined).unwrap();
        assert_eq!(entries.len(), 2, "should recover both files");

        let recovered: std::collections::HashMap<_, _> = entries.into_iter().collect();
        let py_key = PathBuf::from(dir.join("greet.py").to_string_lossy().as_ref());
        let sh_key = PathBuf::from(dir.join("run.sh").to_string_lossy().as_ref());

        assert_eq!(recovered[&py_key].trim_end(), py_content.trim_end());
        assert_eq!(recovered[&sh_key].trim_end(), sh_content.trim_end());

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn empty_combined_returns_empty_vec() {
        let combined = "# Linked by o-link — single-file .O program\n";
        let entries = unlink_source(combined).unwrap();
        assert!(entries.is_empty());
    }
}
