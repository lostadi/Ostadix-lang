use anyhow::{bail, Result};
use std::collections::HashSet;

/// Languages whose bodies are SEQUENCED (children are O-level statements)
/// rather than SPLICED (children are raw source text for a target backend).
///
/// Inside a sequencing lang's body, the parser produces ONode::Call for
/// `name(...)` syntax and resolves VarRefs, LetBindings, and nested
/// TypedExprs as structured ONodes rather than raw text destined for a
/// foreign backend.
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
        lang:   String,
        env_id: u32,
        /// STEP-3.5: optional attribute parsed from `{ident}` on the tag.
        /// `None` for plain `lang^(...)_lang`; `Some("lazy")` for
        /// `lang{lazy}^(...)_lang{lazy}`; `Some("defer")` for `{defer}`.
        /// The evaluator dispatches on this when present.
        attr:   Option<String>,
        body:   Vec<ONode>,
    },

    /// A function call: `name(arg1, arg2, ...)`.
    ///
    /// Introduced in STEP 2 as the surface syntax for the rung-climb operators
    /// `instantiate(expr)`, `realise(drv)`, and the explicit performer `now(req)`.
    /// Each arg is itself an ONode — args can be VarRef, nested Call, or a
    /// TypedExpr (the latter only at let-binding RHS today).
    ///
    /// Parsed at two positions for step 2:
    ///   1. The RHS of a let-binding:                  `let drv = instantiate($expr)`
    ///   2. As a top-level statement:                  `realise($drv)`
    /// Calls are NOT parsed inside typed expression bodies (the body is raw
    /// source text for the receiving backend; embedding O-level calls there
    /// would be ambiguous). STEP3 may lift this.
    Call {
        fn_name: String,
        args:    Vec<ONode>,
    },
}

#[derive(Debug, Clone)]
struct Tag {
    lang:   String,
    env_id: u32,
    /// STEP-3.5: optional `{ident}` attribute on the language tag.
    /// e.g. `python{lazy}^(...)_python{lazy}` → attr = Some("lazy").
    /// Single attribute for now; multi-attribute `{a,b}` is a later parser
    /// change. The attribute travels with the tag through evaluation.
    attr:   Option<String>,
    /// The raw text of the tag — used to construct the closer match string.
    /// Includes the lang, the optional `[N]` env, and the optional `{attr}`,
    /// in source order.
    raw:    String,
}

pub struct Parser<'a> {
    source: &'a str,
    pos: usize,
    line: usize,
    registered_backends: &'a HashSet<String>,
}

impl<'a> Parser<'a> {
    pub fn new(source: &'a str, registered_backends: &'a HashSet<String>) -> Self {
        Self {
            source,
            pos: 0,
            line: 1,
            registered_backends,
        }
    }

    pub fn parse(&mut self) -> Result<Vec<ONode>> {
        self.parse_until(None)
    }

    fn parse_until(&mut self, expected_closer: Option<&Tag>) -> Result<Vec<ONode>> {
        let mut nodes = Vec::new();
        let mut text_start = self.pos;

        while self.pos < self.source.len() {
            if let Some(tag) = expected_closer {
                let closer = format!(")_{}", tag.raw);
                if self.starts_with(&closer) {
                    self.flush_text(&mut nodes, text_start, self.pos);
                    self.advance_bytes(closer.len());
                    return Ok(nodes);
                }
            }

            if self.starts_with_let_keyword() {
                let let_start = self.pos;
                if let Some(binding) = self.try_parse_let_binding()? {
                    self.flush_text(&mut nodes, text_start, let_start);
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
                        } else {
                            nodes.push(ONode::RawText(literal));
                        }
                        text_start = self.pos;
                        true
                    } else {
                        self.pos = temp_pos;
                        false
                    };
                    if had_opener { continue; }

                    // Check if the matching closer follows the backslash.
                    if let Some(tag) = expected_closer {
                        let closer = format!(")_{}", tag.raw);
                        if self.source[after_bs..].starts_with(&closer) {
                            self.flush_text(&mut nodes, text_start, self.pos);
                            self.pos = after_bs + closer.len();
                            if let Some(ONode::RawText(s)) = nodes.last_mut() {
                                s.push_str(&closer);
                            } else {
                                nodes.push(ONode::RawText(closer));
                            }
                            text_start = self.pos;
                            continue;
                        }
                    }
                }
            }

            if self.current_byte() == Some(b'$') {
                if let Some(name) = self.try_parse_var_ref()? {
                    self.flush_text(&mut nodes, text_start, self.pos_before_var(&name));
                    nodes.push(ONode::VarRef(name));
                    text_start = self.pos;
                    continue;
                }
            }

            if let Some(tag) = self.try_parse_opener()? {
                let opener_start = self.last_opener_start(tag.raw.len());
                self.flush_text(&mut nodes, text_start, opener_start);

                let body = self.parse_until(Some(&tag))?;
                nodes.push(ONode::TypedExpr {
                    lang: tag.lang,
                    env_id: tag.env_id,
                    attr: tag.attr,
                    body,
                });

                text_start = self.pos;
                continue;
            }

            // STEP-2/3: try to parse a call like `instantiate($x)` or
            // `realise(instantiate($x))`. Allowed at the document top level
            // AND inside the bodies of SEQUENCING_LANGS (lazy^, eventually O^
            // and quote^). Disallowed inside ordinary typed-expr bodies so
            // that source text destined for a backend isn't reinterpreted.
            let inside_sequencing = expected_closer
                .map(|t| SEQUENCING_LANGS.contains(&t.lang.as_str()))
                .unwrap_or(true);

            // O-lang line comment: `#` to end of line. Recognised only in
            // sequencing contexts (top level and inside `O^`/`quote^`) so that
            // `#` inside e.g. `python^(# Python comment)_python` passes
            // through verbatim as part of the backend source.
            if inside_sequencing && self.current_byte() == Some(b'#') {
                self.flush_text(&mut nodes, text_start, self.pos);
                // Skip to end of line (the `\n` is left in place so the
                // line counter advances normally on the next iteration).
                while self.pos < self.source.len() && self.current_byte() != Some(b'\n') {
                    self.pos += 1;
                }
                text_start = self.pos;
                continue;
            }

            if inside_sequencing {
                let stmt_start = self.pos;
                if let Some(call) = self.try_parse_call()? {
                    self.flush_text(&mut nodes, text_start, stmt_start);
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
        self.skip_whitespace();

        // STEP-2: a let RHS may now be a Call (instantiate(...), realise(...))
        // in addition to a typed expression. Try Call first; on miss, fall
        // through to the typed-expression path.
        if let Some(call) = self.try_parse_call()? {
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

        let body = self.parse_until(Some(&tag))?;

        Ok(Some(ONode::LetBinding {
            name,
            expr: Box::new(ONode::TypedExpr {
                lang: tag.lang,
                env_id: tag.env_id,
                attr: tag.attr,
                body,
            }),
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
    /// Arguments are themselves ONodes — VarRef (`$name`), a string literal
    /// (`"..."`, including `\"` and `\\` escapes), or a nested Call.
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
        if self.registered_backends.contains(&name)
            || self.current_byte() != Some(b'(')
        {
            self.pos = original_pos;
            self.line = original_line;
            return Ok(None);
        }

        // Commit: from here on, errors are real errors.
        self.advance_one_byte(); // consume '('
        self.skip_whitespace();

        let mut args = Vec::new();
        loop {
            if self.current_byte() == Some(b')') {
                self.advance_one_byte();
                break;
            }

            // Each arg is a VarRef ($name), a string literal ("..."), or a nested Call.
            let arg = if self.current_byte() == Some(b'$') {
                let var = self.try_parse_var_ref()?
                    .ok_or_else(|| anyhow::anyhow!(
                        "Line {}: expected variable reference after $", self.line
                    ))?;
                ONode::VarRef(var)
            } else if self.current_byte() == Some(b'"') {
                // String literal: "..." with \" and \\ escape support.
                let s = self.parse_string_literal().map_err(|e| anyhow::anyhow!(
                    "Line {}: {}", self.line, e
                ))?;
                ONode::RawText(s)
            } else if let Some(nested) = self.try_parse_call()? {
                nested
            } else {
                bail!(
                    "Line {}: in call `{}(...)`, expected $var, \"string\", or nested call",
                    self.line, name
                );
            };
            args.push(arg);

            self.skip_whitespace();
            match self.current_byte() {
                Some(b',') => { self.advance_one_byte(); self.skip_whitespace(); }
                Some(b')') => { self.advance_one_byte(); break; }
                _ => bail!(
                    "Line {}: in call `{}(...)`, expected ',' or ')'",
                    self.line, name
                ),
            }
        }

        Ok(Some(ONode::Call { fn_name: name, args }))
    }

    /// Parse a double-quoted string literal at the current position.
    ///
    /// Supports `\"` (literal double-quote) and `\\` (literal backslash) as
    /// the only escape sequences. All other characters (including multi-byte
    /// UTF-8) are taken verbatim.
    /// Advances `self.pos` past the closing `"`.
    fn parse_string_literal(&mut self) -> Result<String> {
        // Expect opening '"'
        if self.current_byte() != Some(b'"') {
            bail!("expected '\"' to start string literal");
        }
        self.advance_one_byte(); // consume opening '"'

        let mut result = String::new();
        loop {
            // Read the next Unicode character from the current position.
            let ch = match self.source[self.pos..].chars().next() {
                Some(c) => c,
                None => bail!("unterminated string literal"),
            };

            match ch {
                '"' => {
                    self.advance_one_byte(); // consume closing '"'
                    break;
                }
                '\\' => {
                    self.advance_one_byte(); // consume '\'
                    let esc = match self.source[self.pos..].chars().next() {
                        Some(c) => c,
                        None => bail!("unterminated string literal after '\\'"),
                    };
                    match esc {
                        '"'  => { result.push('"');  self.advance_one_byte(); }
                        '\\' => { result.push('\\'); self.advance_one_byte(); }
                        'n'  => { result.push('\n'); self.advance_one_byte(); }
                        't'  => { result.push('\t'); self.advance_one_byte(); }
                        other => {
                            // Unknown escape: keep the backslash and the character.
                            result.push('\\');
                            result.push(other);
                            self.pos += other.len_utf8();
                            if other == '\n' { self.line += 1; }
                        }
                    }
                }
                other => {
                    result.push(other);
                    // advance_one_byte handles UTF-8 width and line counting.
                    self.pos += other.len_utf8();
                    if other == '\n' { self.line += 1; }
                }
            }
        }
        Ok(result)
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

        if !self.registered_backends.contains(&lang) {
            return Ok(None);
        }

        let mut env_id = u32::MAX;
        let mut raw = lang.clone();

        if i < bytes.len() && bytes[i] == b'[' {
            let env_start = i;
            i += 1;

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
            env_id = digits.parse::<u32>()?;
            i += 1;

            raw.push_str(&self.source[env_start..i]);
        }

        // STEP-3.5: optional `{attr}` after the env slot, before `^(`.
        // Parses a single identifier in braces. Multi-attribute syntax
        // `{a,b,c}` is left for a later expansion if it becomes useful.
        let mut attr: Option<String> = None;
        if i < bytes.len() && bytes[i] == b'{' {
            let attr_start = i;
            i += 1;

            let ident_start = i;
            while i < bytes.len() && is_ident_continue(bytes[i]) {
                i += 1;
            }
            if ident_start == i {
                return Ok(None);   // empty {}
            }
            if i >= bytes.len() || bytes[i] != b'}' {
                return Ok(None);
            }

            attr = Some(self.source[ident_start..i].to_string());
            i += 1;   // past '}'

            raw.push_str(&self.source[attr_start..i]);
        }

        if i + 2 <= bytes.len() && &self.source[i..i + 2] == "^(" {
            self.pos = i + 2;
            Ok(Some(Tag { lang, env_id, attr, raw }))
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

    fn flush_text(&self, nodes: &mut Vec<ONode>, start: usize, end: usize) {
        if end > start {
            nodes.push(ONode::RawText(self.source[start..end].to_string()));
        }
    }

    fn starts_with(&self, pat: &str) -> bool {
        self.source[self.pos..].starts_with(pat)
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

    fn last_opener_start(&self, raw_len: usize) -> usize {
        self.pos - raw_len - 2
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

        ONode::TypedExpr { lang, env_id, attr, body } => {
            // opener: lang[N]?{attr}?^(
            buf.push_str(lang);
            if *env_id != u32::MAX {
                buf.push('[');
                buf.push_str(&env_id.to_string());
                buf.push(']');
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
            if *env_id != u32::MAX {
                buf.push('[');
                buf.push_str(&env_id.to_string());
                buf.push(']');
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
    use super::*;

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
    fn reconstruct_roundtrips_typed_expr() {
        let src = "python^(6 * 7)_python";
        let backends = make_backends(&["python"]);
        let nodes = Parser::new(src, &backends).parse().unwrap();
        assert_eq!(reconstruct_source(&nodes), src);
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
            let combined: String = body.iter().map(|n| match n {
                ONode::RawText(s) => s.clone(),
                _ => "<node>".to_string(),
            }).collect();
            assert!(combined.contains("python^(1)_python"),
                "body should contain literal python^(: {:?}", combined);
        } else {
            panic!("expected TypedExpr");
        }
    }
}
