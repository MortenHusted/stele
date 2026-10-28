use std::collections::{BTreeMap, BTreeSet};

use super::{
    Artifact, ArtifactError, EvidenceBasis, EvidenceKind, ExpectationDecl, FactDecl, FactValue,
    MutationDecl, MutationOperator, RelationDecl, RuleDecl, SymbolDecl, ValueType,
};

#[derive(Debug, Clone, PartialEq, Eq)]
enum TokenKind {
    Identifier(String),
    String(String),
    Integer(i64),
    LeftBrace,
    RightBrace,
    LeftBracket,
    RightBracket,
    Colon,
    Comma,
    Eof,
}

#[derive(Debug, Clone)]
struct Token {
    kind: TokenKind,
    offset: usize,
}

/// One source comment, kept so declarations can carry their author's prose.
#[derive(Debug, Clone)]
struct Comment {
    text: String,
    start: usize,
    end: usize,
}

struct Lexer<'a> {
    source: &'a str,
    bytes: &'a [u8],
    offset: usize,
    comments: Vec<Comment>,
}

impl<'a> Lexer<'a> {
    fn new(source: &'a str) -> Self {
        Self {
            source,
            bytes: source.as_bytes(),
            offset: 0,
            comments: Vec::new(),
        }
    }

    fn tokenize(mut self) -> Result<(Vec<Token>, Vec<Comment>), ArtifactError> {
        let mut tokens = Vec::new();
        loop {
            self.skip_trivia()?;
            if self.offset >= self.bytes.len() {
                tokens.push(Token {
                    kind: TokenKind::Eof,
                    offset: self.offset,
                });
                return Ok((tokens, self.comments));
            }
            let offset = self.offset;
            let kind = match self.bytes[self.offset] {
                b'{' => {
                    self.offset += 1;
                    TokenKind::LeftBrace
                }
                b'}' => {
                    self.offset += 1;
                    TokenKind::RightBrace
                }
                b'[' => {
                    self.offset += 1;
                    TokenKind::LeftBracket
                }
                b']' => {
                    self.offset += 1;
                    TokenKind::RightBracket
                }
                b':' => {
                    self.offset += 1;
                    TokenKind::Colon
                }
                b',' => {
                    self.offset += 1;
                    TokenKind::Comma
                }
                b'"' => TokenKind::String(self.string()?),
                b'-' if self.peek(1).is_some_and(|byte| byte.is_ascii_digit()) => {
                    TokenKind::Integer(self.integer()?)
                }
                byte if byte.is_ascii_digit() => TokenKind::Integer(self.integer()?),
                byte if is_identifier_start(byte) => TokenKind::Identifier(self.identifier()),
                byte => {
                    return Err(self.error(
                        offset,
                        format!("unexpected character `{}`", char::from(byte)),
                    ));
                }
            };
            tokens.push(Token { kind, offset });
        }
    }

    fn skip_trivia(&mut self) -> Result<(), ArtifactError> {
        loop {
            while self
                .bytes
                .get(self.offset)
                .is_some_and(|byte| byte.is_ascii_whitespace())
            {
                self.offset += 1;
            }
            if self.bytes.get(self.offset..self.offset + 2) == Some(b"//") {
                let start = self.offset;
                self.offset += 2;
                let text_start = self.offset;
                while self
                    .bytes
                    .get(self.offset)
                    .is_some_and(|byte| *byte != b'\n')
                {
                    self.offset += 1;
                }
                self.comment(start, text_start, self.offset, self.offset);
                continue;
            }
            if self.bytes.get(self.offset) == Some(&b'#') {
                let start = self.offset;
                self.offset += 1;
                let text_start = self.offset;
                while self
                    .bytes
                    .get(self.offset)
                    .is_some_and(|byte| *byte != b'\n')
                {
                    self.offset += 1;
                }
                self.comment(start, text_start, self.offset, self.offset);
                continue;
            }
            if self.bytes.get(self.offset..self.offset + 2) == Some(b"/*") {
                let start = self.offset;
                self.offset += 2;
                let text_start = self.offset;
                while self.bytes.get(self.offset..self.offset + 2) != Some(b"*/") {
                    if self.offset >= self.bytes.len() {
                        return Err(self.error(start, "unterminated block comment"));
                    }
                    self.offset += 1;
                }
                let text_end = self.offset;
                self.offset += 2;
                self.comment(start, text_start, text_end, self.offset);
                continue;
            }
            return Ok(());
        }
    }

    fn comment(&mut self, start: usize, text_start: usize, text_end: usize, end: usize) {
        let text = self.source[text_start..text_end]
            .lines()
            .map(|line| {
                let line = line.trim();
                line.strip_prefix('*').map_or(line, str::trim_start)
            })
            .collect::<Vec<_>>()
            .join("\n");
        self.comments.push(Comment {
            text: text.trim().to_string(),
            start,
            end,
        });
    }

    fn identifier(&mut self) -> String {
        let start = self.offset;
        self.offset += 1;
        while self
            .bytes
            .get(self.offset)
            .is_some_and(|byte| is_identifier_continue(*byte))
        {
            self.offset += 1;
        }
        self.source[start..self.offset].to_string()
    }

    fn integer(&mut self) -> Result<i64, ArtifactError> {
        let start = self.offset;
        if self.bytes[self.offset] == b'-' {
            self.offset += 1;
        }
        while self
            .bytes
            .get(self.offset)
            .is_some_and(|byte| byte.is_ascii_digit())
        {
            self.offset += 1;
        }
        self.source[start..self.offset]
            .parse()
            .map_err(|_| self.error(start, "integer is outside the i64 range"))
    }

    fn string(&mut self) -> Result<String, ArtifactError> {
        let start = self.offset;
        self.offset += 1;
        let mut value = String::new();
        while let Some(byte) = self.bytes.get(self.offset).copied() {
            match byte {
                b'"' => {
                    self.offset += 1;
                    return Ok(value);
                }
                b'\\' => {
                    self.offset += 1;
                    let Some(escaped) = self.bytes.get(self.offset).copied() else {
                        return Err(self.error(start, "unterminated string"));
                    };
                    self.offset += 1;
                    value.push(match escaped {
                        b'n' => '\n',
                        b'r' => '\r',
                        b't' => '\t',
                        b'"' => '"',
                        b'\\' => '\\',
                        _ => {
                            return Err(self.error(
                                self.offset - 1,
                                format!("unsupported escape `\\{}`", char::from(escaped)),
                            ));
                        }
                    });
                }
                _ if byte.is_ascii() => {
                    value.push(char::from(byte));
                    self.offset += 1;
                }
                _ => {
                    let tail = &self.source[self.offset..];
                    let character = tail.chars().next().expect("valid UTF-8 source");
                    value.push(character);
                    self.offset += character.len_utf8();
                }
            }
        }
        Err(self.error(start, "unterminated string"))
    }

    fn peek(&self, distance: usize) -> Option<u8> {
        self.bytes.get(self.offset + distance).copied()
    }

    fn error(&self, offset: usize, message: impl Into<String>) -> ArtifactError {
        let (line, column) = line_column(self.source, offset);
        ArtifactError::new(format!("{line}:{column}: {}", message.into()))
    }
}

fn is_identifier_start(byte: u8) -> bool {
    byte.is_ascii_alphabetic() || byte == b'_'
}

fn is_identifier_continue(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
}

/// A ruled divider such as `// ------ policies` is a section heading even
/// when it sits directly above a declaration.
fn is_heading(text: &str) -> bool {
    let first = text.lines().next().unwrap_or_default().trim_start();
    first.chars().next().is_some_and(|rule| {
        "-=#*~".contains(rule) && first.starts_with(&rule.to_string().repeat(3))
    })
}

/// Whitespace spanning at most one line break: the gap between a comment
/// and the declaration it documents.
fn touches(gap: &str) -> bool {
    gap.chars().all(char::is_whitespace) && gap.matches('\n').count() <= 1
}

fn line_column(source: &str, offset: usize) -> (usize, usize) {
    let prefix = &source[..offset.min(source.len())];
    let line = prefix.bytes().filter(|byte| *byte == b'\n').count() + 1;
    let column = prefix
        .rsplit_once('\n')
        .map_or(prefix.chars().count() + 1, |(_, tail)| {
            tail.chars().count() + 1
        });
    (line, column)
}

#[derive(Debug, Clone)]
enum RawValue {
    Identifier(String),
    String(String),
    Integer(i64),
    List(Vec<RawValue>),
    Map(Vec<(String, RawValue)>),
}

#[derive(Debug)]
struct RawBlock {
    kind: String,
    name: String,
    fields: BTreeMap<String, RawValue>,
    doc: Option<String>,
    start: usize,
    end: usize,
}

struct ParsedSpec {
    name: String,
    doc: Option<String>,
    blocks: Vec<RawBlock>,
}

struct Parser<'a> {
    source: &'a str,
    tokens: Vec<Token>,
    comments: Vec<Comment>,
    cursor: usize,
}

impl<'a> Parser<'a> {
    fn new(source: &'a str) -> Result<Self, ArtifactError> {
        let (tokens, comments) = Lexer::new(source).tokenize()?;
        Ok(Self {
            source,
            tokens,
            comments,
            cursor: 0,
        })
    }

    fn parse(mut self) -> Result<ParsedSpec, ArtifactError> {
        let spec_offset = self.current().offset;
        self.expect_keyword("spec")?;
        let name = self.expect_identifier("spec name")?;
        self.expect(TokenKind::LeftBrace, "`{`")?;
        let mut blocks = Vec::new();
        while !self.at(&TokenKind::RightBrace) {
            if self.at(&TokenKind::Eof) {
                return Err(self.error("expected `}` to close spec"));
            }
            blocks.push(self.block()?);
        }
        self.advance();
        self.expect(TokenKind::Eof, "end of file")?;
        let doc = self.assign_docs(spec_offset, &mut blocks);
        Ok(ParsedSpec { name, doc, blocks })
    }

    /// Attach comments to declarations. Everything before `spec` documents
    /// the artifact. A comment block touching the line above a declaration
    /// documents that declaration, as does a comment trailing its closing
    /// brace. Comments separated by a blank line are section headings and
    /// belong to nobody.
    fn assign_docs(&self, spec_offset: usize, blocks: &mut [RawBlock]) -> Option<String> {
        let mut used = vec![false; self.comments.len()];
        let preamble: Vec<usize> = (0..self.comments.len())
            .filter(|index| self.comments[*index].end <= spec_offset)
            .collect();
        preamble.iter().for_each(|index| used[*index] = true);
        let spec_doc = self.join_comments(&preamble);

        let mut trailing = vec![None; blocks.len()];
        for (block_index, block) in blocks.iter().enumerate() {
            let candidate = self.comments.iter().position(|comment| {
                comment.start > block.end && !self.source[block.end..comment.start].contains('\n')
            });
            if let Some(index) =
                candidate.filter(|index| !used[*index] && !is_heading(&self.comments[*index].text))
            {
                used[index] = true;
                trailing[block_index] = Some(index);
            }
        }

        for (block_index, block) in blocks.iter_mut().enumerate() {
            let mut leading = Vec::new();
            let mut boundary = block.start;
            for index in (0..self.comments.len()).rev() {
                let comment = &self.comments[index];
                if used[index] || comment.end > boundary {
                    continue;
                }
                if !touches(&self.source[comment.end..boundary]) || is_heading(&comment.text) {
                    break;
                }
                leading.push(index);
                boundary = comment.start;
            }
            leading.reverse();
            leading.iter().for_each(|index| used[*index] = true);
            let mut parts = Vec::new();
            parts.extend(self.join_comments(&leading));
            parts.extend(trailing[block_index].map(|index| self.comments[index].text.clone()));
            let doc = parts.join("\n");
            block.doc = (!doc.trim().is_empty()).then(|| doc.trim().to_string());
        }
        spec_doc
    }

    fn join_comments(&self, indices: &[usize]) -> Option<String> {
        let mut text = String::new();
        for (position, index) in indices.iter().enumerate() {
            if position > 0 {
                let previous = &self.comments[indices[position - 1]];
                let gap = &self.source[previous.end..self.comments[*index].start];
                text.push('\n');
                if gap.matches('\n').count() > 1 {
                    text.push('\n');
                }
            }
            text.push_str(&self.comments[*index].text);
        }
        let text = text.trim();
        (!text.is_empty()).then(|| text.to_string())
    }

    fn block(&mut self) -> Result<RawBlock, ArtifactError> {
        let start = self.current().offset;
        let kind = self.expect_identifier("block kind")?;
        let name = if kind == "notes" {
            "notes".to_string()
        } else if kind == "symbol" {
            raw_text(self.value()?, "symbol value")?
        } else {
            self.expect_identifier("block name")?
        };
        self.expect(TokenKind::LeftBrace, "`{`")?;
        let mut fields = BTreeMap::new();
        while !self.at(&TokenKind::RightBrace) {
            if self.at(&TokenKind::Eof) {
                return Err(self.error(format!("expected `}}` to close {kind} `{name}`")));
            }
            let field = self.expect_identifier("field name")?;
            self.expect(TokenKind::Colon, "`:`")?;
            let value = self.value()?;
            if fields.insert(field.clone(), value).is_some() {
                return Err(self.error(format!("duplicate field `{field}` in {kind} `{name}`")));
            }
            if self.at(&TokenKind::Comma) {
                self.advance();
            }
        }
        let end = self.current().offset;
        self.advance();
        Ok(RawBlock {
            kind,
            name,
            fields,
            doc: None,
            start,
            end,
        })
    }

    fn value(&mut self) -> Result<RawValue, ArtifactError> {
        match self.current().kind.clone() {
            TokenKind::Identifier(value) => {
                self.advance();
                Ok(RawValue::Identifier(value))
            }
            TokenKind::String(value) => {
                self.advance();
                Ok(RawValue::String(value))
            }
            TokenKind::Integer(value) => {
                self.advance();
                Ok(RawValue::Integer(value))
            }
            TokenKind::LeftBracket => {
                self.advance();
                let mut values = Vec::new();
                while !self.at(&TokenKind::RightBracket) {
                    if self.at(&TokenKind::LeftBracket) {
                        return Err(self.error("nested lists are not supported"));
                    }
                    values.push(self.value()?);
                    if self.at(&TokenKind::Comma) {
                        self.advance();
                        if self.at(&TokenKind::RightBracket) {
                            break;
                        }
                    } else if !self.at(&TokenKind::RightBracket) {
                        return Err(self.error("expected `,` or `]` in list"));
                    }
                }
                self.expect(TokenKind::RightBracket, "`]`")?;
                Ok(RawValue::List(values))
            }
            TokenKind::LeftBrace => {
                self.advance();
                let mut entries = Vec::new();
                let mut names = BTreeSet::new();
                while !self.at(&TokenKind::RightBrace) {
                    if self.at(&TokenKind::Eof) {
                        return Err(self.error("expected `}` to close map"));
                    }
                    let name = self.expect_identifier("map entry name")?;
                    if !names.insert(name.clone()) {
                        return Err(self.error(format!("duplicate map entry `{name}`")));
                    }
                    self.expect(TokenKind::Colon, "`:`")?;
                    entries.push((name, self.value()?));
                    if self.at(&TokenKind::Comma) {
                        self.advance();
                    }
                }
                self.advance();
                Ok(RawValue::Map(entries))
            }
            _ => Err(self.error("expected a value")),
        }
    }

    fn expect_keyword(&mut self, expected: &str) -> Result<(), ArtifactError> {
        let actual = self.expect_identifier(expected)?;
        if actual == expected {
            Ok(())
        } else if expected == "spec" && (actual == "catalog" || actual == "plan") {
            Err(self.error(format!(
                "expected `spec`, found `{actual}`: a catalog or plan is checked with `stele plan check`"
            )))
        } else {
            Err(self.error(format!("expected `{expected}`, found `{actual}`")))
        }
    }

    fn expect_identifier(&mut self, description: &str) -> Result<String, ArtifactError> {
        match self.current().kind.clone() {
            TokenKind::Identifier(value) => {
                self.advance();
                Ok(value)
            }
            _ => Err(self.error(format!("expected {description}"))),
        }
    }

    fn expect(&mut self, kind: TokenKind, label: &str) -> Result<(), ArtifactError> {
        if self.at(&kind) {
            self.advance();
            Ok(())
        } else {
            Err(self.error(format!("expected {label}")))
        }
    }

    fn at(&self, kind: &TokenKind) -> bool {
        std::mem::discriminant(&self.current().kind) == std::mem::discriminant(kind)
    }

    fn current(&self) -> &Token {
        &self.tokens[self.cursor]
    }

    fn advance(&mut self) {
        if self.cursor + 1 < self.tokens.len() {
            self.cursor += 1;
        }
    }

    fn error(&self, message: impl Into<String>) -> ArtifactError {
        let (line, column) = line_column(self.source, self.current().offset);
        ArtifactError::new(format!("{line}:{column}: {}", message.into()))
    }
}

pub fn parse_artifact(source: &str) -> Result<Artifact, ArtifactError> {
    let parsed = Parser::new(source)?.parse()?;
    Artifact::from_blocks(parsed)
}

impl Artifact {
    fn from_blocks(parsed: ParsedSpec) -> Result<Self, ArtifactError> {
        let ParsedSpec { name, doc, blocks } = parsed;
        let mut artifact = Artifact {
            name,
            doc,
            notes: None,
            symbols: Vec::new(),
            relations: Vec::new(),
            facts: Vec::new(),
            rules: Vec::new(),
            expectations: Vec::new(),
            mutations: Vec::new(),
        };
        let mut ids: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();

        for mut block in blocks {
            if !ids
                .entry(block.kind.clone())
                .or_default()
                .insert(block.name.clone())
            {
                return Err(ArtifactError::new(format!(
                    "duplicate {} `{}`",
                    block.kind, block.name
                )));
            }

            match block.kind.as_str() {
                "notes" => {
                    let text = take_text(&mut block, "text")?;
                    reject_unknown_fields(&block)?;
                    artifact.notes = Some(text);
                }
                "symbol" => {
                    let label = take_text(&mut block, "label")?;
                    let source = take_optional_text(&mut block, "source")?;
                    reject_unknown_fields(&block)?;
                    artifact.symbols.push(SymbolDecl {
                        value: block.name,
                        label,
                        source,
                        doc: block.doc,
                    });
                }
                "relation" => {
                    let args = take_list(&mut block, "args")?
                        .into_iter()
                        .map(
                            |value| match raw_text(value, "relation argument type")?.as_str() {
                                "symbol" => Ok(ValueType::Symbol),
                                "integer" => Ok(ValueType::Integer),
                                other => Err(ArtifactError::new(format!(
                                    "relation `{}` has unknown argument type `{other}`",
                                    block.name
                                ))),
                            },
                        )
                        .collect::<Result<Vec<_>, _>>()?;
                    let roles = take_optional_text_list(&mut block, "roles")?;
                    let reads = take_optional_text(&mut block, "reads")?;
                    validate_roles(&block, &args, &roles)?;
                    if let Some(template) = reads.as_deref() {
                        validate_reads(&block, template, &roles, args.len())?;
                    }
                    reject_unknown_fields(&block)?;
                    artifact.relations.push(RelationDecl {
                        name: block.name,
                        args,
                        roles,
                        reads,
                        doc: block.doc,
                    });
                }
                "fact" => {
                    let relation = take_text(&mut block, "relation")?;
                    let args = take_list(&mut block, "args")?
                        .into_iter()
                        .map(raw_fact_value)
                        .collect::<Result<Vec<_>, _>>()?;
                    let confidence = match block.fields.remove("confidence") {
                        Some(RawValue::Integer(value @ 0..=100)) => value as f64 / 100.0,
                        Some(RawValue::Integer(value)) => {
                            return Err(ArtifactError::new(format!(
                                "fact `{}` confidence must be between 0 and 100, got {value}",
                                block.name
                            )));
                        }
                        Some(_) => {
                            return Err(ArtifactError::new(format!(
                                "fact `{}` confidence must be an integer percentage",
                                block.name
                            )));
                        }
                        None => 1.0,
                    };
                    let provenance = match block.fields.remove("provenance") {
                        Some(RawValue::List(values)) => values
                            .into_iter()
                            .map(|value| raw_text(value, "provenance value"))
                            .collect::<Result<Vec<_>, _>>()?,
                        Some(_) => {
                            return Err(ArtifactError::new(format!(
                                "fact `{}` provenance must be a list",
                                block.name
                            )));
                        }
                        None => Vec::new(),
                    };
                    let basis = take_evidence_basis(&mut block)?;
                    reject_unknown_fields(&block)?;
                    artifact.facts.push(FactDecl {
                        id: block.name,
                        relation,
                        args,
                        confidence,
                        provenance,
                        basis,
                        doc: block.doc,
                    });
                }
                "rule" => {
                    let derive = take_text(&mut block, "derive")?;
                    let (when, condition_ids) = take_rule_conditions(&mut block)?;
                    if when.is_empty() {
                        return Err(ArtifactError::new(format!(
                            "rule `{}` must contain at least one condition",
                            block.name
                        )));
                    }
                    reject_unknown_fields(&block)?;
                    artifact.rules.push(RuleDecl {
                        id: block.name,
                        derive,
                        when,
                        condition_ids,
                        doc: block.doc,
                    });
                }
                "expect" => {
                    let query = take_text(&mut block, "query")?;
                    let count = match block.fields.remove("count") {
                        Some(RawValue::Integer(value)) if value >= 0 => value as usize,
                        Some(RawValue::Integer(value)) => {
                            return Err(ArtifactError::new(format!(
                                "expect `{}` count cannot be negative: {value}",
                                block.name
                            )));
                        }
                        Some(_) => {
                            return Err(ArtifactError::new(format!(
                                "expect `{}` count must be an integer",
                                block.name
                            )));
                        }
                        None => 1,
                    };
                    reject_unknown_fields(&block)?;
                    artifact.expectations.push(ExpectationDecl {
                        id: block.name,
                        query,
                        count,
                        doc: block.doc,
                    });
                }
                "mutation" => {
                    let operator = match take_text(&mut block, "operator")?.as_str() {
                        "drop_rule" => MutationOperator::DropRule,
                        "drop_condition" => MutationOperator::DropCondition,
                        "drop_fact" => MutationOperator::DropFact,
                        other => {
                            return Err(ArtifactError::new(format!(
                                "mutation `{}` has unknown operator `{other}`",
                                block.name
                            )));
                        }
                    };
                    let relation = take_optional_text(&mut block, "relation")?;
                    let except = take_optional_text_list(&mut block, "except")?;
                    let must_fail = take_optional_text(&mut block, "must_fail")?;
                    reject_unknown_fields(&block)?;
                    artifact.mutations.push(MutationDecl {
                        id: block.name,
                        operator,
                        relation,
                        except,
                        must_fail,
                        doc: block.doc,
                    });
                }
                other => {
                    return Err(ArtifactError::new(format!(
                        "unknown block kind `{other}`; expected relation, fact, rule, expect, mutation, symbol, or notes"
                    )));
                }
            }
        }

        artifact
            .symbols
            .sort_by(|left, right| left.value.cmp(&right.value));
        artifact
            .relations
            .sort_by(|left, right| left.name.cmp(&right.name));
        artifact.facts.sort_by(|left, right| left.id.cmp(&right.id));
        artifact.rules.sort_by(|left, right| left.id.cmp(&right.id));
        artifact
            .expectations
            .sort_by(|left, right| left.id.cmp(&right.id));
        artifact
            .mutations
            .sort_by(|left, right| left.id.cmp(&right.id));
        artifact.validate_mutations()?;
        if !artifact.symbols.is_empty() {
            let referenced = artifact.referenced_symbols()?;
            for symbol in &artifact.symbols {
                if !referenced.contains(&symbol.value) {
                    return Err(ArtifactError::new(format!(
                        "symbol `{}` labels an unknown symbol",
                        symbol.value
                    )));
                }
            }
        }
        Ok(artifact)
    }
}

fn validate_roles(
    block: &RawBlock,
    args: &[ValueType],
    roles: &[String],
) -> Result<(), ArtifactError> {
    if roles.is_empty() {
        return Ok(());
    }
    if roles.len() != args.len() {
        return Err(ArtifactError::new(format!(
            "relation `{}` declares {} roles for {} arguments",
            block.name,
            roles.len(),
            args.len()
        )));
    }
    let mut seen = BTreeSet::new();
    for role in roles {
        if role.is_empty() || !role.bytes().all(is_identifier_continue) {
            return Err(ArtifactError::new(format!(
                "relation `{}` role `{role}` must be an identifier",
                block.name
            )));
        }
        if !seen.insert(role.as_str()) {
            return Err(ArtifactError::new(format!(
                "relation `{}` repeats role `{role}`",
                block.name
            )));
        }
    }
    Ok(())
}

/// Every `{placeholder}` in a `reads` template must name a role or an
/// argument position, so a fact can always be read back as a sentence.
fn validate_reads(
    block: &RawBlock,
    template: &str,
    roles: &[String],
    arity: usize,
) -> Result<(), ArtifactError> {
    for placeholder in template_placeholders(template).map_err(|message| {
        ArtifactError::new(format!("relation `{}` reads {message}", block.name))
    })? {
        let known = roles.iter().any(|role| role == placeholder)
            || placeholder
                .parse::<usize>()
                .is_ok_and(|index| index < arity);
        if !known {
            return Err(ArtifactError::new(format!(
                "relation `{}` reads placeholder `{{{placeholder}}}` names no role or argument position",
                block.name
            )));
        }
    }
    Ok(())
}

fn template_placeholders(template: &str) -> Result<Vec<&str>, String> {
    let mut placeholders = Vec::new();
    let mut rest = template;
    while let Some(open) = rest.find('{') {
        let after = &rest[open + 1..];
        let Some(close) = after.find('}') else {
            return Err("template has an unclosed `{`".to_string());
        };
        placeholders.push(&after[..close]);
        rest = &after[close + 1..];
    }
    Ok(placeholders)
}

fn take_evidence_basis(block: &mut RawBlock) -> Result<Option<EvidenceBasis>, ArtifactError> {
    let Some(kind) = take_optional_text(block, "basis")? else {
        return Ok(None);
    };
    let kind = match kind.as_str() {
        "snapshot" => EvidenceKind::Snapshot,
        "policy" => EvidenceKind::Policy,
        "observed" => EvidenceKind::Observed,
        "reviewer_declared" => EvidenceKind::ReviewerDeclared,
        _ => {
            return Err(ArtifactError::new(
                "basis must be snapshot, policy, observed, or reviewer_declared",
            ))
        }
    };
    let source = take_text(block, "source")?;
    let identity = take_optional_text(block, "identity")?;
    if source.trim().is_empty()
        || identity
            .as_ref()
            .is_some_and(|value| value.trim().is_empty())
    {
        return Err(ArtifactError::new(
            "evidence source and identity must not be empty",
        ));
    }
    if matches!(kind, EvidenceKind::Snapshot | EvidenceKind::Observed) && identity.is_none() {
        return Err(ArtifactError::new(
            "snapshot and observed evidence require an identity",
        ));
    }
    Ok(Some(EvidenceBasis {
        kind,
        source,
        identity,
    }))
}

fn take_list(block: &mut RawBlock, field: &str) -> Result<Vec<RawValue>, ArtifactError> {
    match block.fields.remove(field) {
        Some(RawValue::List(values)) => Ok(values),
        Some(_) => Err(ArtifactError::new(format!(
            "{} `{}` field `{field}` must be a list",
            block.kind, block.name
        ))),
        None => Err(ArtifactError::new(format!(
            "{} `{}` is missing required field `{field}`",
            block.kind, block.name
        ))),
    }
}

fn take_text(block: &mut RawBlock, field: &str) -> Result<String, ArtifactError> {
    let value = block.fields.remove(field).ok_or_else(|| {
        ArtifactError::new(format!(
            "{} `{}` is missing required field `{field}`",
            block.kind, block.name
        ))
    })?;
    raw_text(value, field)
}

fn take_optional_text(block: &mut RawBlock, field: &str) -> Result<Option<String>, ArtifactError> {
    block
        .fields
        .remove(field)
        .map(|value| raw_text(value, field))
        .transpose()
}

fn take_optional_text_list(
    block: &mut RawBlock,
    field: &str,
) -> Result<Vec<String>, ArtifactError> {
    match block.fields.remove(field) {
        Some(RawValue::List(values)) => values
            .into_iter()
            .map(|value| raw_text(value, field))
            .collect(),
        Some(_) => Err(ArtifactError::new(format!(
            "{} `{}` field `{field}` must be a list",
            block.kind, block.name
        ))),
        None => Ok(Vec::new()),
    }
}

fn take_rule_conditions(block: &mut RawBlock) -> Result<(Vec<String>, Vec<String>), ArtifactError> {
    let value = block.fields.remove("when").ok_or_else(|| {
        ArtifactError::new(format!(
            "rule `{}` is missing required field `when`",
            block.name
        ))
    })?;
    match value {
        RawValue::List(values) => Ok((
            values
                .into_iter()
                .map(|value| raw_text(value, "rule condition"))
                .collect::<Result<Vec<_>, _>>()?,
            Vec::new(),
        )),
        RawValue::Map(entries) => {
            let mut ids = Vec::with_capacity(entries.len());
            let mut conditions = Vec::with_capacity(entries.len());
            for (id, value) in entries {
                ids.push(id);
                conditions.push(raw_text(value, "named rule condition")?);
            }
            Ok((conditions, ids))
        }
        _ => Err(ArtifactError::new(format!(
            "rule `{}` field `when` must be a list or named map",
            block.name
        ))),
    }
}

fn raw_text(value: RawValue, description: &str) -> Result<String, ArtifactError> {
    match value {
        RawValue::Identifier(value) | RawValue::String(value) => Ok(value),
        _ => Err(ArtifactError::new(format!(
            "{description} must be an identifier or string"
        ))),
    }
}

fn raw_fact_value(value: RawValue) -> Result<FactValue, ArtifactError> {
    match value {
        RawValue::Identifier(value) | RawValue::String(value) => Ok(FactValue::Symbol(value)),
        RawValue::Integer(value) => Ok(FactValue::Integer(value)),
        RawValue::List(_) => Err(ArtifactError::new(
            "fact arguments cannot contain nested lists",
        )),
        RawValue::Map(_) => Err(ArtifactError::new("fact arguments cannot contain maps")),
    }
}

fn reject_unknown_fields(block: &RawBlock) -> Result<(), ArtifactError> {
    if let Some(field) = block.fields.keys().next() {
        Err(ArtifactError::new(format!(
            "{} `{}` has unknown field `{field}`",
            block.kind, block.name
        )))
    } else {
        Ok(())
    }
}
