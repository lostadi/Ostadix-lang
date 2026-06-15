// ─────────────────────────────────────────────────────────────────────────────
// o-link — the O-lang linker / combiner compiler
//
// Accepts a list of scripts, source files, or whole codebases (directories)
// and links them into a single .O file. Each input file is wrapped in the
// typed-expression block of the backend that matches its extension:
//
//   hello.py    →  python[N]^( ...file contents... )_python[N]
//   build.sh    →  bash[N]^( ...file contents... )_bash[N]
//   index.html  →  html[N]^( ...file contents... )_html[N]
//   notes.md    →  markdown[N]^( ... )_markdown[N]
//   prog.O      →  inlined verbatim (it is already O-lang source)
//
// Every wrapped file receives a unique `[N]` environment index within its
// language group (python[0], python[1], …), so each file runs in its own
// isolated backend environment and their state cannot leak into one another.
//
// Files of the same language are ordered by their import-dependency graph
// before being assigned environment indices: if `b.py` imports from `a.py`,
// `a.py` will appear as python[0] and `b.py` as python[1], regardless of
// their alphabetical order.  For languages without import scanning support,
// files keep the sorted order from the directory walk.
//
// Directories are walked recursively; every file with a recognized extension
// is included, in sorted order, so the output is deterministic.
//
// Any text inside a wrapped file that would collide with O-lang syntax —
// a registered opener like `python^(`, the wrapping block's own closer
// like `)_python`, or a splice like `$HOME` — is backslash-escaped
// (`\python^(`, `\)_python`, `\$HOME`), which the O-lang parser turns
// back into the literal text at evaluation time, so file contents survive
// the round trip byte-for-byte.
//
// Usage:
//   o-link a.py b.sh c.html -o program.O      # link three scripts
//   o-link src/ -o project.O                  # link a whole codebase
//   o-link a.py --lang txt=markdown -o out.O  # extra extension mapping
//   o-link a.py --stdout                      # write to stdout instead
//   o-link a.py b.sh --run                    # link, then execute in-process
//   o-link src/ -o app.O --shebang            # emit `#!/usr/bin/env o`, chmod +x
//
// Robustness guarantees:
//   * The combined output is re-parsed with the O-lang parser before it is
//     written, so o-link never emits a .O file that the runtime cannot read.
//   * Directory walks skip binary / non-UTF-8 files (with a warning), follow
//     symlinked directories at most once (no infinite loops), and never pick
//     up the output file itself.
//   * The same file given twice (directly or via overlapping directories) is
//     linked only once.
// ─────────────────────────────────────────────────────────────────────────────

use anyhow::{bail, Context, Result};
use clap::Parser as ClapParser;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

use o_lang::eval::Evaluator;
use o_lang::parser::Parser;
use o_lang::value::OValue;

/// o-link — link multiple scripts or codebases into a single .O file.
#[derive(Debug, ClapParser)]
#[command(
    name = "o-link",
    about = "Link scripts and codebases into a single .O file"
)]
struct Cli {
    /// Input files and/or directories to link, in order.
    #[arg(required = true)]
    inputs: Vec<PathBuf>,

    /// Output path for the combined .O file.
    #[arg(short = 'o', long = "output", default_value = "combined.O")]
    output: PathBuf,

    /// Write the combined source to stdout instead of a file.
    #[arg(long = "stdout", conflicts_with = "output")]
    to_stdout: bool,

    /// Extra extension→backend mappings, e.g. --lang txt=markdown.
    /// May be given multiple times; overrides the built-in mapping.
    #[arg(long = "lang", value_name = "EXT=BACKEND")]
    lang: Vec<String>,

    /// Skip the parse-validation pass on the combined output.
    #[arg(long = "no-validate")]
    no_validate: bool,

    /// Execute the combined program in-process after linking.
    #[arg(long = "run")]
    run: bool,

    /// Shim directory used by --run (defaults to ./backends).
    #[arg(long = "shim-dir", default_value = "backends")]
    shim_dir: PathBuf,

    /// Prepend `#!/usr/bin/env o` and mark the output executable, so the
    /// combined .O file can be run directly (`./program.O`).
    #[arg(long = "shebang", conflicts_with = "to_stdout")]
    shebang: bool,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let backends = registered_backends();

    let mut ext_map = default_extension_map();
    for spec in &cli.lang {
        let (ext, backend) = spec
            .split_once('=')
            .with_context(|| format!("--lang expects EXT=BACKEND, got `{}`", spec))?;
        if !backends.contains(backend) {
            bail!(
                "--lang {}: `{}` is not a registered backend",
                spec,
                backend
            );
        }
        ext_map.insert(ext.trim_start_matches('.').to_string(), backend.to_string());
    }

    // Never let the output file get linked into itself when a directory walk
    // would otherwise reach it (e.g. `o-link . -o ./combined.O` run twice).
    let exclude = if cli.to_stdout {
        None
    } else {
        cli.output.canonicalize().ok()
    };

    let files = collect_files(&cli.inputs, &ext_map, exclude.as_deref())?;
    if files.is_empty() {
        bail!("no linkable files found in the given inputs");
    }

    let mut combined = link_files(&files, &ext_map, &backends)?;

    if !cli.no_validate {
        let mut parser = Parser::new(&combined, &backends);
        parser
            .parse()
            .context("internal error: combined output does not parse as .O source")?;
    }

    if cli.shebang {
        combined.insert_str(0, "#!/usr/bin/env o\n");
    }

    if cli.to_stdout {
        print!("{}", combined);
    } else {
        fs::write(&cli.output, &combined)
            .with_context(|| format!("failed to write {}", cli.output.display()))?;
        if cli.shebang {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(&cli.output, fs::Permissions::from_mode(0o755))
                    .with_context(|| {
                        format!("failed to mark {} executable", cli.output.display())
                    })?;
            }
        }
        eprintln!(
            "linked {} file(s) into {}",
            files.len(),
            cli.output.display()
        );
    }

    if cli.run {
        run_combined(&combined, cli.shim_dir, backends)?;
    }

    Ok(())
}

/// Execute the combined program in-process, the same way the `O` interpreter
/// would: strip the shebang (if any), parse, evaluate, print the result.
fn run_combined(source: &str, shim_dir: PathBuf, backends: HashSet<String>) -> Result<()> {
    let body = if source.starts_with("#!") {
        source.find('\n').map(|nl| &source[nl + 1..]).unwrap_or("")
    } else {
        source
    };

    let mut parser = Parser::new(body, &backends);
    let nodes = parser
        .parse()
        .context("failed to parse combined .O source")?;

    let mut evaluator = Evaluator::new(shim_dir).with_registered_backends(backends);
    let result = evaluator
        .eval_document(nodes)
        .context("failed to evaluate combined .O program")?;

    match result {
        OValue::Str { v } | OValue::Html { v } => println!("{}", v),
        other => println!("{}", other),
    }
    Ok(())
}

// ─────────────────────────────────────────────────────────────────────────────
// Input collection
// ─────────────────────────────────────────────────────────────────────────────

/// Expand the input list: files are taken as-is (and must be mappable),
/// directories are walked recursively in sorted order, keeping only files
/// whose extension maps to a backend. Duplicate files (the same file given
/// twice, or reachable via overlapping directory inputs) are linked once,
/// and `exclude` (the output file) is never picked up.
fn collect_files(
    inputs: &[PathBuf],
    ext_map: &BTreeMap<String, String>,
    exclude: Option<&Path>,
) -> Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    let mut seen_files: HashSet<PathBuf> = HashSet::new();
    let mut seen_dirs: HashSet<PathBuf> = HashSet::new();

    for input in inputs {
        if input.is_dir() {
            walk_dir(input, ext_map, exclude, &mut seen_files, &mut seen_dirs, &mut files)?;
        } else if input.is_file() {
            if file_backend(input, ext_map).is_none() {
                bail!(
                    "{}: unrecognized extension — use --lang EXT=BACKEND to map it",
                    input.display()
                );
            }
            if push_unique(input, exclude, &mut seen_files, &mut files) {
                // Explicitly-listed files must be readable text: fail loudly
                // here instead of skipping silently like directory walks do.
                fs::read_to_string(input).with_context(|| {
                    format!("{}: not readable as UTF-8 text", input.display())
                })?;
            }
        } else {
            bail!("{}: no such file or directory", input.display());
        }
    }
    Ok(files)
}

/// Push `path` onto `files` unless it is the excluded output file or has
/// already been collected (compared by canonical path, so symlinks and
/// `./a.py` vs `a.py` spellings dedupe correctly). Returns true if pushed.
fn push_unique(
    path: &Path,
    exclude: Option<&Path>,
    seen: &mut HashSet<PathBuf>,
    files: &mut Vec<PathBuf>,
) -> bool {
    let canonical = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    if exclude.is_some_and(|e| e == canonical) {
        return false;
    }
    if !seen.insert(canonical) {
        return false;
    }
    files.push(path.to_path_buf());
    true
}

const SKIP_DIRS: &[&str] = &["target", "node_modules", "__pycache__", ".git"];

fn walk_dir(
    dir: &Path,
    ext_map: &BTreeMap<String, String>,
    exclude: Option<&Path>,
    seen_files: &mut HashSet<PathBuf>,
    seen_dirs: &mut HashSet<PathBuf>,
    out: &mut Vec<PathBuf>,
) -> Result<()> {
    // Symlink-loop protection: visit each real directory at most once.
    let canonical = dir
        .canonicalize()
        .with_context(|| format!("failed to resolve directory {}", dir.display()))?;
    if !seen_dirs.insert(canonical) {
        return Ok(());
    }

    let mut entries: Vec<PathBuf> = fs::read_dir(dir)
        .with_context(|| format!("failed to read directory {}", dir.display()))?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .collect();
    entries.sort();

    for entry in entries {
        let name = entry
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default();
        if name.starts_with('.') {
            continue;
        }
        if entry.is_dir() {
            if SKIP_DIRS.contains(&name) {
                continue;
            }
            walk_dir(&entry, ext_map, exclude, seen_files, seen_dirs, out)?;
        } else if entry.is_file() && file_backend(&entry, ext_map).is_some() {
            // Directory walks skip binary / non-UTF-8 files with a warning
            // rather than aborting the whole link.
            match fs::read(&entry) {
                Ok(bytes) if std::str::from_utf8(&bytes).is_ok() => {
                    push_unique(&entry, exclude, seen_files, out);
                }
                Ok(_) => {
                    eprintln!(
                        "warning: {}: skipped (not UTF-8 text)",
                        entry.display()
                    );
                }
                Err(err) => {
                    eprintln!("warning: {}: skipped ({})", entry.display(), err);
                }
            }
        }
    }
    Ok(())
}

/// Resolve a file path to its backend language, or None if the extension is
/// unknown. `.O` files map to the pseudo-backend "" (inline).
fn file_backend(path: &Path, ext_map: &BTreeMap<String, String>) -> Option<String> {
    let ext = path.extension()?.to_str()?;
    if ext == "O" {
        return Some(String::new());
    }
    ext_map.get(&ext.to_ascii_lowercase()).cloned()
}

// ─────────────────────────────────────────────────────────────────────────────
// Linking
// ─────────────────────────────────────────────────────────────────────────────

fn link_files(
    files: &[PathBuf],
    ext_map: &BTreeMap<String, String>,
    backends: &HashSet<String>,
) -> Result<String> {
    // Reorder same-language files according to their import-graph so that
    // files depended on by others always appear first.  Files of different
    // languages keep their relative order from the input list.
    let ordered = order_by_deps(files, ext_map);

    let mut out = String::new();
    out.push_str("# Linked by o-link — single-file .O program\n");

    // Track how many files of each language we have seen so far so we can
    // give every wrapped file its own isolated `[N]` environment slot.
    // `.O` files are inlined verbatim and do not get an env slot.
    let mut lang_counters: HashMap<String, u32> = HashMap::new();

    for path in &ordered {
        let backend = file_backend(path, ext_map)
            .with_context(|| format!("{}: unrecognized extension", path.display()))?;
        let mut content = fs::read_to_string(path)
            .with_context(|| format!("failed to read {}", path.display()))?;

        out.push('\n');
        out.push_str(&format!("# ── {} ──\n", path.display()));

        if backend.is_empty() {
            // .O source: strip a shebang line and inline verbatim.
            if content.starts_with("#!") {
                content = content
                    .find('\n')
                    .map(|nl| content[nl + 1..].to_string())
                    .unwrap_or_default();
            }
            out.push_str(&content);
            if !content.ends_with('\n') {
                out.push('\n');
            }
        } else {
            // Assign a unique per-language environment index so that every
            // wrapped file runs in its own isolated backend environment.
            // `python[0]^(...)_python[0]`, `python[1]^(...)_python[1]`, …
            let env_id = lang_counters.entry(backend.clone()).or_insert(0);
            let n = *env_id;
            *env_id += 1;

            let escaped = escape_body(&content, &backend, backends);
            out.push_str(&backend);
            out.push('[');
            out.push_str(&n.to_string());
            out.push_str("]^(\n");
            out.push_str(&escaped);
            if !escaped.ends_with('\n') {
                out.push('\n');
            }
            out.push_str(")_");
            out.push_str(&backend);
            out.push('[');
            out.push_str(&n.to_string());
            out.push_str("]\n");
        }
    }

    Ok(out)
}

// ─────────────────────────────────────────────────────────────────────────────
// Dependency ordering
// ─────────────────────────────────────────────────────────────────────────────

/// Reorder `files` so that within each language group, files that are imported
/// by others appear before the files that import them.  The relative order of
/// files from different language groups is preserved, and within a language
/// group the original (alphabetical) order is preserved for files that have
/// no dependency relationship with each other.
///
/// Cycles are broken conservatively: any file that participates in a cycle
/// keeps its original position relative to the other cycle members.
pub fn order_by_deps(files: &[PathBuf], ext_map: &BTreeMap<String, String>) -> Vec<PathBuf> {
    // Group files by backend language, preserving original indices so we can
    // interleave the sorted groups back correctly.
    // Files whose extension is not recognised are kept as-is in their original
    // positions so callers can decide how to handle them.
    let mut groups: HashMap<String, Vec<(usize, &PathBuf)>> = HashMap::new();
    let mut unknowns: Vec<(usize, &PathBuf)> = Vec::new();
    for (i, path) in files.iter().enumerate() {
        if let Some(backend) = file_backend(path, ext_map) {
            groups.entry(backend).or_default().push((i, path));
        } else {
            unknowns.push((i, path));
        }
    }

    // Sort each group by import-graph dependencies, then reassemble the full
    // list in original index order (preserving cross-language ordering).
    let mut sorted_entries: Vec<(usize, PathBuf)> = Vec::with_capacity(files.len());

    for (_lang, group) in &groups {
        let orig_indices: Vec<usize> = group.iter().map(|(i, _)| *i).collect();
        let paths: Vec<&PathBuf> = group.iter().map(|(_, p)| *p).collect();
        let sorted_paths = topo_sort_group(&paths, ext_map);
        // Zip the topo-sorted paths back with the original indices so the
        // interleave step uses the slot each file occupied in the input list.
        for (orig_i, path) in orig_indices.iter().zip(sorted_paths) {
            sorted_entries.push((*orig_i, path.clone()));
        }
    }

    // Preserve unknown-extension inputs in their original positions.
    for (i, path) in unknowns {
        sorted_entries.push((i, (*path).clone()));
    }

    sorted_entries.sort_by_key(|(i, _)| *i);
    sorted_entries.into_iter().map(|(_, p)| p).collect()
}

/// Topological sort of a single-language file group.
///
/// Builds a directed dependency graph among the files in `paths` using
/// language-specific import scanning, then emits files in an order where
/// every dependency precedes the files that depend on it.  Files that have
/// no dependency relationship keep their original relative order.  Cycles
/// are detected and broken by removing one back-edge (the cycle members keep
/// their original order).
fn topo_sort_group(paths: &[&PathBuf], ext_map: &BTreeMap<String, String>) -> Vec<PathBuf> {
    if paths.len() <= 1 {
        return paths.iter().map(|p| (*p).clone()).collect();
    }

    // Build a stem→index map so we can resolve import names to file indices.
    // For `src/utils.py` the stem is `utils`; for `pkg/sub/helper.py` we also
    // register `sub.helper` and `pkg.sub.helper` (dotted module paths).
    let mut stem_to_idx: HashMap<String, usize> = HashMap::new();
    for (i, path) in paths.iter().enumerate() {
        let stems = module_stems(path);
        for s in stems {
            stem_to_idx.entry(s).or_insert(i);
        }
    }

    // For each file, collect the set of file indices it depends on.
    let mut deps: Vec<HashSet<usize>> = vec![HashSet::new(); paths.len()];
    for (i, path) in paths.iter().enumerate() {
        if let Ok(src) = fs::read_to_string(path) {
            for imp in imported_modules(&src, ext_map, path) {
                if let Some(&j) = stem_to_idx.get(&imp) {
                    if j != i {
                        deps[i].insert(j);
                    }
                }
            }
        }
    }

    // Kahn's algorithm for topological sort.
    let n = paths.len();
    let mut in_degree = vec![0u32; n];
    // adjacency: rev_adj[j] = files that depend on j (j must come before them)
    let mut rev_adj: Vec<Vec<usize>> = vec![Vec::new(); n];
    for (i, dep_set) in deps.iter().enumerate() {
        for &j in dep_set {
            in_degree[i] += 1;
            rev_adj[j].push(i);
        }
    }

    // Use a stable queue (preserve original order among equal-priority nodes).
    let mut queue: Vec<usize> = (0..n).filter(|&i| in_degree[i] == 0).collect();
    let mut result: Vec<PathBuf> = Vec::with_capacity(n);

    while !queue.is_empty() {
        // Pick the smallest original index among ready nodes to preserve order.
        let pos = queue.iter().enumerate().min_by_key(|(_, &i)| i).map(|(p, _)| p).unwrap();
        let node = queue.remove(pos);
        result.push(paths[node].clone());
        for &dependent in &rev_adj[node] {
            in_degree[dependent] -= 1;
            if in_degree[dependent] == 0 {
                queue.push(dependent);
            }
        }
    }

    // If there are cycles, some nodes were never enqueued.  Append them in
    // original order (conservative: keep what the user gave us).
    if result.len() < n {
        let emitted: HashSet<usize> = (0..result.len())
            .filter_map(|k| {
                paths.iter().position(|p| *p == &result[k])
            })
            .collect();
        for i in 0..n {
            if !emitted.contains(&i) {
                result.push(paths[i].clone());
            }
        }
    }

    result
}

/// Return the set of module-name stems that `path` could be imported as.
///
/// For `/some/src/pkg/utils.py`, we return `["utils", "pkg.utils"]`.
/// We stop at directory components named `src`, `lib`, or `source` since
/// those are common source roots that are not part of the import path.
fn module_stems(path: &Path) -> Vec<String> {
    let mut stems = Vec::new();
    let stem = match path.file_stem().and_then(|s| s.to_str()) {
        Some(s) if s != "__init__" => s.to_string(),
        _ => return stems,
    };

    stems.push(stem.clone());

    // Build dotted-path variants by walking parent components.
    let mut parts: Vec<String> = vec![stem];
    let root_markers = ["src", "lib", "source", "tests"];
    for component in path.parent().map(|p| p.components()).into_iter().flatten().rev() {
        let name = match component {
            std::path::Component::Normal(n) => n.to_str().unwrap_or("").to_string(),
            _ => break,
        };
        if root_markers.contains(&name.as_str()) || name.starts_with('.') {
            break;
        }
        parts.insert(0, name.clone());
        stems.push(parts.join("."));
    }

    stems
}

/// Extract the module names imported by the source text of a file.
///
/// Currently handles Python (`import X`, `from X import …`).
/// Returns module stems suitable for lookup in `stem_to_idx`.
fn imported_modules(src: &str, ext_map: &BTreeMap<String, String>, path: &Path) -> Vec<String> {
    let lang = file_backend(path, ext_map).unwrap_or_default();
    let mut mods = Vec::new();

    match lang.as_str() {
        "python" => {
            for line in src.lines() {
                let line = line.trim();
                // `import X`, `import X as Y`, `import X, Y`
                if let Some(rest) = line.strip_prefix("import ") {
                    for part in rest.split(',') {
                        let module = part.trim().split_whitespace().next().unwrap_or("").trim();
                        if !module.is_empty() {
                            // Use only the top-level component for relative matching.
                            let top = module.split('.').next().unwrap_or(module);
                            mods.push(top.to_string());
                            // Also push the full dotted path for sub-module matching.
                            if module.contains('.') {
                                mods.push(module.to_string());
                            }
                        }
                    }
                }
                // `from X import Y` — the module being depended on is X.
                if let Some(rest) = line.strip_prefix("from ") {
                    let module = rest.split_whitespace().next().unwrap_or("").trim();
                    // Relative imports (`from . import X`) are skipped.
                    if !module.starts_with('.') && !module.is_empty() {
                        let top = module.split('.').next().unwrap_or(module);
                        mods.push(top.to_string());
                        if module.contains('.') {
                            mods.push(module.to_string());
                        }
                    }
                }
            }
        }
        // Additional languages can be added here following the same pattern.
        _ => {}
    }

    mods
}

/// Backslash-escape any text in `body` that the O-lang parser would otherwise
/// treat as syntax inside a `wrapper^( ... )_wrapper` block:
///
///   * any registered opener `IDENT[N]?{attr}?^(`  →  `\IDENT...^(`
///   * the wrapping block's own closer `)_wrapper`  →  `\)_wrapper`
///   * any splice `$IDENT`                          →  `\$IDENT`
///
/// The parser consumes the backslash and emits the literal text, so the
/// backend receives the file contents unchanged.
fn escape_body(body: &str, wrapper: &str, backends: &HashSet<String>) -> String {
    let closer = format!(")_{}", wrapper);
    let bytes = body.as_bytes();
    let mut out = String::with_capacity(body.len());
    let mut i = 0;

    while i < bytes.len() {
        if body[i..].starts_with(&closer) {
            out.push('\\');
            out.push_str(&closer);
            i += closer.len();
            continue;
        }
        if let Some(len) = opener_len(&body[i..], backends) {
            out.push('\\');
            out.push_str(&body[i..i + len]);
            i += len;
            continue;
        }
        // Escape `$IDENT` — the O-lang parser treats `$name` as a splice
        // (variable reference). Backslash-escaping it (`\$name`) makes the
        // parser emit the literal text `$name`, so the backend receives the
        // original file contents unchanged. This is critical for shell
        // scripts (`$HOME`, `$PATH`, …) and any language that uses `$`
        // followed by an identifier-shaped name.
        if bytes[i] == b'$'
            && i + 1 < bytes.len()
            && (bytes[i + 1].is_ascii_alphabetic() || bytes[i + 1] == b'_')
        {
            out.push('\\');
            out.push('$');
            i += 1;
            continue;
        }
        // Advance one full UTF-8 character.
        let ch = body[i..].chars().next().unwrap();
        out.push(ch);
        i += ch.len_utf8();
    }

    out
}

/// If `s` begins with a registered opener (`IDENT [N]? {attr}? ^(`), return
/// the byte length of the opener text including the trailing `^(`.
fn opener_len(s: &str, backends: &HashSet<String>) -> Option<usize> {
    let bytes = s.as_bytes();
    if bytes.is_empty() || !(bytes[0].is_ascii_alphabetic() || bytes[0] == b'_') {
        return None;
    }
    let mut i = 1;
    while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
        i += 1;
    }
    if !backends.contains(&s[..i]) {
        return None;
    }
    // Optional `[digits]` env marker.
    if i < bytes.len() && bytes[i] == b'[' {
        let mut j = i + 1;
        let digits_start = j;
        while j < bytes.len() && bytes[j].is_ascii_digit() {
            j += 1;
        }
        if j > digits_start && j < bytes.len() && bytes[j] == b']' {
            i = j + 1;
        }
    }
    // Optional `{attr}` marker.
    if i < bytes.len() && bytes[i] == b'{' {
        let mut j = i + 1;
        let ident_start = j;
        while j < bytes.len() && (bytes[j].is_ascii_alphanumeric() || bytes[j] == b'_') {
            j += 1;
        }
        if j > ident_start && j < bytes.len() && bytes[j] == b'}' {
            i = j + 1;
        }
    }
    if s[i..].starts_with("^(") {
        Some(i + 2)
    } else {
        None
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Tables
// ─────────────────────────────────────────────────────────────────────────────

/// Built-in extension → backend mapping. Keys are lowercase extensions
/// without the leading dot. `.O` is handled separately (inline).
fn default_extension_map() -> BTreeMap<String, String> {
    [
        ("py", "python"),
        ("sh", "bash"),
        ("bash", "bash"),
        ("html", "html"),
        ("htm", "html"),
        ("tex", "latex"),
        ("md", "markdown"),
        ("markdown", "markdown"),
        ("rs", "rust"),
        ("rkt", "racket"),
        ("nix", "nix"),
        ("txt", "text"),
        ("cs", "csharp"),
        ("c", "cpp"),
        ("cc", "cpp"),
        ("cpp", "cpp"),
        ("cxx", "cpp"),
        ("h", "cpp"),
        ("hpp", "cpp"),
        ("hs", "haskell"),
        ("lisp", "lisp"),
        ("cl", "common_lisp"),
        ("sql", "sql"),
        ("rb", "ruby"),
        ("m", "matlab"),
        ("wl", "mathematica"),
        ("wat", "webassembly"),
        ("java", "java"),
        ("js", "javascript"),
        ("mjs", "javascript"),
        ("cjs", "javascript"),
        ("ml", "ocaml"),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_string(), v.to_string()))
    .collect()
}

/// The registered backend set — must stay in sync with `registered_backends`
/// in src/main.rs so o-link escapes exactly the openers the runtime parses.
fn registered_backends() -> HashSet<String> {
    [
        "O", "python", "html", "latex", "markdown", "bash", "shell", "rust",
        "racket", "nix", "nix_expr", "nix_store", "nixos_test", "text",
        "csharp", "cpp", "haskell", "lisp", "common_lisp", "sql", "ruby",
        "matlab", "mathematica", "webassembly", "java", "javascript", "ocaml",
        "quote",
        // Aliases (canonicalized by the parser via the BackendRegistry).
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
    use o_lang::parser::{reconstruct_source, ONode, Parser};

    fn parse(src: &str) -> Vec<ONode> {
        let backends = registered_backends();
        Parser::new(src, &backends).parse().unwrap()
    }

    /// Concatenate all raw text inside the body of the first TypedExpr.
    fn first_block_text(nodes: &[ONode]) -> String {
        for node in nodes {
            if let ONode::TypedExpr { body, .. } = node {
                return reconstruct_source(body);
            }
        }
        panic!("no TypedExpr in parsed output");
    }

    #[test]
    fn escape_is_identity_for_plain_code() {
        let backends = registered_backends();
        let src = "x = 1 + 2\nprint(x)\n";
        assert_eq!(escape_body(src, "python", &backends), src);
    }

    #[test]
    fn escapes_opener_and_closer_collisions() {
        let backends = registered_backends();
        let src = "s = \"python^(1)_python\"";
        let escaped = escape_body(src, "python", &backends);
        assert_eq!(escaped, "s = \"\\python^(1\\)_python\"");
    }

    #[test]
    fn escaped_body_round_trips_through_parser() {
        let backends = registered_backends();
        let inner = "doc = \"use python^( ... )_python blocks\"\nx = 2 ^ (3 + 1)\n";
        let escaped = escape_body(inner, "python", &backends);
        let combined = format!("python^(\n{})_python\n", escaped);
        let nodes = parse(&combined);
        let body = first_block_text(&nodes);
        assert_eq!(body.trim_start_matches('\n'), inner);
    }

    #[test]
    fn foreign_closers_are_left_alone() {
        let backends = registered_backends();
        let src = "html closer: )_html stays literal";
        assert_eq!(escape_body(src, "python", &backends), src);
    }

    #[test]
    fn env_and_attr_openers_are_escaped() {
        let backends = registered_backends();
        let src = "python[3]^(x)_python[3] and python{lazy}^(y)_python{lazy}";
        let escaped = escape_body(src, "bash", &backends);
        assert!(escaped.contains("\\python[3]^("));
        assert!(escaped.contains("\\python{lazy}^("));
    }

    #[test]
    fn unregistered_idents_are_not_escaped() {
        let backends = registered_backends();
        let src = "result = pow2^(n) if weird else 2 ^ (x+1)";
        assert_eq!(escape_body(src, "python", &backends), src);
    }

    #[test]
    fn dollar_ident_splices_are_escaped() {
        let backends = registered_backends();
        let src = "echo $HOME and $PATH";
        let escaped = escape_body(src, "bash", &backends);
        assert_eq!(escaped, "echo \\$HOME and \\$PATH");
    }

    #[test]
    fn dollar_non_ident_is_left_alone() {
        let backends = registered_backends();
        // $1, $@, $? — the parser does not treat these as splices, so no escaping.
        let src = "echo $1 $@ $? $$";
        assert_eq!(escape_body(src, "bash", &backends), src);
    }

    #[test]
    fn dollar_ident_round_trips_through_parser() {
        let backends = registered_backends();
        let inner = "echo $HOME\ncd $PATH/bin\n";
        let escaped = escape_body(inner, "bash", &backends);
        assert!(escaped.contains("\\$HOME"));
        assert!(escaped.contains("\\$PATH"));
        // Use [0] env_id syntax since link_files now emits `bash[0]^(...)_bash[0]`.
        let combined = format!("bash[0]^(\n{})_bash[0]\n", escaped);
        let nodes = parse(&combined);
        let body = first_block_text(&nodes);
        assert_eq!(body.trim_start_matches('\n'), inner);
    }

    #[test]
    fn default_map_covers_common_scripts() {
        let map = default_extension_map();
        assert_eq!(map.get("py").unwrap(), "python");
        assert_eq!(map.get("sh").unwrap(), "bash");
        assert_eq!(map.get("html").unwrap(), "html");
        assert_eq!(map.get("md").unwrap(), "markdown");
    }

    /// Build a unique scratch directory for filesystem-backed tests.
    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "olink_test_{}_{}",
            name,
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn collect_dedupes_overlapping_inputs() {
        let dir = scratch("dedupe");
        let file = dir.join("a.py");
        fs::write(&file, "x = 1\n").unwrap();

        let map = default_extension_map();
        // Same file via the directory AND explicitly: linked once.
        let files = collect_files(&[dir.clone(), file.clone()], &map, None).unwrap();
        assert_eq!(files.len(), 1);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn collect_excludes_output_and_binary_files() {
        let dir = scratch("exclude");
        fs::write(dir.join("a.py"), "x = 1\n").unwrap();
        fs::write(dir.join("out.O"), "stale combined output\n").unwrap();
        fs::write(dir.join("blob.py"), [0xff_u8, 0xfe, 0x00]).unwrap();

        let map = default_extension_map();
        let exclude = dir.join("out.O").canonicalize().unwrap();
        let files = collect_files(&[dir.clone()], &map, Some(&exclude)).unwrap();

        // Only a.py: out.O is the excluded output, blob.py is not UTF-8.
        assert_eq!(files.len(), 1);
        assert!(files[0].ends_with("a.py"));

        let _ = fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn walk_survives_symlink_loops() {
        let dir = scratch("symloop");
        let sub = dir.join("sub");
        fs::create_dir_all(&sub).unwrap();
        fs::write(sub.join("a.py"), "x = 1\n").unwrap();
        std::os::unix::fs::symlink(&dir, sub.join("loop")).unwrap();

        let map = default_extension_map();
        let files = collect_files(&[dir.clone()], &map, None).unwrap();
        assert_eq!(files.len(), 1);

        let _ = fs::remove_dir_all(&dir);
    }

    // ── env_id isolation tests ───────────────────────────────────────────────

    #[test]
    fn link_files_assigns_unique_env_ids_per_language() {
        let dir = scratch("env_ids");
        fs::write(dir.join("a.py"), "x = 1\n").unwrap();
        fs::write(dir.join("b.py"), "y = 2\n").unwrap();
        fs::write(dir.join("c.sh"), "echo hi\n").unwrap();

        let map = default_extension_map();
        let backends = registered_backends();
        let files = collect_files(&[dir.clone()], &map, None).unwrap();
        let combined = link_files(&files, &map, &backends).unwrap();

        // Both Python files must appear with distinct [N] tags.
        assert!(combined.contains("python[0]^("), "expected python[0]^(, got:\n{}", combined);
        assert!(combined.contains("python[1]^("), "expected python[1]^(, got:\n{}", combined);
        assert!(combined.contains(")_python[0]"), "expected )_python[0], got:\n{}", combined);
        assert!(combined.contains(")_python[1]"), "expected )_python[1], got:\n{}", combined);
        // The shell file is the only file of its language so it gets [0].
        assert!(combined.contains("bash[0]^("), "expected bash[0]^(, got:\n{}", combined);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn link_files_env_ids_parse_cleanly() {
        let dir = scratch("env_parse");
        fs::write(dir.join("a.py"), "x = 1\n").unwrap();
        fs::write(dir.join("b.py"), "y = 2\n").unwrap();

        let map = default_extension_map();
        let backends = registered_backends();
        let files = collect_files(&[dir.clone()], &map, None).unwrap();
        let combined = link_files(&files, &map, &backends).unwrap();

        // The combined output must parse without errors.
        let mut parser = o_lang::parser::Parser::new(&combined, &backends);
        parser.parse().expect("combined output with env_ids should parse");

        let _ = fs::remove_dir_all(&dir);
    }

    // ── dependency ordering tests ────────────────────────────────────────────

    #[test]
    fn python_files_ordered_by_import_dependency() {
        let dir = scratch("pydeps");
        // b.py imports from a, so a.py should come first.
        fs::write(dir.join("a.py"), "def helper(): pass\n").unwrap();
        fs::write(dir.join("b.py"), "from a import helper\nhelper()\n").unwrap();

        let map = default_extension_map();
        let backends = registered_backends();
        let files = collect_files(&[dir.clone()], &map, None).unwrap();
        let combined = link_files(&files, &map, &backends).unwrap();

        // a.py (the dependency) must appear before b.py in the output.
        let pos_a = combined.find("python[0]^(").expect("python[0] not found");
        let pos_b = combined.find("python[1]^(").expect("python[1] not found");
        assert!(
            pos_a < pos_b,
            "a.py (dependency) should be python[0] but positions are a={} b={}",
            pos_a, pos_b
        );
        // Verify a.py body is in python[0] slot.
        let slot0_start = combined.find("python[0]^(").unwrap();
        let slot0_end   = combined.find(")_python[0]").unwrap();
        let slot0_body  = &combined[slot0_start..slot0_end];
        assert!(slot0_body.contains("def helper"), "python[0] should contain a.py");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn python_import_ordering_import_statement() {
        let dir = scratch("pydeps_import");
        // c.py imports from b, b imports from a.
        fs::write(dir.join("c.py"), "import b\n").unwrap();
        fs::write(dir.join("b.py"), "import a\n").unwrap();
        fs::write(dir.join("a.py"), "VALUE = 42\n").unwrap();

        let map = default_extension_map();
        let files = collect_files(&[dir.clone()], &map, None).unwrap();
        let ordered = order_by_deps(&files, &map);

        // The expected order after topo-sort: a.py, b.py, c.py.
        let names: Vec<&str> = ordered
            .iter()
            .filter_map(|p| p.file_name()?.to_str())
            .collect();
        assert_eq!(names, ["a.py", "b.py", "c.py"], "expected topo order a<b<c, got {:?}", names);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn dependency_ordering_does_not_reorder_different_languages() {
        let dir = scratch("crosslang");
        fs::write(dir.join("a.py"), "x = 1\n").unwrap();
        fs::write(dir.join("b.sh"), "echo hi\n").unwrap();
        fs::write(dir.join("c.py"), "import a\n").unwrap();

        let map = default_extension_map();
        let files = collect_files(&[dir.clone()], &map, None).unwrap();
        let ordered = order_by_deps(&files, &map);

        // a.py and c.py are Python; b.sh is bash.
        // After ordering: a.py (py dep) before c.py; b.sh keeps its position.
        let names: Vec<&str> = ordered
            .iter()
            .filter_map(|p| p.file_name()?.to_str())
            .collect();
        let pos_a = names.iter().position(|&n| n == "a.py").unwrap();
        let pos_c = names.iter().position(|&n| n == "c.py").unwrap();
        assert!(pos_a < pos_c, "a.py must come before c.py, got {:?}", names);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn cyclic_deps_do_not_panic() {
        let dir = scratch("cycle");
        fs::write(dir.join("a.py"), "import b\n").unwrap();
        fs::write(dir.join("b.py"), "import a\n").unwrap();

        let map = default_extension_map();
        let files = collect_files(&[dir.clone()], &map, None).unwrap();
        // Should not panic; result has both files.
        let ordered = order_by_deps(&files, &map);
        assert_eq!(ordered.len(), 2);

        let _ = fs::remove_dir_all(&dir);
    }
}
