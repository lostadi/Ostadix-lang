use anyhow::{bail, Result};
use std::collections::HashSet;

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
        body: Vec<ONode>,
    },
}

#[derive(Debug, Clone)]
struct Tag {
    lang: String,
    env_id: u32,
    raw: String,
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
                    body,
                });

                text_start = self.pos;
                continue;
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

        let tag = match self.try_parse_opener()? {
            Some(tag) => tag,
            None => {
                bail!(
                    "Line {}: let binding `{}` must be assigned a typed expression",
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
                body,
            }),
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

        if i + 2 <= bytes.len() && &self.source[i..i + 2] == "^(" {
            self.pos = i + 2;
            Ok(Some(Tag { lang, env_id, raw }))
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
