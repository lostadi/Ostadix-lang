use crate::environment::{EnvironmentRefV2, EPHEMERAL_ENV_ID};
use crate::syntax_dialect::SyntaxDialect;
use anyhow::{bail, Result};
use sha2::{Digest, Sha256};

/// Languages whose bodies are SEQUENCED (children are O-level statements)
/// rather than SPLICED (children are raw source text for a target backend).
///
/// Inside a sequencing lang's body, the parser produces ONode::Call for
/// `name(...)` syntax and resolves LetBindings and nested TypedExprs as
/// structured ONodes rather than raw text destined for a foreign backend.
/// VarRefs remain meaningful in every backend because they are the splice
/// syntax used to pass OValues across evaluator boundaries.
///
/// `quote` is here because its body is the captured AST to wrap as an
/// OValue::Expr — evaluating its children as O-level statements is correct
/// (VarRefs and nested blocks need to round-trip through reconstruct_source).
/// `O` is here for the host-sequencing language (evaluates children
/// left-to-right as O-level statements).
const SEQUENCING_LANGS: &[&str] = &["quote", "O"];

#[derive(Debug, Clone, PartialEq)]
pub enum ONode {
    RawText(String),
    VarRef(String),
    LetBinding {
        name: String,
        expr: Box<ONode>,
    },
    TypedExpr {
        lang: String,
        env_id: u32,
        /// Optional comma-separated attributes parsed from `{...}` on the tag.
        /// `None` for plain `lang^(...)_lang`; `Some("lazy")` for
        /// `lang{lazy}^(...)_lang{lazy}`; `Some("defer")` for `{defer}`.
        /// The evaluator dispatches on this when present.
        attr: Option<String>,
        body: Vec<ONode>,
    },

    /// A function call: `name(arg1, arg2, ...)`.
    ///
    /// Introduced in STEP 2 as the surface syntax for the rung-climb operators
    /// `instantiate(expr)`, `realise(drv)`, and the explicit performer `now(req)`.
    /// Each arg is itself an ONode — args can be VarRef, nested Call, or a
    /// TypedExpr.
    ///
    /// Parsed at two positions for step 2:
    ///   1. The RHS of a let-binding:                  `let drv = instantiate($expr)`
    ///   2. As a top-level statement:                  `realise($drv)`
    ///
    /// Calls are NOT parsed inside typed expression bodies (the body is raw
    /// source text for the receiving backend; embedding O-level calls there
    /// would be ambiguous). STEP3 may lift this.
    Call {
        fn_name: String,
        args: Vec<ONode>,
    },
}

/// Half-open location of one executable syntax node in the exact UTF-8 source
/// handed to [`Parser`]. Byte offsets are zero-based; lines and columns are
/// one-based. Columns count Unicode scalar values, and the end position is the
/// first position after the node.
///
/// This is descriptive source provenance only. It deliberately does not live
/// in [`ONode`], OIR, or `ExecutionPlan`, so source layout cannot change their
/// structural equality or evidence/admission digests.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SourceSpanV1 {
    pub start_byte: usize,
    pub end_byte: usize,
    pub start_line: usize,
    pub start_column: usize,
    pub end_line: usize,
    pub end_column: usize,
}

impl SourceSpanV1 {
    /// The exact half-open byte range in the source passed to [`Parser::new`].
    pub fn byte_range(self) -> std::ops::Range<usize> {
        self.start_byte..self.end_byte
    }
}

/// Additive parse result carrying the unchanged syntax tree plus a source-span
/// sidecar in canonical executable-plan preorder.
///
/// `plan_origins()[index]` corresponds to the OIR node returned at the same
/// index by `OIrProgram::flatten_for_plan`. Bodies owned by `quote` are
/// intentionally absent because they are captured syntax, not executable plan
/// nodes. The parser captures the exact source digest and length, but a caller
/// associates this parser-relative map with any external source path.
///
/// Parsed nodes are read-only after construction:
///
/// ```compile_fail
/// fn mutate(document: &mut o_lang::parser::ParsedDocumentV1) {
///     document.nodes.clear();
/// }
/// ```
#[derive(Debug, Clone, PartialEq)]
pub struct ParsedDocumentV1 {
    nodes: Vec<ONode>,
    plan_origins: Vec<SourceSpanV1>,
    source_sha256: [u8; 32],
    source_len: usize,
}

impl ParsedDocumentV1 {
    /// The unchanged syntax nodes parsed from the source document.
    pub fn nodes(&self) -> &[ONode] {
        &self.nodes
    }

    pub fn plan_origins(&self) -> &[SourceSpanV1] {
        &self.plan_origins
    }

    /// Parser-computed identity of the exact UTF-8 bytes from which this
    /// document and its origin sidecar were produced.
    pub fn source_sha256(&self) -> &[u8; 32] {
        &self.source_sha256
    }

    pub fn source_len(&self) -> usize {
        self.source_len
    }

    pub fn origin_for_plan_index(&self, plan_index: usize) -> Option<&SourceSpanV1> {
        self.plan_origins.get(plan_index)
    }

    /// Historical 0.2 convenience accepting either a plain index or an IR
    /// `PlanNodeId`. The parser itself owns only syntax-relative indices; the
    /// `From<PlanNodeId> for usize` bridge lives in the lowering layer.
    pub fn origin_for_plan_node(&self, plan_node: impl Into<usize>) -> Option<&SourceSpanV1> {
        self.origin_for_plan_index(plan_node.into())
    }

    pub fn into_nodes(self) -> Vec<ONode> {
        self.nodes
    }
}

#[derive(Debug, Clone)]
struct Tag {
    start: usize,
    lang: String,
    env_id: u32,
    /// Optional attribute list on the language tag. The normalized string is
    /// carried through OIR while `raw` preserves the exact closer spelling.
    attr: Option<String>,
    /// The raw text of the tag — used to construct the closer match string.
    /// Includes the lang, the optional `[N]` env, and the optional `{attr}`,
    /// in source order.
    raw: String,
}

pub struct Parser<'a> {
    source: &'a str,
    pos: usize,
    line: usize,
    line_starts: Option<Vec<usize>>,
    plan_origins: Option<Vec<SourceSpanV1>>,
    origin_suppression_depth: usize,
    syntax_dialect: &'a dyn SyntaxDialect,
}

impl<'a> Parser<'a> {
    pub fn new(source: &'a str, syntax_dialect: &'a dyn SyntaxDialect) -> Self {
        Self {
            source,
            pos: 0,
            line: 1,
            line_starts: None,
            plan_origins: None,
            origin_suppression_depth: 0,
            syntax_dialect,
        }
    }

    pub fn parse(&mut self) -> Result<Vec<ONode>> {
        self.line_starts = None;
        self.plan_origins = None;
        self.parse_until(None)
    }

    /// Parse the unchanged [`ONode`] forest and record one source span for each
    /// node that canonical OIR lowering will allocate into `ExecutionPlan`.
    pub fn parse_with_origins(&mut self) -> Result<ParsedDocumentV1> {
        let mut line_starts = vec![0];
        line_starts.extend(
            self.source
                .bytes()
                .enumerate()
                .filter_map(|(index, byte)| (byte == b'\n').then_some(index + 1)),
        );
        self.line_starts = Some(line_starts);
        self.plan_origins = Some(Vec::new());
        self.origin_suppression_depth = 0;
        let nodes = self.parse_until(None)?;
        let plan_origins = self
            .plan_origins
            .take()
            .expect("source-origin recording was enabled for this parse");
        Ok(ParsedDocumentV1 {
            nodes,
            plan_origins,
            source_sha256: Sha256::digest(self.source.as_bytes()).into(),
            source_len: self.source.len(),
        })
    }

    fn parse_until(&mut self, expected_closer: Option<&Tag>) -> Result<Vec<ONode>> {
        let mut nodes = Vec::new();
        let mut text_start = self.pos;
        let inside_sequencing = expected_closer
            .map(|tag| SEQUENCING_LANGS.contains(&tag.lang.as_str()))
            .unwrap_or(true);

        while self.pos < self.source.len() {
            if let Some(tag) = expected_closer {
                if let Some(closer_len) = self.exact_closer_len_at(self.pos, tag) {
                    self.flush_text(&mut nodes, text_start, self.pos);
                    self.advance_bytes(closer_len);
                    return Ok(nodes);
                }
            }

            // Skip `#` line comments at the top level and inside sequencing
            // langs (quote^, O^).  Inside other typed-expression bodies the
            // `#` character is valid syntax (e.g. markdown headings, Python
            // comments) and must NOT be swallowed by the Ostadix-lang parser.
            if inside_sequencing && self.current_byte() == Some(b'#') {
                self.flush_text(&mut nodes, text_start, self.pos);
                self.skip_to_end_of_line();
                text_start = self.pos;
                continue;
            }

            // A foreign backend owns ordinary `let name = ...` syntax in its
            // body. Only the O document and sequencing backends may turn it
            // into an ONode::LetBinding.
            if inside_sequencing && self.starts_with_let_keyword() {
                let let_start = self.pos;
                let origin_checkpoint = self.origin_count();
                if let Some(binding) = self.try_parse_let_binding()? {
                    self.flush_text_before_recorded(
                        &mut nodes,
                        text_start,
                        let_start,
                        origin_checkpoint,
                    );
                    nodes.push(binding);
                    text_start = self.pos;
                    continue;
                }
            }

            // Backslash escape: `\IDENT^(` or `\)_IDENT` are emitted as the
            // literal text of the opener/closer without triggering expression
            // parsing. This lets O source code contain opener/closer syntax as
            // raw text — e.g. `python^(src = "\python^(1)_python")_python`
            // where `\python^(` is a literal Python string, not a nested expr.
            if self.current_byte() == Some(b'\\') {
                // Check if a registered opener follows the backslash.
                let after_bs = self.pos + 1;
                if after_bs < self.source.len() {
                    let temp_pos = self.pos;
                    self.pos = after_bs;
                    let had_opener = if let Some(tag) = self.try_parse_opener()? {
                        // Emit the literal opener text (including `^(`) as raw text.
                        // `tag.raw` is `lang[N]?{attr}?`; we need `lang[N]?{attr}?^(`
                        let literal = format!("{}^(", tag.raw);
                        // flush everything up to (not including) the backslash
                        self.flush_text(&mut nodes, text_start, temp_pos);
                        // push the literal opener text
                        if let Some(ONode::RawText(s)) = nodes.last_mut() {
                            s.push_str(&literal);
                            self.extend_last_origin(self.pos);
                        } else {
                            self.record_origin(temp_pos, self.pos);
                            nodes.push(ONode::RawText(literal));
                        }
                        text_start = self.pos;
                        true
                    } else {
                        self.pos = temp_pos;
                        false
                    };
                    if had_opener {
                        continue;
                    }

                    // Check if the matching closer follows the backslash.
                    if let Some(tag) = expected_closer {
                        if let Some(closer_len) = self.exact_closer_len_at(after_bs, tag) {
                            let closer = self.source[after_bs..after_bs + closer_len].to_string();
                            self.flush_text(&mut nodes, text_start, self.pos);
                            self.pos = after_bs + closer_len;
                            if let Some(ONode::RawText(s)) = nodes.last_mut() {
                                s.push_str(&closer);
                                self.extend_last_origin(self.pos);
                            } else {
                                self.record_origin(temp_pos, self.pos);
                                nodes.push(ONode::RawText(closer));
                            }
                            text_start = self.pos;
                            continue;
                        }
                    }

                    // Check for \$ — emit a literal `$` and continue parsing the
                    // following source as raw text. This covers shell variables
                    // like \$PATH and shell arithmetic like \$((x + y)) inside
                    // bash^(...)_bash / shell^(...)_shell blocks.
                    if self.source.as_bytes()[after_bs] == b'$' {
                        self.flush_text(&mut nodes, text_start, temp_pos);
                        if let Some(ONode::RawText(s)) = nodes.last_mut() {
                            s.push('$');
                            self.extend_last_origin(after_bs + 1);
                        } else {
                            self.record_origin(temp_pos, after_bs + 1);
                            nodes.push(ONode::RawText("$".to_string()));
                        }
                        self.pos = after_bs + 1;
                        text_start = self.pos;
                        continue;
                    }
                }
            }

            if self.current_byte() == Some(b'$') {
                if let Some(name) = self.try_parse_var_ref()? {
                    let var_start = self.pos_before_var(&name);
                    self.flush_text(&mut nodes, text_start, var_start);
                    self.record_origin(var_start, self.pos);
                    nodes.push(ONode::VarRef(name));
                    text_start = self.pos;
                    continue;
                }
            }

            if let Some(tag) = self.try_parse_opener()? {
                let opener_start = tag.start;
                self.flush_text(&mut nodes, text_start, opener_start);
                nodes.push(self.parse_typed_expr(tag)?);

                text_start = self.pos;
                continue;
            }

            // STEP-2/3: try to parse a call like `instantiate($x)` or
            // `realise(instantiate($x))`. Allowed at the document top level
            // AND inside the bodies of SEQUENCING_LANGS (lazy^, eventually O^
            // and quote^). Disallowed inside ordinary typed-expr bodies so
            // that source text destined for a backend isn't reinterpreted.
            if inside_sequencing {
                let stmt_start = self.pos;
                let origin_checkpoint = self.origin_count();
                if let Some(call) = self.try_parse_call()? {
                    self.flush_text_before_recorded(
                        &mut nodes,
                        text_start,
                        stmt_start,
                        origin_checkpoint,
                    );
                    nodes.push(call);
                    text_start = self.pos;
                    continue;
                }
            }

            self.advance_one_byte();
        }

        if let Some(tag) = expected_closer {
            bail!(
                "Line {}: Unclosed expression, expected )_{}",
                self.line,
                tag.raw
            );
        }

        self.flush_text(&mut nodes, text_start, self.pos);
        Ok(nodes)
    }

    /// Finish a typed expression after its opener has been consumed. Recording
    /// the parent before descending reproduces `ExecutionPlan`'s preorder.
    fn parse_typed_expr(&mut self, tag: Tag) -> Result<ONode> {
        let origin = self.begin_origin(tag.start);
        let owns_quoted_syntax = self.syntax_dialect.owns_quoted_syntax(&tag.lang);
        if owns_quoted_syntax {
            self.origin_suppression_depth += 1;
        }
        let body = self.parse_until(Some(&tag));
        if owns_quoted_syntax {
            self.origin_suppression_depth -= 1;
        }
        let body = body?;
        self.finish_origin(origin, self.pos);
        Ok(ONode::TypedExpr {
            lang: tag.lang,
            env_id: tag.env_id,
            attr: tag.attr,
            body,
        })
    }

    fn try_parse_let_binding(&mut self) -> Result<Option<ONode>> {
        let original_pos = self.pos;

        if !self.starts_with_let_keyword() {
            return Ok(None);
        }

        self.advance_bytes(3);
        self.skip_horizontal_whitespace();

        let name = match self.parse_identifier() {
            Some(name) => name,
            None => {
                self.pos = original_pos;
                return Ok(None);
            }
        };

        self.skip_horizontal_whitespace();

        if self.current_byte() != Some(b'=') {
            self.pos = original_pos;
            return Ok(None);
        }

        self.advance_one_byte();
        let origin = self.begin_origin(original_pos);
        self.skip_whitespace();

        // STEP-2: a let RHS may now be a Call (instantiate(...), realise(...))
        // in addition to a typed expression. Try Call first; on miss, fall
        // through to the typed-expression path.
        if let Some(call) = self.try_parse_call()? {
            self.finish_origin(origin, self.pos);
            return Ok(Some(ONode::LetBinding {
                name,
                expr: Box::new(call),
            }));
        }

        let tag = match self.try_parse_opener()? {
            Some(tag) => tag,
            None => {
                bail!(
                    "Line {}: let binding `{}` must be assigned a typed expression \
                     or a call",
                    self.line,
                    name
                );
            }
        };

        let expr = self.parse_typed_expr(tag)?;
        self.finish_origin(origin, self.pos);

        Ok(Some(ONode::LetBinding {
            name,
            expr: Box::new(expr),
        }))
    }

    /// Try to parse a function call: `name(arg1, arg2, ...)`.
    ///
    /// Returns `Ok(Some(call))` on a successful parse, `Ok(None)` if the input
    /// at the current position isn't a call (so the caller can try other
    /// productions), and `Err(_)` if it starts to look like a call but is
    /// malformed mid-parse (we commit to the call path once we've seen
    /// `name(`).
    ///
    /// Arguments are themselves ONodes: VarRef (`$name`), nested Call, or a
    /// typed backend expression. The typed-expression case is what lets an
    /// explicit coordination group own the operations it coordinates instead
    /// of forcing users to evaluate them first in separate `let` bindings.
    fn try_parse_call(&mut self) -> Result<Option<ONode>> {
        let original_pos = self.pos;
        let original_line = self.line;

        let name = match self.parse_identifier() {
            Some(n) => n,
            None => return Ok(None),
        };

        // The opener of a TypedExpr is `name(` BUT with `name` being a
        // registered backend (or `name[N](`). For a call we want plain
        // `name(` with `name` NOT being a registered backend (otherwise it
        // would be ambiguous with a typed expression with no body).
        if self.syntax_dialect.is_registered_syntax_tag(&name) || self.current_byte() != Some(b'(')
        {
            self.pos = original_pos;
            self.line = original_line;
            return Ok(None);
        }

        // Commit: from here on, errors are real errors.
        let origin = self.begin_origin(original_pos);
        self.advance_one_byte(); // consume '('
        self.skip_call_trivia();

        let mut args = Vec::new();
        loop {
            if self.current_byte() == Some(b')') {
                self.advance_one_byte();
                break;
            }

            // Each arg is a VarRef, nested Call, or typed backend expression.
            let arg = if self.current_byte() == Some(b'$') {
                let start = self.pos;
                let var = self.try_parse_var_ref()?.ok_or_else(|| {
                    anyhow::anyhow!("Line {}: expected variable reference after $", self.line)
                })?;
                self.record_origin(start, self.pos);
                ONode::VarRef(var)
            } else if let Some(nested) = self.try_parse_call()? {
                nested
            } else if let Some(tag) = self.try_parse_opener()? {
                self.parse_typed_expr(tag)?
            } else {
                bail!(
                    "Line {}: in call `{}(...)`, expected $var, nested call, or typed expression",
                    self.line,
                    name
                );
            };
            args.push(arg);

            self.skip_call_trivia();
            match self.current_byte() {
                Some(b',') => {
                    self.advance_one_byte();
                    self.skip_call_trivia();
                }
                Some(b')') => {
                    self.advance_one_byte();
                    break;
                }
                _ => bail!(
                    "Line {}: in call `{}(...)`, expected ',' or ')'",
                    self.line,
                    name
                ),
            }
        }

        self.finish_origin(origin, self.pos);
        Ok(Some(ONode::Call {
            fn_name: name,
            args,
        }))
    }

    fn try_parse_var_ref(&mut self) -> Result<Option<String>> {
        let start = self.pos;

        if self.current_byte() != Some(b'$') {
            return Ok(None);
        }

        let name_start = start + 1;
        if name_start >= self.source.len() {
            return Ok(None);
        }

        let b = self.source.as_bytes()[name_start];
        if !is_ident_start(b) {
            return Ok(None);
        }

        let mut end = name_start + 1;
        while end < self.source.len() && is_ident_continue(self.source.as_bytes()[end]) {
            end += 1;
        }

        let name = self.source[name_start..end].to_string();
        self.pos = end;
        Ok(Some(name))
    }

    fn try_parse_opener(&mut self) -> Result<Option<Tag>> {
        let start = self.pos;
        let bytes = self.source.as_bytes();

        if start >= bytes.len() || !is_ident_start(bytes[start]) {
            return Ok(None);
        }

        let mut i = start + 1;
        while i < bytes.len() && is_ident_continue(bytes[i]) {
            i += 1;
        }

        let lang = self.source[start..i].to_string();

        if !self.syntax_dialect.is_registered_syntax_tag(&lang) {
            return Ok(None);
        }

        let mut env_id = EPHEMERAL_ENV_ID;
        let mut raw = lang.clone();

        if i < bytes.len() && bytes[i] == b'[' {
            let env_start = i;
            i += 1;

            if i < bytes.len() && bytes[i] == b'*' {
                i += 1;
                if i >= bytes.len() || bytes[i] != b']' {
                    return Ok(None);
                }
                env_id = EnvironmentRefV2::LinkerIsolated.encoded();
                i += 1;
            } else {
                let digits_start = i;
                while i < bytes.len() && bytes[i].is_ascii_digit() {
                    i += 1;
                }

                if digits_start == i {
                    return Ok(None);
                }

                if i >= bytes.len() || bytes[i] != b']' {
                    return Ok(None);
                }

                let digits = &self.source[digits_start..i];
                let numeric = digits.parse::<u32>()?;
                env_id = EnvironmentRefV2::persistent(numeric)
                    .map_err(anyhow::Error::from)?
                    .encoded();
                i += 1;
            }

            raw.push_str(&self.source[env_start..i]);
        }

        // Optional comma-separated `{attr}` list after the env slot. Entries
        // are identifiers or `name=value`; whitespace around entries is
        // ignored. Attribute values use a single-line visible-ASCII
        // vocabulary that covers capability names and semantic resource
        // declarations such as `project:src+host:/etc/hosts` without making
        // braces, commas, or newlines ambiguous. The exact source spelling
        // remains part of `raw` so closer matching stays literal.
        let mut attr: Option<String> = None;
        if i < bytes.len() && bytes[i] == b'{' {
            let attr_start = i;
            i += 1;

            let content_start = i;
            while i < bytes.len() && bytes[i] != b'}' {
                let byte = bytes[i];
                if !(is_attribute_value_continue(byte)
                    || matches!(byte, b',' | b'=' | b' ' | b'\t'))
                {
                    if attribute_suffix_closes_as_opener(bytes, i) {
                        bail!(
                            "Invalid character in block attribute list at line {}",
                            self.line
                        );
                    }
                    return Ok(None);
                }
                i += 1;
            }
            if i >= bytes.len() || bytes[i] != b'}' {
                return Ok(None);
            }
            let entries = self.source[content_start..i]
                .split(',')
                .map(str::trim)
                .collect::<Vec<_>>();
            if entries.is_empty() || entries.iter().any(|entry| entry.is_empty()) {
                if attribute_suffix_closes_as_opener(bytes, i) {
                    bail!("Empty block attribute at line {}", self.line);
                }
                return Ok(None);
            }
            for entry in &entries {
                let mut parts = entry.split('=');
                let name = parts.next().unwrap();
                let value = parts.next();
                if parts.next().is_some()
                    || !name
                        .as_bytes()
                        .first()
                        .is_some_and(|byte| is_ident_start(*byte))
                    || !name.as_bytes().iter().copied().all(is_ident_continue)
                    || value.is_some_and(|value| {
                        value.is_empty()
                            || !value
                                .as_bytes()
                                .iter()
                                .copied()
                                .all(is_attribute_value_continue)
                    })
                {
                    if attribute_suffix_closes_as_opener(bytes, i) {
                        bail!("Malformed block attribute at line {}", self.line);
                    }
                    return Ok(None);
                }
            }

            attr = Some(entries.join(","));
            i += 1; // past '}'

            raw.push_str(&self.source[attr_start..i]);
        }

        if bytes.get(i..i + 2) == Some(b"^(") {
            self.pos = i + 2;
            // Canonicalize alias tags (`py` → `python`, `md` → `markdown`,
            // …) so the AST, evaluator env keys, and shim resolution all see
            // the canonical name. `raw` keeps the source spelling so the
            // closer `)_py` still matches its opener.
            let lang = self.syntax_dialect.canonical_syntax_name(&lang);
            Ok(Some(Tag {
                start,
                lang,
                env_id,
                attr,
                raw,
            }))
        } else {
            Ok(None)
        }
    }

    fn parse_identifier(&mut self) -> Option<String> {
        let start = self.pos;
        let bytes = self.source.as_bytes();

        if start >= bytes.len() || !is_ident_start(bytes[start]) {
            return None;
        }

        let mut end = start + 1;
        while end < bytes.len() && is_ident_continue(bytes[end]) {
            end += 1;
        }

        self.pos = end;
        Some(self.source[start..end].to_string())
    }

    fn starts_with_let_keyword(&self) -> bool {
        if !self.source[self.pos..].starts_with("let") {
            return false;
        }

        let before_ok = if self.pos == 0 {
            true
        } else {
            self.source[..self.pos]
                .chars()
                .next_back()
                .map(|c| c.is_whitespace())
                .unwrap_or(true)
        };

        let after = self.pos + 3;
        let after_ok = if after >= self.source.len() {
            true
        } else {
            self.source[after..]
                .chars()
                .next()
                .map(|c| c.is_whitespace())
                .unwrap_or(true)
        };

        before_ok && after_ok
    }

    fn skip_horizontal_whitespace(&mut self) {
        while matches!(self.current_byte(), Some(b' ' | b'\t')) {
            self.advance_one_byte();
        }
    }

    fn skip_whitespace(&mut self) {
        while matches!(self.current_byte(), Some(b' ' | b'\t' | b'\n' | b'\r')) {
            self.advance_one_byte();
        }
    }

    /// O-level calls are sequencing syntax, so line comments are trivia
    /// between arguments just as they are between top-level statements.
    fn skip_call_trivia(&mut self) {
        loop {
            self.skip_whitespace();
            if self.current_byte() != Some(b'#') {
                break;
            }
            self.skip_to_end_of_line();
        }
    }

    /// Advance past everything up to and including the next newline (or EOF).
    /// Used to skip `#` line comments.
    fn skip_to_end_of_line(&mut self) {
        while self.pos < self.source.len() {
            let b = self.source.as_bytes()[self.pos];
            self.advance_one_byte();
            if b == b'\n' {
                break;
            }
        }
    }

    fn flush_text(&mut self, nodes: &mut Vec<ONode>, start: usize, end: usize) {
        if end > start {
            self.record_origin(start, end);
            nodes.push(ONode::RawText(self.source[start..end].to_string()));
        }
    }

    /// Let-bindings and calls are parsed speculatively before the preceding raw
    /// text can be flushed. Insert that text's span before the origins recorded
    /// by the successful parse so sidecar order still matches syntax preorder.
    fn flush_text_before_recorded(
        &mut self,
        nodes: &mut Vec<ONode>,
        start: usize,
        end: usize,
        origin_checkpoint: usize,
    ) {
        if end <= start {
            return;
        }
        if self.origin_suppression_depth == 0 && self.plan_origins.is_some() {
            let span = self.source_span(start, end);
            if let Some(origins) = self.plan_origins.as_mut() {
                origins.insert(origin_checkpoint, span);
            }
        }
        nodes.push(ONode::RawText(self.source[start..end].to_string()));
    }

    fn origin_count(&self) -> usize {
        self.plan_origins.as_ref().map_or(0, Vec::len)
    }

    fn begin_origin(&mut self, start: usize) -> Option<usize> {
        if self.origin_suppression_depth != 0 || self.plan_origins.is_none() {
            return None;
        }
        let span = self.source_span(start, start);
        let origins = self
            .plan_origins
            .as_mut()
            .expect("origin inventory was checked above");
        let index = origins.len();
        origins.push(span);
        Some(index)
    }

    fn finish_origin(&mut self, index: Option<usize>, end: usize) {
        let Some(index) = index else {
            return;
        };
        let (_, end_line, end_column) = self.source_position(end);
        let origin = &mut self
            .plan_origins
            .as_mut()
            .expect("an active origin must have an inventory")[index];
        origin.end_byte = end;
        origin.end_line = end_line;
        origin.end_column = end_column;
    }

    fn record_origin(&mut self, start: usize, end: usize) {
        if self.origin_suppression_depth != 0 || self.plan_origins.is_none() {
            return;
        }
        let span = self.source_span(start, end);
        self.plan_origins
            .as_mut()
            .expect("origin inventory was checked above")
            .push(span);
    }

    /// Escaped syntax is appended to the parser's preceding RawText node. Its
    /// sidecar therefore expands to the same bounding source extent.
    fn extend_last_origin(&mut self, end: usize) {
        if self.origin_suppression_depth != 0 || self.plan_origins.is_none() {
            return;
        }
        let (_, end_line, end_column) = self.source_position(end);
        let Some(origin) = self
            .plan_origins
            .as_mut()
            .expect("origin inventory was checked above")
            .last_mut()
        else {
            return;
        };
        origin.end_byte = end;
        origin.end_line = end_line;
        origin.end_column = end_column;
    }

    fn source_span(&self, start: usize, end: usize) -> SourceSpanV1 {
        let (start_byte, start_line, start_column) = self.source_position(start);
        let (end_byte, end_line, end_column) = self.source_position(end);
        SourceSpanV1 {
            start_byte,
            end_byte,
            start_line,
            start_column,
            end_line,
            end_column,
        }
    }

    fn source_position(&self, byte: usize) -> (usize, usize, usize) {
        debug_assert!(byte <= self.source.len());
        debug_assert!(self.source.is_char_boundary(byte));
        let line_starts = self
            .line_starts
            .as_ref()
            .expect("source positions are only requested while recording origins");
        let line_index = line_starts
            .partition_point(|line_start| *line_start <= byte)
            .saturating_sub(1);
        let line_start = line_starts[line_index];
        let column = self.source[line_start..byte].chars().count() + 1;
        (byte, line_index + 1, column)
    }

    /// Return the byte length of the matching closer only when the expected
    /// raw tag is the complete lexical tag at `position`. A prefix comparison
    /// would let a bare `)_python` consume `)_python[*]`, or let
    /// `)_python[1]` consume the prefix of `)_python[1]{lazy}`.
    fn exact_closer_len_at(&self, position: usize, tag: &Tag) -> Option<usize> {
        let closer = format!(")_{}", tag.raw);
        let remaining = self.source.get(position..)?;
        if !remaining.starts_with(&closer) {
            return None;
        }

        let next = remaining.as_bytes().get(closer.len()).copied();
        let has_environment = tag.raw.as_bytes().contains(&b'[');
        let has_attributes = tag.attr.is_some();
        let extends_tag = if has_attributes {
            false
        } else if has_environment {
            next == Some(b'{')
        } else {
            next.is_some_and(is_ident_continue) || matches!(next, Some(b'[' | b'{'))
        };
        (!extends_tag).then_some(closer.len())
    }

    fn current_byte(&self) -> Option<u8> {
        self.source.as_bytes().get(self.pos).copied()
    }

    fn advance_one_byte(&mut self) {
        if self.pos >= self.source.len() {
            return;
        }

        let ch = self.source[self.pos..]
            .chars()
            .next()
            .expect("parser position should be inside source");

        if ch == '\n' {
            self.line += 1;
        }

        self.pos += ch.len_utf8();
    }

    fn advance_bytes(&mut self, n: usize) {
        for _ in 0..n {
            self.advance_one_byte();
        }
    }

    fn pos_before_var(&self, name: &str) -> usize {
        self.pos - name.len() - 1
    }
}

fn is_ident_start(b: u8) -> bool {
    b.is_ascii_alphabetic() || b == b'_'
}

fn is_ident_continue(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

/// Bytes accepted inside the value half of a block attribute. This deliberately
/// excludes the parser's structural separators: comma separates
/// entries, `=` separates name from value, and braces delimit the attribute
/// list. Whitespace and newlines are not value bytes. Other visible ASCII
/// punctuation is safe here and permits resource schemes, paths, endpoints,
/// and `+`/`;` resource lists without assigning them semantic meaning.
fn is_attribute_value_continue(b: u8) -> bool {
    b.is_ascii_graphic() && !matches!(b, b'{' | b'}' | b',' | b'=')
}

fn attribute_suffix_closes_as_opener(bytes: &[u8], start: usize) -> bool {
    for (offset, byte) in bytes[start..].iter().enumerate() {
        match byte {
            b'\n' | b'\r' => return false,
            b'}' => {
                let close = start + offset;
                return bytes.get(close + 1..close + 3) == Some(b"^(");
            }
            _ => {}
        }
    }
    false
}

// ─────────────────────────────────────────────────────────────────────────────
// Source reconstruction
//
// Converts a slice of ONodes back into O source text. Used by the `quote^`
// evaluator to capture the body as a re-evaluable `OValue::Expr { src }`.
//
// Reconstruction is lossless for all structural information (nesting, envs,
// attrs, var refs, let bindings) but does NOT preserve formatting whitespace
// that was between tokens (e.g., blank lines inside a Python body are
// preserved as RawText, but leading/trailing whitespace that the parser
// merged into adjacent RawText nodes may differ from the original source).
// This is sufficient for re-evaluation via `O.eval`.
// ─────────────────────────────────────────────────────────────────────────────

/// Reconstruct O source text from a slice of ONodes.
///
/// Used by `quote^(...)_quote` to capture the body as `OValue::Expr { src }`.
/// The resulting string, when parsed again with the same registered-backends
/// set, produces an equivalent ONode tree.
pub fn reconstruct_source(nodes: &[ONode]) -> String {
    let mut buf = String::new();
    for node in nodes {
        reconstruct_node(node, &mut buf);
    }
    buf
}

fn reconstruct_node(node: &ONode, buf: &mut String) {
    match node {
        ONode::RawText(s) => buf.push_str(s),

        ONode::VarRef(name) => {
            buf.push('$');
            buf.push_str(name);
        }

        ONode::LetBinding { name, expr } => {
            buf.push_str("let ");
            buf.push_str(name);
            buf.push_str(" = ");
            reconstruct_node(expr, buf);
        }

        ONode::TypedExpr {
            lang,
            env_id,
            attr,
            body,
        } => {
            // opener: lang[N]? / lang[*]? followed by attributes and ^(
            buf.push_str(lang);
            if let Some(marker) = EnvironmentRefV2::from_encoded(*env_id).source_marker() {
                buf.push_str(&marker);
            }
            if let Some(a) = attr {
                buf.push('{');
                buf.push_str(a);
                buf.push('}');
            }
            buf.push_str("^(");
            // body
            for child in body {
                reconstruct_node(child, buf);
            }
            // closer: )_lang[N]?{attr}?
            buf.push(')');
            buf.push('_');
            buf.push_str(lang);
            if let Some(marker) = EnvironmentRefV2::from_encoded(*env_id).source_marker() {
                buf.push_str(&marker);
            }
            if let Some(a) = attr {
                buf.push('{');
                buf.push_str(a);
                buf.push('}');
            }
        }

        ONode::Call { fn_name, args } => {
            buf.push_str(fn_name);
            buf.push('(');
            for (i, arg) in args.iter().enumerate() {
                if i > 0 {
                    buf.push_str(", ");
                }
                reconstruct_node(arg, buf);
            }
            buf.push(')');
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;
    use crate::ir::PlanNodeId;

    fn make_backends(tags: &[&str]) -> HashSet<String> {
        tags.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn reconstruct_roundtrips_raw_text() {
        let src = "hello world";
        let backends = make_backends(&["python"]);
        let nodes = Parser::new(src, &backends).parse().unwrap();
        assert_eq!(reconstruct_source(&nodes), src);
    }

    #[test]
    fn source_origin_sidecar_preserves_existing_syntax_and_oir() {
        let source = "prefix\nlet answer = python^(40 + 2)_python\nautonomous(batch(python^($answer)_python, quote^(python^(hidden)_python)_quote))";
        let backends = make_backends(&["python", "quote"]);
        let ordinary = Parser::new(source, &backends).parse().unwrap();
        let sourced = Parser::new(source, &backends).parse_with_origins().unwrap();

        assert_eq!(sourced.nodes(), ordinary);
        let ordinary_oir = crate::ir::OIrProgram::lower(&ordinary);
        let sourced_oir = crate::ir::OIrProgram::lower(sourced.nodes());
        assert_eq!(sourced_oir, ordinary_oir);
        assert_eq!(sourced_oir.to_text(), ordinary_oir.to_text());
        assert_eq!(
            sourced.plan_origins().len(),
            sourced_oir.flatten_for_plan().len()
        );
    }

    #[test]
    fn source_origins_follow_canonical_plan_preorder_and_exclude_quote_bodies() {
        let source = "quote^(python^($hidden)_python)_quote\npython^($visible)_python";
        let backends = make_backends(&["python", "quote"]);
        let sourced = Parser::new(source, &backends).parse_with_origins().unwrap();
        let program = crate::ir::OIrProgram::lower(sourced.nodes());
        let plan = program.plan();

        let slices = sourced
            .plan_origins()
            .iter()
            .map(|origin| &source[origin.byte_range()])
            .collect::<Vec<_>>();
        assert_eq!(
            slices,
            [
                "quote^(python^($hidden)_python)_quote",
                "\n",
                "python^($visible)_python",
                "$visible",
            ]
        );
        assert_eq!(sourced.plan_origins().len(), plan.nodes.len());
        assert_eq!(program.flatten_for_plan().len(), plan.nodes.len());
        assert_eq!(
            sourced.origin_for_plan_node(PlanNodeId(2)),
            sourced.origin_for_plan_index(2)
        );
        assert!(sourced.origin_for_plan_node(PlanNodeId(4)).is_none());
    }

    #[test]
    fn source_origins_report_half_open_utf8_byte_line_and_scalar_columns() {
        let source = "é\npython^(\n$x\n)_python";
        let backends = make_backends(&["python"]);
        let sourced = Parser::new(source, &backends).parse_with_origins().unwrap();
        let origins = sourced.plan_origins();

        assert_eq!(
            origins,
            [
                SourceSpanV1 {
                    start_byte: 0,
                    end_byte: 3,
                    start_line: 1,
                    start_column: 1,
                    end_line: 2,
                    end_column: 1,
                },
                SourceSpanV1 {
                    start_byte: 3,
                    end_byte: 23,
                    start_line: 2,
                    start_column: 1,
                    end_line: 4,
                    end_column: 9,
                },
                SourceSpanV1 {
                    start_byte: 11,
                    end_byte: 12,
                    start_line: 2,
                    start_column: 9,
                    end_line: 3,
                    end_column: 1,
                },
                SourceSpanV1 {
                    start_byte: 12,
                    end_byte: 14,
                    start_line: 3,
                    start_column: 1,
                    end_line: 3,
                    end_column: 3,
                },
                SourceSpanV1 {
                    start_byte: 14,
                    end_byte: 15,
                    start_line: 3,
                    start_column: 3,
                    end_line: 4,
                    end_column: 1,
                },
            ]
        );
    }

    #[test]
    fn source_origins_keep_raw_text_before_speculatively_parsed_nodes_in_order() {
        let source = "raw let x = python^(1)_python tail autonomous($x)";
        let backends = make_backends(&["python"]);
        let sourced = Parser::new(source, &backends).parse_with_origins().unwrap();
        let slices = sourced
            .plan_origins()
            .iter()
            .map(|origin| &source[origin.byte_range()])
            .collect::<Vec<_>>();

        assert_eq!(
            slices,
            [
                "raw ",
                "let x = python^(1)_python",
                "python^(1)_python",
                "1",
                " tail ",
                "autonomous($x)",
                "$x",
            ]
        );
    }

    #[test]
    fn reconstruct_roundtrips_typed_expr() {
        let src = "python^(6 * 7)_python";
        let backends = make_backends(&["python"]);
        let nodes = Parser::new(src, &backends).parse().unwrap();
        assert_eq!(reconstruct_source(&nodes), src);
    }

    #[test]
    fn bare_closer_cannot_consume_linker_isolated_closer_prefix() {
        let src = "python^(6 * 7)_python[*]";
        let backends = make_backends(&["python"]);
        let error = Parser::new(src, &backends).parse().unwrap_err();
        let message = error.to_string();
        assert!(message.contains("Unclosed expression"), "{message}");
        assert!(message.contains(")_python"), "{message}");
    }

    #[test]
    fn environment_closer_cannot_consume_attributed_closer_prefix() {
        let src = "python[7]^(6 * 7)_python[7]{defer}";
        let backends = make_backends(&["python"]);
        let error = Parser::new(src, &backends).parse().unwrap_err();
        let message = error.to_string();
        assert!(message.contains("Unclosed expression"), "{message}");
        assert!(message.contains(")_python[7]"), "{message}");
    }

    #[test]
    fn linker_isolated_environment_roundtrips_without_becoming_persistent() {
        let src = "python[*]^(6 * 7)_python[*]";
        let backends = make_backends(&["python"]);
        let nodes = Parser::new(src, &backends).parse().unwrap();
        let ONode::TypedExpr { env_id, .. } = &nodes[0] else {
            panic!("expected typed expression")
        };
        assert_eq!(
            EnvironmentRefV2::from_encoded(*env_id),
            EnvironmentRefV2::LinkerIsolated
        );
        assert_eq!(reconstruct_source(&nodes), src);
    }

    #[test]
    fn numeric_environment_cannot_alias_reserved_fresh_sentinels() {
        let backends = make_backends(&["python"]);
        for reserved in [
            crate::environment::LINKER_ISOLATED_ENV_ID,
            crate::environment::EPHEMERAL_ENV_ID,
        ] {
            let src = format!("python[{reserved}]^(1)_python[{reserved}]");
            let error = Parser::new(&src, &backends).parse().unwrap_err();
            assert!(
                error.to_string().contains("is reserved"),
                "unexpected error: {error:#}"
            );
        }
    }

    #[test]
    fn call_arguments_accept_and_roundtrip_typed_expressions() {
        let src = "autonomous(batch(python^(6 * 7)_python, python^(7 * 8)_python))";
        let backends = make_backends(&["python"]);
        let nodes = Parser::new(src, &backends).parse().unwrap();
        let ONode::Call { args, .. } = &nodes[0] else {
            panic!("expected outer autonomous call")
        };
        let ONode::Call {
            fn_name,
            args: members,
        } = &args[0]
        else {
            panic!("expected nested batch call")
        };
        assert_eq!(fn_name, "batch");
        assert!(members
            .iter()
            .all(|member| matches!(member, ONode::TypedExpr { lang, .. } if lang == "python")));
        assert_eq!(reconstruct_source(&nodes), src);
    }

    #[test]
    fn coordination_calls_accept_generated_section_comments() {
        let src = "autonomous(batch(\n# section one\npython[*]^(1)_python[*],\n# section two\npython[*]^(2)_python[*]\n))";
        let backends = make_backends(&["python"]);
        let nodes = Parser::new(src, &backends).parse().unwrap();
        let ONode::Call { args, .. } = &nodes[0] else {
            panic!("expected autonomous call")
        };
        let ONode::Call { args: members, .. } = &args[0] else {
            panic!("expected batch call")
        };
        assert_eq!(members.len(), 2);
    }

    #[test]
    fn parses_and_normalizes_backend_authority_attributes() {
        let src = "python{ defer, cap=runner, process, fs_read }^(1)_python{ defer, cap=runner, process, fs_read }";
        let backends = make_backends(&["python"]);
        let nodes = Parser::new(src, &backends).parse().unwrap();
        let ONode::TypedExpr { attr, .. } = &nodes[0] else {
            panic!("expected typed expression")
        };
        assert_eq!(attr.as_deref(), Some("defer,cap=runner,process,fs_read"));
        assert_eq!(
            reconstruct_source(&nodes),
            "python{defer,cap=runner,process,fs_read}^(1)_python{defer,cap=runner,process,fs_read}"
        );
    }

    #[test]
    fn effect_attributes_accept_resource_values_and_roundtrip_canonically() {
        let backends = make_backends(&["python"]);

        for purity in ["pure", "unknown"] {
            let source_attr = format!(
                " effects={purity}, reads=project:src_2/../data-file.1+host:/etc/hosts;network:https://api.example.com/v1, writes=env:PATH+service:db-main, serial=host "
            );
            let canonical_attr = format!(
                "effects={purity},reads=project:src_2/../data-file.1+host:/etc/hosts;network:https://api.example.com/v1,writes=env:PATH+service:db-main,serial=host"
            );
            let source = format!("python{{{source_attr}}}^(1)_python{{{source_attr}}}");

            let nodes = Parser::new(&source, &backends).parse().unwrap();
            let ONode::TypedExpr { attr, .. } = &nodes[0] else {
                panic!("expected typed expression")
            };
            assert_eq!(attr.as_deref(), Some(canonical_attr.as_str()));

            let canonical_source =
                format!("python{{{canonical_attr}}}^(1)_python{{{canonical_attr}}}");
            assert_eq!(reconstruct_source(&nodes), canonical_source);

            let reparsed = Parser::new(&canonical_source, &backends).parse().unwrap();
            assert_eq!(reparsed, nodes);
        }
    }

    #[test]
    fn malformed_attribute_assignments_on_actual_openers_are_rejected() {
        let backends = make_backends(&["python"]);
        for attr in [
            "",
            " ",
            "effects=",
            "=pure",
            "effects==pure",
            "effects=pure=unknown",
            ",effects=pure",
            "effects=pure,",
            "effects=pure,,serial=host",
        ] {
            let source = format!("python{{{attr}}}^(1)_python{{{attr}}}");
            let error = Parser::new(&source, &backends)
                .parse()
                .unwrap_err()
                .to_string();
            assert!(
                error.contains("Empty block attribute")
                    || error.contains("Malformed block attribute"),
                "unexpected error for attribute {attr:?}: {error}"
            );
        }
    }

    #[test]
    fn attribute_like_text_without_an_opener_remains_raw() {
        let source = "python{reads=host:/tmp/state-file.1+env:PATH}";
        let backends = make_backends(&["python"]);
        let nodes = Parser::new(source, &backends).parse().unwrap();
        assert_eq!(nodes, vec![ONode::RawText(source.into())]);
    }

    #[test]
    fn alias_tags_are_canonicalized() {
        // `py^(...)_py` parses and the AST carries the canonical name.
        let backends = make_backends(&["py", "md", "plain", "o"]);
        for (src, canonical) in [
            ("py^(6 * 7)_py", "python"),
            ("md^(# hi)_md", "markdown"),
            ("plain^(hi)_plain", "text"),
            ("o^(x)_o", "O"),
        ] {
            let nodes = Parser::new(src, &backends).parse().unwrap();
            match &nodes[0] {
                ONode::TypedExpr { lang, .. } => assert_eq!(lang, canonical),
                other => panic!("expected TypedExpr for {src}, got {other:?}"),
            }
        }
    }

    #[test]
    fn reconstruct_roundtrips_var_ref() {
        // VarRef is only parsed at sequencing-lang or top level
        let src = "$answer";
        let backends = make_backends(&["python"]);
        let nodes = Parser::new(src, &backends).parse().unwrap();
        assert_eq!(reconstruct_source(&nodes), src);
    }

    #[test]
    fn registered_name_followed_by_ordinary_braces_remains_raw_text() {
        let src = "O{'not an opener'}";
        let backends = make_backends(&["O"]);
        let nodes = Parser::new(src, &backends).parse().unwrap();
        assert_eq!(nodes, vec![ONode::RawText(src.into())]);
    }

    #[test]
    fn malformed_attribute_on_an_actual_opener_is_an_error() {
        let src = "python{cap=,network}^(1)_python{cap=,network}";
        let backends = make_backends(&["python"]);
        let error = Parser::new(src, &backends).parse().unwrap_err().to_string();
        assert!(error.contains("Malformed block attribute"));
    }

    #[test]
    fn incomplete_raw_attribute_does_not_scan_into_the_next_line() {
        let src = concat!(
            "python[0]^(\n",
            ")_python{\n",
            ")_python[0]\n",
            "cpp[0]{cap=backend}^(\n",
            ")_cpp[0]{cap=backend}\n",
        );
        let backends = make_backends(&["python", "cpp"]);
        let nodes = Parser::new(src, &backends).parse().unwrap();
        assert_eq!(
            nodes
                .iter()
                .filter(|node| matches!(node, ONode::TypedExpr { .. }))
                .count(),
            2
        );
    }

    #[test]
    fn backslash_escapes_opener_as_literal_text() {
        // \python^( inside a python[0] body should be treated as literal text,
        // NOT as a nested expression. The outer closer is )_python[0], so
        // )_python (no env) inside the escaped string doesn't close the block.
        let src = r#"python[0]^(src = "\python^(1)_python")_python[0]"#;
        let backends = make_backends(&["python"]);
        let nodes = Parser::new(src, &backends).parse().unwrap();
        // The outer python[0] block should be a single TypedExpr.
        assert_eq!(nodes.len(), 1);
        if let ONode::TypedExpr { body, .. } = &nodes[0] {
            // Body should be raw text — the backslash was consumed and
            // `python^(` emitted as literal text. The inner `1)_python`
            // is also raw text because `)_python` ≠ outer closer `)_python[0]`.
            let combined: String = body
                .iter()
                .map(|n| match n {
                    ONode::RawText(s) => s.clone(),
                    _ => "<node>".to_string(),
                })
                .collect();
            assert!(
                combined.contains("python^(1)_python"),
                "body should contain literal python^(: {:?}",
                combined
            );
        } else {
            panic!("expected TypedExpr");
        }
    }

    #[test]
    fn backslash_escapes_dollar_as_literal_text() {
        // \$PATH inside a bash block should emit the literal text "$PATH",
        // NOT parse as a VarRef (which would fail with "Undefined variable: $PATH").
        // This is essential for writing real shell code in bash^(...)_bash blocks.
        let src = r#"bash^(echo \$PATH)_bash"#;
        let backends = make_backends(&["bash"]);
        let nodes = Parser::new(src, &backends).parse().unwrap();
        assert_eq!(nodes.len(), 1);
        if let ONode::TypedExpr { body, .. } = &nodes[0] {
            let combined: String = body
                .iter()
                .map(|n| match n {
                    ONode::RawText(s) => s.clone(),
                    ONode::VarRef(n) => format!("<VarRef:{n}>"),
                    _ => "<node>".to_string(),
                })
                .collect();
            assert!(
                combined.contains("$PATH") && !combined.contains("<VarRef:PATH>"),
                "bash body should contain literal $PATH, not a VarRef: {:?}",
                combined
            );
        } else {
            panic!("expected TypedExpr");
        }
    }

    #[test]
    fn backslash_escapes_dollar_paren_as_literal_text() {
        // \$((...)) inside a bash block should emit literal shell arithmetic
        // syntax, not leave the backslash in place or try to parse an O splice.
        let src = r#"bash^(echo \$((1 + 2)))_bash"#;
        let backends = make_backends(&["bash"]);
        let nodes = Parser::new(src, &backends).parse().unwrap();
        assert_eq!(nodes.len(), 1);
        if let ONode::TypedExpr { body, .. } = &nodes[0] {
            let combined: String = body
                .iter()
                .map(|n| match n {
                    ONode::RawText(s) => s.clone(),
                    _ => "<node>".to_string(),
                })
                .collect();
            assert!(
                combined.contains("$((1 + 2))"),
                "bash body should contain literal $((...)) arithmetic syntax: {:?}",
                combined
            );
        } else {
            panic!("expected TypedExpr");
        }
    }

    #[test]
    fn dollar_without_backslash_is_still_a_varref() {
        // $answer at the top level (unescaped) must still parse as a VarRef.
        let src = "$answer";
        let backends = make_backends(&["python"]);
        let nodes = Parser::new(src, &backends).parse().unwrap();
        assert!(
            nodes
                .iter()
                .any(|n| matches!(n, ONode::VarRef(name) if name == "answer")),
            "unescaped $answer should still be a VarRef: {:?}",
            nodes
        );
    }

    #[test]
    fn comments_are_skipped_at_top_level() {
        // A `#` line at the top level must not be parsed as code.
        let src = "# activate() is just a comment\nlet x = instantiate($e)";
        let backends = make_backends(&["python"]);
        let nodes = Parser::new(src, &backends).parse().unwrap();
        // Only the let binding should be present, not a Call for activate().
        assert_eq!(nodes.len(), 1);
        assert!(matches!(&nodes[0], ONode::LetBinding { name, .. } if name == "x"));
    }

    #[test]
    fn inline_comment_after_let_binding() {
        // Text after `#` on the same line as a let binding should be ignored.
        let src = "let x = instantiate($e)  # this is a comment with realise($x)";
        let backends = make_backends(&["python"]);
        let nodes = Parser::new(src, &backends).parse().unwrap();
        // The let binding is present; the comment is stripped (whitespace
        // between `)` and `#` may produce a RawText node — that's fine).
        assert!(nodes
            .iter()
            .any(|n| matches!(n, ONode::LetBinding { name, .. } if name == "x")));
        // No Call node for realise should exist.
        assert!(!nodes
            .iter()
            .any(|n| matches!(n, ONode::Call { fn_name, .. } if fn_name == "realise")));
    }

    #[test]
    fn hash_inside_non_sequencing_body_is_not_a_comment() {
        // Inside a markdown body, `#` is a heading, not a comment.
        let src = "markdown^(# Heading\nsome text)_markdown";
        let backends = make_backends(&["markdown"]);
        let nodes = Parser::new(src, &backends).parse().unwrap();
        assert_eq!(nodes.len(), 1);
        if let ONode::TypedExpr { body, .. } = &nodes[0] {
            let combined: String = body
                .iter()
                .map(|n| match n {
                    ONode::RawText(s) => s.clone(),
                    _ => "<node>".to_string(),
                })
                .collect();
            assert!(
                combined.contains("# Heading"),
                "markdown body should keep #: {:?}",
                combined
            );
        } else {
            panic!("expected TypedExpr");
        }
    }

    #[test]
    fn let_syntax_inside_non_sequencing_body_remains_backend_source() {
        let src = concat!(
            "markdown^(Swift package example:\n",
            "let package = Package(\n",
            "    name: \"MyLlamaPackage\"\n",
            "))_markdown"
        );
        let backends = make_backends(&["markdown"]);
        let nodes = Parser::new(src, &backends).parse().unwrap();
        assert_eq!(nodes.len(), 1);
        let ONode::TypedExpr { body, .. } = &nodes[0] else {
            panic!("expected TypedExpr")
        };
        assert_eq!(
            body,
            &[ONode::RawText(
                "Swift package example:\nlet package = Package(\n    name: \"MyLlamaPackage\"\n)"
                    .into()
            )]
        );
    }

    #[test]
    fn comment_with_call_syntax_is_ignored() {
        // Reproduces the bug: `activate()` in a comment must not produce a Call.
        let src = "# with activate() chains.\nlet here = current_system()";
        let backends = make_backends(&["python"]);
        let nodes = Parser::new(src, &backends).parse().unwrap();
        // Only the let binding; the comment line is stripped.
        assert_eq!(nodes.len(), 1);
        assert!(matches!(&nodes[0], ONode::LetBinding { name, .. } if name == "here"));
    }
}
