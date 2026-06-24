// ─────────────────────────────────────────────────────────────────────────────
// o-link: the O-lang linker / combiner compiler
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
// Directories are walked recursively; every UTF-8 text file is included in
// sorted order so the output is deterministic. Unknown and extensionless
// files use the inert text backend unless --lang selects another backend.
//
// Any text inside a wrapped file that would collide with O-lang syntax:
// a registered opener like `python^(`, the wrapping block's own closer
// like `)_python`, or a splice like `$HOME`, is backslash-escaped
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
//   o-link src/ -o app.O --verbose-skips      # report every excluded path
//
// Robustness guarantees:
//   * The combined output is re-parsed with the O-lang parser before it is
//     written, so o-link never emits a .O file that the runtime cannot read.
//   * Directory walks skip binary / non-UTF-8 files, group warnings by reason,
//     do not descend into excluded subtrees unless --verbose-skips is set,
//     follow symlinked directories at most once (no infinite loops), and never
//     pick up the output file itself.
//   * The same file given twice (directly or via overlapping directories) is
//     linked only once.
// ─────────────────────────────────────────────────────────────────────────────

use anyhow::{bail, Context, Result};
use clap::Parser as ClapParser;
use ignore::gitignore::{Gitignore, GitignoreBuilder};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::path::{Component, Path, PathBuf};

use o_lang::eval::Evaluator;
use o_lang::ir::BackendRegistry;
use o_lang::parser::Parser;
use o_lang::value::OValue;

const SECTION_LENGTH_PREFIX: &str = "# o-link-section-bytes: ";
const O_LINK_GENERATED_HEADER: &str = "# Linked by o-link";

#[derive(Debug, Clone, PartialEq, Eq)]
struct SkippedPath {
    path: PathBuf,
    reason: String,
}

#[derive(Debug)]
struct CollectedFiles {
    files: Vec<PathBuf>,
    marker_root: PathBuf,
    skipped: Vec<SkippedPath>,
}

impl CollectedFiles {
    fn report_lines(&self, verbose_skips: bool) -> Vec<String> {
        let mut lines = Vec::new();
        if verbose_skips {
            lines.extend(self.skipped.iter().map(|skipped| {
                format!(
                    "warning: skipped {} ({})",
                    skipped.path.display(),
                    skipped.reason
                )
            }));
        } else {
            let mut counts = BTreeMap::<&str, usize>::new();
            for skipped in &self.skipped {
                *counts.entry(&skipped.reason).or_default() += 1;
            }
            lines.extend(counts.into_iter().map(|(reason, count)| {
                let noun = if count == 1 { "path" } else { "paths" };
                format!("warning: skipped {count} {noun} ({reason})")
            }));
        }
        lines.push(format!(
            "o-link scan: {} selected, {} skipped",
            self.files.len(),
            self.skipped.len()
        ));
        lines
    }

    fn report(&self, verbose_skips: bool) {
        for line in self.report_lines(verbose_skips) {
            eprintln!("{line}");
        }
    }
}

#[derive(Debug)]
struct IgnoreRules {
    source: PathBuf,
    matcher: Gitignore,
}

struct WalkState<'a> {
    exclude: Option<&'a Path>,
    seen_files: &'a mut HashSet<PathBuf>,
    seen_dirs: &'a mut HashSet<PathBuf>,
    files: &'a mut Vec<PathBuf>,
    skipped: &'a mut Vec<SkippedPath>,
    ignore_rules: &'a mut Vec<IgnoreRules>,
    enumerate_excluded_trees: bool,
}

/// o-link links multiple scripts or codebases into a single .O file.
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

    /// Print one warning for every skipped path instead of grouping by reason.
    #[arg(long)]
    verbose_skips: bool,

    /// Skip the parse-validation pass on the combined output.
    #[arg(long = "no-validate")]
    no_validate: bool,

    /// Execute the combined program in-process after linking.
    #[arg(long = "run")]
    run: bool,

    /// Shim directory used by --run (defaults to ./backends).
    #[arg(long = "shim-dir", default_value = "backends")]
    shim_dir: PathBuf,

    /// Mint a live backend capability for --run. Format:
    /// NAME=LANG[:fs_read,fs_write,network,process].
    #[arg(long = "backend-grant", requires = "run")]
    backend_grants: Vec<String>,

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
            bail!("--lang {}: `{}` is not a registered backend", spec, backend);
        }
        ext_map.insert(ext.trim_start_matches('.').to_string(), backend.to_string());
    }

    // Never let the output file get linked into itself when a directory walk
    // would otherwise reach it (e.g. `o-link . -o ./combined.O` run twice).
    let exclude = (!cli.to_stdout)
        .then(|| path_identity(&cli.output))
        .transpose()?;

    let collected =
        collect_files_with_skip_mode(&cli.inputs, &ext_map, exclude.as_deref(), cli.verbose_skips)?;
    collected.report(cli.verbose_skips);
    if collected.files.is_empty() {
        bail!("no linkable files found in the given inputs");
    }

    let mut combined = link_files(
        &collected.files,
        &collected.marker_root,
        &ext_map,
        &backends,
    )?;

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
                fs::set_permissions(&cli.output, fs::Permissions::from_mode(0o755)).with_context(
                    || format!("failed to mark {} executable", cli.output.display()),
                )?;
            }
        }
        eprintln!(
            "linked {} file(s) into {}",
            collected.files.len(),
            cli.output.display()
        );
    }

    if cli.run {
        run_combined(&combined, cli.shim_dir, backends, &cli.backend_grants)?;
    }

    Ok(())
}

/// Execute the combined program in-process, the same way the `O` interpreter
/// would: strip the shebang (if any), parse, evaluate, print the result.
fn run_combined(
    source: &str,
    shim_dir: PathBuf,
    backends: HashSet<String>,
    backend_grants: &[String],
) -> Result<()> {
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
    let mut scope = HashMap::new();
    for grant in backend_grants {
        evaluator.install_backend_grant(grant, &mut scope)?;
    }
    let result = evaluator
        .eval_document_with_scope(nodes, &mut scope)
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

/// Expand the input list and compute the root against which marker paths are
/// written. Explicit files fail on invalid input. Directory walks record every
/// skipped path so callers can report exclusions uniformly.
#[cfg(test)]
fn collect_files(
    inputs: &[PathBuf],
    _ext_map: &BTreeMap<String, String>,
    exclude: Option<&Path>,
) -> Result<CollectedFiles> {
    collect_files_with_skip_mode(inputs, _ext_map, exclude, false)
}

fn collect_files_with_skip_mode(
    inputs: &[PathBuf],
    _ext_map: &BTreeMap<String, String>,
    exclude: Option<&Path>,
    enumerate_excluded_trees: bool,
) -> Result<CollectedFiles> {
    let mut files = Vec::new();
    let mut skipped = Vec::new();
    let mut seen_files: HashSet<PathBuf> = HashSet::new();
    let mut seen_dirs: HashSet<PathBuf> = HashSet::new();

    for input in inputs {
        let input = absolute_path(input)?;
        if input.is_dir() {
            let mut ignore_rules = Vec::new();
            let mut state = WalkState {
                exclude,
                seen_files: &mut seen_files,
                seen_dirs: &mut seen_dirs,
                files: &mut files,
                skipped: &mut skipped,
                ignore_rules: &mut ignore_rules,
                enumerate_excluded_trees,
            };
            walk_dir(&input, &mut state)?;
        } else if input.is_file() {
            if push_unique(&input, exclude, &mut seen_files, &mut files, &mut skipped)? {
                // Explicitly-listed files must be readable text: fail loudly
                // here instead of skipping silently like directory walks do.
                fs::read_to_string(&input)
                    .with_context(|| format!("{}: not readable as UTF-8 text", input.display()))?;
            }
        } else {
            bail!("{}: no such file or directory", input.display());
        }
    }

    let marker_root = compute_marker_root(inputs, &files)?;
    Ok(CollectedFiles {
        files,
        marker_root,
        skipped,
    })
}

/// Push `path` onto `files` unless it is the excluded output file or has
/// already been collected (compared by canonical path, so symlinks and
/// `./a.py` vs `a.py` spellings dedupe correctly). Returns true if pushed.
fn push_unique(
    path: &Path,
    exclude: Option<&Path>,
    seen: &mut HashSet<PathBuf>,
    files: &mut Vec<PathBuf>,
    skipped: &mut Vec<SkippedPath>,
) -> Result<bool> {
    let canonical = path_identity(path)?;
    if exclude.is_some_and(|e| e == canonical) {
        skipped.push(SkippedPath {
            path: path.to_path_buf(),
            reason: "output file".into(),
        });
        return Ok(false);
    }
    if !seen.insert(canonical) {
        skipped.push(SkippedPath {
            path: path.to_path_buf(),
            reason: "duplicate or symlink alias".into(),
        });
        return Ok(false);
    }
    files.push(path.to_path_buf());
    Ok(true)
}

const SKIP_DIRS: &[&str] = &["target", "node_modules", "__pycache__", ".git"];

fn walk_dir(dir: &Path, state: &mut WalkState<'_>) -> Result<()> {
    // Symlink-loop protection: visit each real directory at most once.
    let canonical = dir
        .canonicalize()
        .with_context(|| format!("failed to resolve directory {}", dir.display()))?;
    if !state.seen_dirs.insert(canonical) {
        state.skipped.push(SkippedPath {
            path: dir.to_path_buf(),
            reason: "directory already visited through another path".into(),
        });
        return Ok(());
    }

    let previous_rule_count = state.ignore_rules.len();
    load_ignore_rules(dir, state.ignore_rules);

    let mut entries = Vec::new();
    for entry in
        fs::read_dir(dir).with_context(|| format!("failed to read directory {}", dir.display()))?
    {
        match entry {
            Ok(entry) => entries.push(entry.path()),
            Err(error) => state.skipped.push(SkippedPath {
                path: dir.join("<unreadable-entry>"),
                reason: format!("directory entry error: {error}"),
            }),
        }
    }
    entries.sort();

    for entry in entries {
        let name = entry
            .file_name()
            .map(|n| n.to_string_lossy())
            .unwrap_or_default();
        let is_dir = entry.is_dir();
        let is_file = entry.is_file();

        if let Some(source) = ignored_by(state.ignore_rules, &entry, is_dir) {
            record_excluded_tree(
                &entry,
                &format!("ignored by {}", source.display()),
                state.skipped,
                state.enumerate_excluded_trees,
            );
            continue;
        }

        if name.starts_with('.') {
            record_excluded_tree(
                &entry,
                "hidden path",
                state.skipped,
                state.enumerate_excluded_trees,
            );
            continue;
        }

        if is_dir {
            if SKIP_DIRS.contains(&name.as_ref()) {
                record_excluded_tree(
                    &entry,
                    "built-in excluded directory",
                    state.skipped,
                    state.enumerate_excluded_trees,
                );
                continue;
            }
            walk_dir(&entry, state)?;
            continue;
        }

        if !is_file {
            state.skipped.push(SkippedPath {
                path: entry,
                reason: "unsupported filesystem entry".into(),
            });
            continue;
        }

        match fs::read(&entry) {
            Ok(bytes) => match std::str::from_utf8(&bytes) {
                Ok(text) => {
                    if is_generated_olink_output(text) {
                        state.skipped.push(SkippedPath {
                            path: entry,
                            reason: "generated o-link output".into(),
                        });
                        continue;
                    }
                    let _ = push_unique(
                        &entry,
                        state.exclude,
                        state.seen_files,
                        state.files,
                        state.skipped,
                    )?;
                }
                Err(_) => state.skipped.push(SkippedPath {
                    path: entry,
                    reason: "not UTF-8 text".into(),
                }),
            },
            Err(error) => state.skipped.push(SkippedPath {
                path: entry,
                reason: format!("read error: {error}"),
            }),
        }
    }

    state.ignore_rules.truncate(previous_rule_count);
    Ok(())
}

fn is_generated_olink_output(text: &str) -> bool {
    text.starts_with(O_LINK_GENERATED_HEADER)
}

fn record_excluded_tree(
    path: &Path,
    reason: &str,
    skipped: &mut Vec<SkippedPath>,
    enumerate_children: bool,
) {
    skipped.push(SkippedPath {
        path: path.to_path_buf(),
        reason: reason.to_string(),
    });

    if !enumerate_children {
        return;
    }

    let Ok(metadata) = fs::symlink_metadata(path) else {
        return;
    };
    if !metadata.file_type().is_dir() {
        return;
    }

    let entries = match fs::read_dir(path) {
        Ok(entries) => entries,
        Err(error) => {
            skipped.push(SkippedPath {
                path: path.join("<unreadable-entry>"),
                reason: format!("excluded directory entry error: {error}"),
            });
            return;
        }
    };
    let mut paths = Vec::new();
    for entry in entries {
        match entry {
            Ok(entry) => paths.push(entry.path()),
            Err(error) => skipped.push(SkippedPath {
                path: path.join("<unreadable-entry>"),
                reason: format!("excluded directory entry error: {error}"),
            }),
        }
    }
    let mut entries = paths;
    entries.sort();
    for entry in entries {
        record_excluded_tree(&entry, reason, skipped, enumerate_children);
    }
}

fn load_ignore_rules(dir: &Path, rules: &mut Vec<IgnoreRules>) {
    for name in [".gitignore", ".olinkignore"] {
        let source = dir.join(name);
        if !source.is_file() {
            continue;
        }
        let mut builder = GitignoreBuilder::new(dir);
        if let Some(error) = builder.add(&source) {
            eprintln!(
                "warning: {}: some ignore rules could not be loaded ({error})",
                source.display()
            );
        }
        match builder.build() {
            Ok(matcher) => rules.push(IgnoreRules { source, matcher }),
            Err(error) => eprintln!(
                "warning: {}: ignore rules disabled ({error})",
                source.display()
            ),
        }
    }
}

fn ignored_by(rules: &[IgnoreRules], path: &Path, is_dir: bool) -> Option<PathBuf> {
    let mut ignored = None;
    for rule_set in rules {
        let matched = rule_set.matcher.matched(path, is_dir);
        if matched.is_ignore() {
            ignored = Some(rule_set.source.clone());
        } else if matched.is_whitelist() {
            ignored = None;
        }
    }
    ignored
}

fn absolute_path(path: &Path) -> Result<PathBuf> {
    let path = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .context("failed to resolve current directory")?
            .join(path)
    };
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            Component::Prefix(_) | Component::RootDir | Component::Normal(_) => {
                normalized.push(component.as_os_str());
            }
        }
    }
    Ok(normalized)
}

fn path_identity(path: &Path) -> Result<PathBuf> {
    path.canonicalize().or_else(|_| absolute_path(path))
}

fn compute_marker_root(inputs: &[PathBuf], files: &[PathBuf]) -> Result<PathBuf> {
    let mut anchors = Vec::new();
    for input in inputs {
        let absolute = absolute_path(input)?;
        if absolute.is_dir() {
            anchors.push(absolute);
        } else if let Some(parent) = absolute.parent() {
            anchors.push(parent.to_path_buf());
        }
    }
    for file in files {
        let absolute = absolute_path(file)?;
        if let Some(parent) = absolute.parent() {
            anchors.push(parent.to_path_buf());
        }
    }
    common_path_root(&anchors).context("inputs do not share a filesystem root")
}

fn common_path_root(paths: &[PathBuf]) -> Option<PathBuf> {
    let first = paths.first()?;
    let mut common: Vec<Component<'_>> = first.components().collect();
    for path in &paths[1..] {
        let components: Vec<Component<'_>> = path.components().collect();
        let keep = common
            .iter()
            .zip(&components)
            .take_while(|(left, right)| left == right)
            .count();
        common.truncate(keep);
    }
    if common.is_empty() {
        return None;
    }
    let mut root = PathBuf::new();
    for component in common {
        root.push(component.as_os_str());
    }
    Some(root)
}

fn marker_path(path: &Path, marker_root: &Path) -> Result<PathBuf> {
    let absolute = absolute_path(path)?;
    let relative = absolute.strip_prefix(marker_root).with_context(|| {
        format!(
            "{} is outside marker root {}",
            absolute.display(),
            marker_root.display()
        )
    })?;
    if relative.as_os_str().is_empty()
        || relative.is_absolute()
        || relative
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        bail!("unsafe marker path derived from {}", path.display());
    }
    let text = relative
        .to_str()
        .with_context(|| format!("{} is not a UTF-8 path", relative.display()))?;
    if text.contains(['\n', '\r']) {
        bail!("marker path contains a line break: {}", relative.display());
    }
    Ok(relative.to_path_buf())
}

/// Resolve a file path to its backend language. Unknown and extensionless
/// UTF-8 files use the inert `text` backend so arbitrary textual source trees
/// remain lossless. `.O` files map to the pseudo-backend "" (inline).
fn file_backend(path: &Path, ext_map: &BTreeMap<String, String>) -> String {
    let Some(ext) = path.extension().and_then(|ext| ext.to_str()) else {
        return "text".to_string();
    };
    if ext == "O" {
        return String::new();
    }
    ext_map
        .get(&ext.to_ascii_lowercase())
        .cloned()
        .unwrap_or_else(|| "text".to_string())
}

// ─────────────────────────────────────────────────────────────────────────────
// Linking
// ─────────────────────────────────────────────────────────────────────────────

fn link_files(
    files: &[PathBuf],
    marker_root: &Path,
    ext_map: &BTreeMap<String, String>,
    backends: &HashSet<String>,
) -> Result<String> {
    // Reorder same-language files according to their import-graph so that
    // files depended on by others always appear first.  Files of different
    // languages keep their relative order from the input list.
    let ordered = order_by_deps(files, ext_map);

    let mut out = String::new();
    out.push_str("# Linked by o-link: single-file .O program\n");

    // Track how many files of each language we have seen so far so we can
    // give every wrapped file its own isolated `[N]` environment slot.
    // `.O` files are inlined verbatim and do not get an env slot.
    let mut lang_counters: HashMap<String, u32> = HashMap::new();

    for path in &ordered {
        let backend = file_backend(path, ext_map);
        let mut content = fs::read_to_string(path)
            .with_context(|| format!("failed to read {}", path.display()))?;
        let marker = marker_path(path, marker_root)?;

        out.push('\n');
        out.push_str(&format!("# ── {} ──\n", marker.display()));

        let mut section = String::new();

        if backend.is_empty() {
            // .O source: strip a shebang line and inline verbatim.
            if content.starts_with("#!") {
                content = content
                    .find('\n')
                    .map(|nl| content[nl + 1..].to_string())
                    .unwrap_or_default();
            }
            section.push_str(&content);
        } else {
            // Assign a unique per-language environment index so that every
            // wrapped file runs in its own isolated backend environment.
            // `python[0]^(...)_python[0]`, `python[1]^(...)_python[1]`, …
            let env_id = lang_counters.entry(backend.clone()).or_insert(0);
            let n = *env_id;
            *env_id += 1;

            let interface = BackendRegistry::global().interface_for(&backend);
            let authority_attr = if interface.required_authorities.is_empty() {
                ""
            } else {
                "{cap=backend}"
            };
            let tag = format!("{backend}[{n}]{authority_attr}");
            let closer = format!(")_{tag}");
            let escaped = escape_body(&content, &closer, backends);
            section.push_str(&tag);
            section.push_str("^(\n");
            section.push_str(&escaped);
            section.push_str(&closer);
            section.push('\n');
        }

        out.push_str(SECTION_LENGTH_PREFIX);
        out.push_str(&section.len().to_string());
        out.push('\n');
        out.push_str(&section);
        if !section.ends_with('\n') {
            out.push('\n');
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
    let mut groups: HashMap<String, Vec<(usize, &PathBuf)>> = HashMap::new();
    for (i, path) in files.iter().enumerate() {
        groups
            .entry(file_backend(path, ext_map))
            .or_default()
            .push((i, path));
    }

    // Sort each group by import-graph dependencies, then reassemble the full
    // list in original index order (preserving cross-language ordering).
    let mut sorted_entries: Vec<(usize, PathBuf)> = Vec::with_capacity(files.len());

    for group in groups.values() {
        let orig_indices: Vec<usize> = group.iter().map(|(i, _)| *i).collect();
        let paths: Vec<&PathBuf> = group.iter().map(|(_, p)| *p).collect();
        let sorted_paths = topo_sort_group(&paths, ext_map);
        // Zip the topo-sorted paths back with the original indices so the
        // interleave step uses the slot each file occupied in the input list.
        for (orig_i, path) in orig_indices.iter().zip(sorted_paths) {
            sorted_entries.push((*orig_i, path.clone()));
        }
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
        let pos = queue
            .iter()
            .enumerate()
            .min_by_key(|(_, &i)| i)
            .map(|(p, _)| p)
            .unwrap();
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
            .filter_map(|k| paths.iter().position(|p| *p == &result[k]))
            .collect();
        for (i, path) in paths.iter().enumerate() {
            if !emitted.contains(&i) {
                result.push((*path).clone());
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
    for component in path
        .parent()
        .map(|p| p.components())
        .into_iter()
        .flatten()
        .rev()
    {
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

/// Extract module names imported by one source file. This is intentionally a
/// lightweight dependency scanner, not a full parser. It recognizes the
/// ordinary static import forms of each hosted language and returns module
/// stems suitable for lookup in `stem_to_idx`.
fn imported_modules(src: &str, ext_map: &BTreeMap<String, String>, path: &Path) -> Vec<String> {
    let lang = file_backend(path, ext_map);
    let mut mods = Vec::new();

    for line in src.lines() {
        let line = line.trim();
        match lang.as_str() {
            "python" => {
                // `import X`, `import X as Y`, `import X, Y`
                if let Some(rest) = line.strip_prefix("import ") {
                    for part in rest.split(',') {
                        push_import_candidates(
                            &mut mods,
                            part.split_whitespace().next().unwrap_or(""),
                        );
                    }
                }
                // `from X import Y`: the dependency is X.
                if let Some(rest) = line.strip_prefix("from ") {
                    let module = rest.split_whitespace().next().unwrap_or("");
                    push_import_candidates(&mut mods, module);
                }
            }
            "javascript" => {
                if line.starts_with("import ") || line.starts_with("export ") {
                    if let Some(specifier) = quoted_text(line) {
                        push_import_candidates(&mut mods, specifier);
                    }
                }
                for prefix in ["require(", "import("] {
                    if let Some(start) = line.find(prefix) {
                        if let Some(specifier) = quoted_text(&line[start + prefix.len()..]) {
                            push_import_candidates(&mut mods, specifier);
                        }
                    }
                }
            }
            "rust" => {
                let line = line.strip_prefix("pub ").unwrap_or(line);
                if let Some(module) = line.strip_prefix("mod ") {
                    push_import_candidates(&mut mods, module.trim_end_matches(';'));
                }
                if let Some(module) = line.strip_prefix("use ") {
                    push_import_candidates(&mut mods, module.trim_end_matches(';'));
                }
                if let Some(module) = line.strip_prefix("extern crate ") {
                    push_import_candidates(&mut mods, module.trim_end_matches(';'));
                }
            }
            "cpp" => {
                if let Some(include) = line.strip_prefix("#include") {
                    if include.trim_start().starts_with('"') {
                        if let Some(specifier) = quoted_text(include) {
                            push_import_candidates(&mut mods, specifier);
                        }
                    }
                }
            }
            "java" => {
                if let Some(module) = line.strip_prefix("import ") {
                    let module = module.strip_prefix("static ").unwrap_or(module);
                    push_import_candidates(
                        &mut mods,
                        module.trim_end_matches(';').trim_end_matches(".*"),
                    );
                }
            }
            "haskell" => {
                if let Some(module) = line.strip_prefix("import ") {
                    let module = module.strip_prefix("qualified ").unwrap_or(module);
                    push_import_candidates(
                        &mut mods,
                        module.split_whitespace().next().unwrap_or(""),
                    );
                }
            }
            "ruby" => {
                if line.starts_with("require ") || line.starts_with("require_relative ") {
                    if let Some(specifier) = quoted_text(line) {
                        push_import_candidates(&mut mods, specifier);
                    }
                }
            }
            "ocaml" => {
                for prefix in ["open ", "include "] {
                    if let Some(module) = line.strip_prefix(prefix) {
                        push_import_candidates(
                            &mut mods,
                            module.split_whitespace().next().unwrap_or(""),
                        );
                    }
                }
            }
            "racket" | "lisp" | "common_lisp" => {
                if line.contains("require") || line.contains("load") {
                    if let Some(specifier) = quoted_text(line) {
                        push_import_candidates(&mut mods, specifier);
                    }
                }
            }
            "bash" | "shell" => {
                for prefix in ["source ", ". "] {
                    if let Some(specifier) = line.strip_prefix(prefix) {
                        push_import_candidates(
                            &mut mods,
                            specifier.split_whitespace().next().unwrap_or(""),
                        );
                    }
                }
            }
            "nix" | "nix_expr" => {
                for token in line.split(|ch: char| {
                    ch.is_whitespace() || matches!(ch, '(' | ')' | '{' | '}' | '[' | ']' | ';')
                }) {
                    if token.starts_with("./") || token.starts_with("../") {
                        push_import_candidates(&mut mods, token);
                    }
                }
            }
            "csharp" => {
                if let Some(module) = line.strip_prefix("using ") {
                    push_import_candidates(&mut mods, module.trim_end_matches(';'));
                }
            }
            "mathematica" | "matlab" => {
                if let Some(specifier) = quoted_text(line) {
                    if line.contains("Get")
                        || line.contains("Needs")
                        || line.contains("run(")
                        || line.contains("source(")
                    {
                        push_import_candidates(&mut mods, specifier);
                    }
                }
            }
            _ => {}
        }
    }

    mods.sort();
    mods.dedup();
    mods
}

fn quoted_text(text: &str) -> Option<&str> {
    let (start, quote) = text
        .char_indices()
        .find(|(_, ch)| matches!(ch, '\'' | '"'))?;
    let rest = &text[start + quote.len_utf8()..];
    let end = rest.find(quote)?;
    Some(&rest[..end])
}

fn push_import_candidates(modules: &mut Vec<String>, specifier: &str) {
    let mut specifier = specifier
        .trim()
        .trim_matches(|ch: char| matches!(ch, '\'' | '"' | '(' | ')' | ';' | ','))
        .trim_start_matches("crate::")
        .trim_start_matches("self::")
        .trim_start_matches("super::")
        .trim_start_matches("./")
        .trim_start_matches("../")
        .replace("::", ".")
        .replace(['/', '\\'], ".");
    for extension in [
        ".py", ".js", ".mjs", ".cjs", ".rs", ".h", ".hpp", ".c", ".cpp", ".java", ".hs", ".rb",
        ".ml", ".rkt", ".scm", ".lisp", ".sh", ".nix", ".wl", ".m",
    ] {
        if specifier.ends_with(extension) {
            specifier.truncate(specifier.len() - extension.len());
            break;
        }
    }
    let specifier = specifier
        .trim_matches('.')
        .trim_end_matches(".*")
        .trim_end_matches("::{")
        .trim();
    if specifier.is_empty() {
        return;
    }
    modules.push(specifier.to_string());
    if let Some(first) = specifier.split('.').next() {
        modules.push(first.to_string());
    }
    if let Some(last) = specifier.rsplit('.').next() {
        modules.push(last.to_string());
        let lowercase = last.to_ascii_lowercase();
        if lowercase != last {
            modules.push(lowercase);
        }
    }
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
fn escape_body(body: &str, closer: &str, backends: &HashSet<String>) -> String {
    let bytes = body.as_bytes();
    let mut out = String::with_capacity(body.len());
    let mut i = 0;

    while i < bytes.len() {
        if body[i..].starts_with(closer) {
            out.push('\\');
            out.push_str(closer);
            i += closer.len();
            continue;
        }
        if let Some(len) = opener_len(&body[i..], backends) {
            out.push('\\');
            out.push_str(&body[i..i + len]);
            i += len;
            continue;
        }
        // Escape `$IDENT`. The O-lang parser treats `$name` as a splice
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
        while j < bytes.len() && bytes[j] != b'}' {
            j += 1;
        }
        if j > i + 1 && j < bytes.len() && bytes[j] == b'}' {
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

/// The registered backend set must stay in sync with `registered_backends`
/// in src/main.rs so o-link escapes exactly the openers the runtime parses.
fn registered_backends() -> HashSet<String> {
    [
        "O",
        "python",
        "html",
        "latex",
        "markdown",
        "bash",
        "shell",
        "rust",
        "racket",
        "nix",
        "nix_expr",
        "nix_store",
        "nixos_test",
        "text",
        "csharp",
        "cpp",
        "haskell",
        "lisp",
        "common_lisp",
        "sql",
        "ruby",
        "matlab",
        "mathematica",
        "webassembly",
        "java",
        "javascript",
        "ocaml",
        "quote",
        // Aliases (canonicalized by the parser via the BackendRegistry).
        "py",
        "md",
        "tex",
        "plain",
        "o",
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
        assert_eq!(escape_body(src, ")_python", &backends), src);
    }

    #[test]
    fn escapes_opener_and_closer_collisions() {
        let backends = registered_backends();
        let src = "s = \"python^(1)_python\"";
        let escaped = escape_body(src, ")_python", &backends);
        assert_eq!(escaped, "s = \"\\python^(1\\)_python\"");
    }

    #[test]
    fn escaped_body_round_trips_through_parser() {
        let backends = registered_backends();
        let inner = "doc = \"use python^( ... )_python blocks\"\nx = 2 ^ (3 + 1)\n";
        let escaped = escape_body(inner, ")_python", &backends);
        let combined = format!("python^(\n{})_python\n", escaped);
        let nodes = parse(&combined);
        let body = first_block_text(&nodes);
        assert_eq!(body.trim_start_matches('\n'), inner);
    }

    #[test]
    fn indexed_closer_escaping_is_exact() {
        let backends = registered_backends();
        let inner = ")_python stays literal; )_python[0] is the real closer";
        let escaped = escape_body(inner, ")_python[0]", &backends);
        assert!(escaped.starts_with(")_python stays literal"));
        assert!(escaped.contains("\\)_python[0]"));
        let combined = format!("python[0]^(\n{escaped})_python[0]\n");
        let nodes = parse(&combined);
        let body = first_block_text(&nodes);
        assert_eq!(body.trim_start_matches('\n'), inner);
    }

    #[test]
    fn foreign_closers_are_left_alone() {
        let backends = registered_backends();
        let src = "html closer: )_html stays literal";
        assert_eq!(escape_body(src, ")_python", &backends), src);
    }

    #[test]
    fn env_and_attr_openers_are_escaped() {
        let backends = registered_backends();
        let src = "python[3]^(x)_python[3] and python{lazy}^(y)_python{lazy}";
        let escaped = escape_body(src, ")_bash", &backends);
        assert!(escaped.contains("\\python[3]^("));
        assert!(escaped.contains("\\python{lazy}^("));
    }

    #[test]
    fn unregistered_idents_are_not_escaped() {
        let backends = registered_backends();
        let src = "result = pow2^(n) if weird else 2 ^ (x+1)";
        assert_eq!(escape_body(src, ")_python", &backends), src);
    }

    #[test]
    fn dollar_ident_splices_are_escaped() {
        let backends = registered_backends();
        let src = "echo $HOME and $PATH";
        let escaped = escape_body(src, ")_bash", &backends);
        assert_eq!(escaped, "echo \\$HOME and \\$PATH");
    }

    #[test]
    fn dollar_non_ident_is_left_alone() {
        let backends = registered_backends();
        // The parser does not treat $1, $@, or $? as splices, so they need no escaping.
        let src = "echo $1 $@ $? $$";
        assert_eq!(escape_body(src, ")_bash", &backends), src);
    }

    #[test]
    fn dollar_ident_round_trips_through_parser() {
        let backends = registered_backends();
        let inner = "echo $HOME\ncd $PATH/bin\n";
        let escaped = escape_body(inner, ")_bash[0]", &backends);
        assert!(escaped.contains("\\$HOME"));
        assert!(escaped.contains("\\$PATH"));
        // Use [0] env_id syntax to exercise the same delimiter shape as link_files.
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

    #[test]
    fn unknown_and_extensionless_text_use_inert_text_backend() {
        let map = default_extension_map();
        assert_eq!(file_backend(Path::new("component.svelte"), &map), "text");
        assert_eq!(file_backend(Path::new("module.ts"), &map), "text");
        assert_eq!(file_backend(Path::new("README"), &map), "text");
        assert_eq!(file_backend(Path::new("program.O"), &map), "");
    }

    #[test]
    fn skip_report_is_aggregated_unless_verbose() {
        let collection = CollectedFiles {
            files: vec![PathBuf::from("selected.py")],
            marker_root: PathBuf::from("."),
            skipped: vec![
                SkippedPath {
                    path: PathBuf::from("one.bin"),
                    reason: "not UTF-8 text".into(),
                },
                SkippedPath {
                    path: PathBuf::from("two.bin"),
                    reason: "not UTF-8 text".into(),
                },
                SkippedPath {
                    path: PathBuf::from(".hidden"),
                    reason: "hidden path".into(),
                },
            ],
        };

        assert_eq!(
            collection.report_lines(false),
            vec![
                "warning: skipped 1 path (hidden path)",
                "warning: skipped 2 paths (not UTF-8 text)",
                "o-link scan: 1 selected, 3 skipped",
            ]
        );
        let verbose = collection.report_lines(true);
        assert_eq!(verbose.len(), 4);
        assert!(verbose[0].contains("one.bin"));
        assert!(verbose[1].contains("two.bin"));
        assert!(verbose[2].contains(".hidden"));
    }

    /// Build a unique scratch directory for filesystem-backed tests.
    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("olink_test_{}_{}", name, std::process::id()));
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
        let collection = collect_files(&[dir.clone(), file.clone()], &map, None).unwrap();
        assert_eq!(collection.files.len(), 1);

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
        let collection = collect_files(std::slice::from_ref(&dir), &map, Some(&exclude)).unwrap();

        // Only a.py: out.O is the excluded output, blob.py is not UTF-8.
        assert_eq!(collection.files.len(), 1);
        assert!(collection.files[0].ends_with("a.py"));

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn directory_walk_skips_generated_olink_outputs() {
        let dir = scratch("skip_generated_olink");
        fs::write(dir.join("a.py"), "x = 1\n").unwrap();
        fs::write(dir.join("ordinary.O"), "python^(1)_python\n").unwrap();
        fs::write(
            dir.join("combined.O"),
            "# Linked by o-link: single-file .O program\npython^(2)_python\n",
        )
        .unwrap();

        let map = default_extension_map();
        let collection = collect_files(std::slice::from_ref(&dir), &map, None).unwrap();

        assert_eq!(collection.files.len(), 2);
        assert!(collection.files.iter().any(|path| path.ends_with("a.py")));
        assert!(collection
            .files
            .iter()
            .any(|path| path.ends_with("ordinary.O")));
        assert!(!collection
            .files
            .iter()
            .any(|path| path.ends_with("combined.O")));
        assert!(collection
            .skipped
            .iter()
            .any(|skip| skip.path.ends_with("combined.O")
                && skip.reason == "generated o-link output"));

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn marker_paths_are_relative_to_the_input_root() {
        let dir = scratch("relative_markers");
        let project = dir.join("project");
        let nested = project.join("src/nested");
        fs::create_dir_all(&nested).unwrap();
        fs::write(nested.join("main.py"), "print('ok')").unwrap();

        let map = default_extension_map();
        let backends = registered_backends();
        let collection = collect_files(std::slice::from_ref(&project), &map, None).unwrap();
        assert_eq!(collection.marker_root, absolute_path(&project).unwrap());
        let combined =
            link_files(&collection.files, &collection.marker_root, &map, &backends).unwrap();

        assert!(combined.contains("# ── src/nested/main.py ──"));
        assert!(!combined.contains(&project.display().to_string()));
        assert!(combined.contains(SECTION_LENGTH_PREFIX));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn marker_paths_use_the_common_root_of_multiple_inputs() {
        let dir = scratch("common_marker_root");
        let left = dir.join("left/src");
        let right = dir.join("right/lib");
        fs::create_dir_all(&left).unwrap();
        fs::create_dir_all(&right).unwrap();
        fs::write(left.join("main.py"), "print('left')\n").unwrap();
        fs::write(right.join("util.py"), "print('right')\n").unwrap();

        let map = default_extension_map();
        let backends = registered_backends();
        let collection = collect_files(&[left.clone(), right.clone()], &map, None).unwrap();
        assert_eq!(collection.marker_root, dir);
        let combined =
            link_files(&collection.files, &collection.marker_root, &map, &backends).unwrap();
        assert!(combined.contains("# ── left/src/main.py ──"));
        assert!(combined.contains("# ── right/lib/util.py ──"));

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn directory_walk_keeps_unknown_text_and_records_real_skips() {
        let dir = scratch("skip_report");
        fs::create_dir_all(dir.join("cache")).unwrap();
        fs::write(dir.join(".gitignore"), "*.py\n!keep.py\ncache/\n").unwrap();
        fs::write(dir.join(".olinkignore"), "notes.txt\n").unwrap();
        fs::write(dir.join("keep.py"), "print('keep')\n").unwrap();
        fs::write(dir.join("ignored.py"), "print('ignored')\n").unwrap();
        fs::write(dir.join("notes.txt"), "ignored by o-link\n").unwrap();
        fs::write(dir.join("README"), "extensionless\n").unwrap();
        fs::write(dir.join("unknown.xyz"), "unknown\n").unwrap();
        fs::write(dir.join("binary.rs"), [0xff_u8, 0x00]).unwrap();
        fs::write(dir.join(".hidden.py"), "hidden\n").unwrap();
        fs::write(dir.join("cache/generated.py"), "cached\n").unwrap();

        let map = default_extension_map();
        let collection =
            collect_files_with_skip_mode(std::slice::from_ref(&dir), &map, None, true).unwrap();
        assert_eq!(collection.files.len(), 3);
        assert!(collection
            .files
            .iter()
            .any(|path| path.ends_with("keep.py")));
        assert!(collection.files.iter().any(|path| path.ends_with("README")));
        assert!(collection
            .files
            .iter()
            .any(|path| path.ends_with("unknown.xyz")));

        let reasons = collection
            .skipped
            .iter()
            .map(|skip| skip.reason.as_str())
            .collect::<Vec<_>>();
        assert!(reasons.iter().any(|reason| reason.contains(".gitignore")));
        assert!(reasons.iter().any(|reason| reason.contains(".olinkignore")));
        assert!(reasons.contains(&"not UTF-8 text"));
        assert!(reasons.contains(&"hidden path"));
        assert!(collection
            .skipped
            .iter()
            .any(|skip| skip.path.ends_with("cache/generated.py")));

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn default_walk_records_excluded_subtrees_without_descending() {
        let dir = scratch("bounded_skips");
        fs::create_dir_all(dir.join(".hidden/deep")).unwrap();
        fs::create_dir_all(dir.join("cache/deep")).unwrap();
        fs::write(dir.join(".gitignore"), "cache/\n").unwrap();
        fs::write(dir.join("keep.py"), "print('keep')\n").unwrap();
        fs::write(dir.join(".hidden/deep/a.py"), "print('hidden')\n").unwrap();
        fs::write(dir.join("cache/deep/generated.py"), "print('cache')\n").unwrap();

        let map = default_extension_map();
        let default_collection = collect_files(std::slice::from_ref(&dir), &map, None).unwrap();
        assert_eq!(default_collection.files.len(), 1);
        assert_eq!(
            default_collection
                .skipped
                .iter()
                .filter(|skip| skip.path.ends_with(".hidden"))
                .count(),
            1
        );
        assert_eq!(
            default_collection
                .skipped
                .iter()
                .filter(|skip| skip.reason.contains(".gitignore"))
                .count(),
            1
        );
        assert!(!default_collection
            .skipped
            .iter()
            .any(|skip| skip.path.ends_with(".hidden/deep/a.py")));
        assert!(!default_collection
            .skipped
            .iter()
            .any(|skip| skip.path.ends_with("cache/deep/generated.py")));

        let verbose_collection =
            collect_files_with_skip_mode(std::slice::from_ref(&dir), &map, None, true).unwrap();
        assert!(verbose_collection
            .skipped
            .iter()
            .any(|skip| skip.path.ends_with(".hidden/deep/a.py")));
        assert!(verbose_collection
            .skipped
            .iter()
            .any(|skip| skip.path.ends_with("cache/deep/generated.py")));

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
        let collection = collect_files(std::slice::from_ref(&dir), &map, None).unwrap();
        assert_eq!(collection.files.len(), 1);

        let _ = fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn explicit_symlink_keeps_its_lexical_marker_path() {
        let dir = scratch("symlink_marker");
        let target_dir = dir.join("target");
        fs::create_dir_all(&target_dir).unwrap();
        let target = target_dir.join("target.py");
        let alias = dir.join("alias.py");
        fs::write(&target, "print('target')\n").unwrap();
        std::os::unix::fs::symlink(&target, &alias).unwrap();

        let map = default_extension_map();
        let backends = registered_backends();
        let collection = collect_files(std::slice::from_ref(&alias), &map, None).unwrap();
        let combined =
            link_files(&collection.files, &collection.marker_root, &map, &backends).unwrap();
        assert!(combined.contains("# ── alias.py ──"));
        assert!(!combined.contains("# ── target/target.py ──"));

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
        let collection = collect_files(std::slice::from_ref(&dir), &map, None).unwrap();
        let combined =
            link_files(&collection.files, &collection.marker_root, &map, &backends).unwrap();

        // Both Python files must appear with distinct [N] tags.
        assert!(
            combined.contains("python[0]^("),
            "expected python[0]^(, got:\n{}",
            combined
        );
        assert!(
            combined.contains("python[1]^("),
            "expected python[1]^(, got:\n{}",
            combined
        );
        assert!(
            combined.contains(")_python[0]"),
            "expected )_python[0], got:\n{}",
            combined
        );
        assert!(
            combined.contains(")_python[1]"),
            "expected )_python[1], got:\n{}",
            combined
        );
        // The shell file is the only file of its language so it gets [0].
        assert!(
            combined.contains("bash[0]{cap=backend}^("),
            "expected authority-scoped bash block, got:\n{}",
            combined
        );

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn link_files_env_ids_parse_cleanly() {
        let dir = scratch("env_parse");
        fs::write(dir.join("a.py"), "x = 1\n").unwrap();
        fs::write(dir.join("b.py"), "y = 2\n").unwrap();

        let map = default_extension_map();
        let backends = registered_backends();
        let collection = collect_files(std::slice::from_ref(&dir), &map, None).unwrap();
        let combined =
            link_files(&collection.files, &collection.marker_root, &map, &backends).unwrap();

        // The combined output must parse without errors.
        let mut parser = o_lang::parser::Parser::new(&combined, &backends);
        parser
            .parse()
            .expect("combined output with env_ids should parse");

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
        let collection = collect_files(std::slice::from_ref(&dir), &map, None).unwrap();
        let combined =
            link_files(&collection.files, &collection.marker_root, &map, &backends).unwrap();

        // a.py (the dependency) must appear before b.py in the output.
        let pos_a = combined.find("python[0]^(").expect("python[0] not found");
        let pos_b = combined.find("python[1]^(").expect("python[1] not found");
        assert!(
            pos_a < pos_b,
            "a.py (dependency) should be python[0] but positions are a={} b={}",
            pos_a,
            pos_b
        );
        // Verify a.py body is in python[0] slot.
        let slot0_start = combined.find("python[0]^(").unwrap();
        let slot0_end = combined.find(")_python[0]").unwrap();
        let slot0_body = &combined[slot0_start..slot0_end];
        assert!(
            slot0_body.contains("def helper"),
            "python[0] should contain a.py"
        );

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
        let collection = collect_files(std::slice::from_ref(&dir), &map, None).unwrap();
        let ordered = order_by_deps(&collection.files, &map);

        // The expected order after topo-sort: a.py, b.py, c.py.
        let names: Vec<&str> = ordered
            .iter()
            .filter_map(|p| p.file_name()?.to_str())
            .collect();
        assert_eq!(
            names,
            ["a.py", "b.py", "c.py"],
            "expected topo order a<b<c, got {:?}",
            names
        );

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn dependency_ordering_covers_hosted_language_import_forms() {
        let cases = [
            ("a.py", "z.py", "from .z import value\n"),
            ("a.js", "z.js", "import { value } from './z.js';\n"),
            ("a.rs", "z.rs", "mod z;\n"),
            ("a.c", "z.h", "#include \"z.h\"\n"),
            ("A.java", "Z.java", "import local.Z;\n"),
            ("A.hs", "Z.hs", "import Z\n"),
            ("a.rb", "z.rb", "require_relative './z'\n"),
            ("a.ml", "z.ml", "open Z\n"),
            ("a.rkt", "z.rkt", "(require \"z.rkt\")\n"),
            ("a.sh", "z.sh", "source ./z.sh\n"),
            ("a.nix", "z.nix", "let z = import ./z.nix; in z\n"),
            ("A.cs", "Z.cs", "using Z;\n"),
            ("a.m", "z.m", "run('z.m')\n"),
            ("a.wl", "z.wl", "Get[\"z.wl\"]\n"),
        ];

        for (importer, dependency, source) in cases {
            let dir = scratch(&format!("deps_{}", importer.replace('.', "_")));
            fs::write(dir.join(importer), source).unwrap();
            fs::write(dir.join(dependency), "dependency\n").unwrap();

            let map = default_extension_map();
            let collection = collect_files(std::slice::from_ref(&dir), &map, None).unwrap();
            let ordered = order_by_deps(&collection.files, &map);
            let names = ordered
                .iter()
                .filter_map(|path| path.file_name()?.to_str())
                .collect::<Vec<_>>();
            let dependency_position = names.iter().position(|name| *name == dependency).unwrap();
            let importer_position = names.iter().position(|name| *name == importer).unwrap();
            assert!(
                dependency_position < importer_position,
                "{importer} did not follow {dependency}: {names:?}"
            );

            let _ = fs::remove_dir_all(&dir);
        }
    }

    #[test]
    fn dependency_ordering_does_not_reorder_different_languages() {
        let dir = scratch("crosslang");
        fs::write(dir.join("a.py"), "x = 1\n").unwrap();
        fs::write(dir.join("b.sh"), "echo hi\n").unwrap();
        fs::write(dir.join("c.py"), "import a\n").unwrap();

        let map = default_extension_map();
        let collection = collect_files(std::slice::from_ref(&dir), &map, None).unwrap();
        let ordered = order_by_deps(&collection.files, &map);

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
        let collection = collect_files(std::slice::from_ref(&dir), &map, None).unwrap();
        // Should not panic; result has both files.
        let ordered = order_by_deps(&collection.files, &map);
        assert_eq!(ordered.len(), 2);

        let _ = fs::remove_dir_all(&dir);
    }
}
