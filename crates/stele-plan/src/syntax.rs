//! The block syntax catalogs and plans share: a header `catalog NAME { ... }`
//! or `plan NAME { ... }`, `use "<path>" as <alias>` imports first, then
//! blocks `KIND ID { field: value ... }`. The ledgers beside a catalog are
//! records `KIND { field: value ... }` with no header and no ID, whose values
//! may also be integers and `key: value` list entries.

use std::{collections::BTreeMap, fmt};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct File {
    pub path: String,
    pub header: Header,
    /// The name in `catalog NAME` or `plan NAME`.
    pub name: String,
    pub imports: Vec<Import>,
    pub blocks: Vec<Block>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Header {
    Catalog,
    Plan,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Import {
    pub path: String,
    pub alias: String,
    pub line: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Block {
    pub kind: String,
    pub name: String,
    pub fields: Vec<Field>,
    pub line: usize,
}

impl Block {
    /// The block's fields by name. An unknown field fails as
    /// `path:line: <subject> has no field <name>`, a repeated one as
    /// `path:line: duplicate field <name> in <id>`.
    pub fn fields_by_name(
        &self,
        path: &str,
        allowed: &[&str],
        subject: &str,
    ) -> Result<Fields<'_>, String> {
        Fields::new(&self.fields, path, self.line, allowed, subject, &self.name)
    }
}

/// A ledger's entry: `KIND { field: value ... }`, with no ID.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Record {
    pub kind: String,
    pub fields: Vec<Field>,
    pub line: usize,
}

impl Record {
    /// The record's fields by name, failing as [`Block::fields_by_name`]
    /// does, but naming `<subject>` where a repeated field is reported.
    pub fn fields_by_name(
        &self,
        path: &str,
        allowed: &[&str],
        subject: &str,
    ) -> Result<Fields<'_>, String> {
        Fields::new(&self.fields, path, self.line, allowed, subject, subject)
    }
}

pub struct Fields<'a> {
    by_name: BTreeMap<&'a str, &'a Field>,
    at: String,
    subject: String,
}

impl<'a> Fields<'a> {
    fn new(
        fields: &'a [Field],
        path: &str,
        line: usize,
        allowed: &[&str],
        subject: &str,
        owner: &str,
    ) -> Result<Self, String> {
        let mut by_name = BTreeMap::new();
        for field in fields {
            let at = format!("{path}:{}", field.line);
            if !allowed.contains(&field.name.as_str()) {
                return Err(format!("{at}: {subject} has no field {}", field.name));
            }
            if by_name.insert(field.name.as_str(), field).is_some() {
                return Err(format!("{at}: duplicate field {} in {owner}", field.name));
            }
        }
        Ok(Self {
            by_name,
            at: format!("{path}:{line}"),
            subject: subject.into(),
        })
    }

    pub fn get(&self, name: &str) -> Option<&'a Field> {
        self.by_name.get(name).copied()
    }

    /// Fails as `path:line: <subject> needs <name>`, at the block's line.
    pub fn required(&self, name: &str) -> Result<&'a Field, String> {
        self.get(name)
            .ok_or_else(|| format!("{}: {} needs {name}", self.at, self.subject))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Field {
    pub name: String,
    pub value: Value,
    pub line: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Value {
    Ident(String),
    /// `alias.id`
    Qualified(String, String),
    Str(String),
    /// Only in a ledger.
    Int(u64),
    List(Vec<Self>),
    /// `key: value`, only as a list item in a ledger.
    Entry(String, Box<Self>),
}

impl Value {
    pub fn as_ident(&self) -> Option<&str> {
        match self {
            Self::Ident(word) => Some(word),
            _ => None,
        }
    }
}

impl fmt::Display for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Ident(id) => write!(f, "{id}"),
            Self::Qualified(alias, id) => write!(f, "{alias}.{id}"),
            Self::Str(text) => write!(f, "{text:?}"),
            Self::Int(number) => write!(f, "{number}"),
            Self::List(items) => {
                write!(f, "[")?;
                for (index, item) in items.iter().enumerate() {
                    if index > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{item}")?;
                }
                write!(f, "]")
            }
            Self::Entry(key, value) => write!(f, "{key}: {value}"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Token {
    Word(String),
    Str(String),
    Int(u64),
    Punct(char),
}

/// Parse one catalog or plan file. `path` names the file in errors as
/// `path:line: message`.
pub fn parse(source: &str, path: &str) -> Result<File, String> {
    let mut parser = Parser {
        tokens: lex(source, path)?,
        position: 0,
        path,
        ledger: false,
    };
    let header = if parser.at_word("catalog") {
        Header::Catalog
    } else if parser.at_word("plan") {
        Header::Plan
    } else if parser.at_word("spec") {
        return Err(
            parser.error("this is a specification, not a catalog or plan; run it with stele spec")
        );
    } else {
        return Err(parser.unexpected("catalog NAME or plan NAME"));
    };
    parser.advance();
    let name = parser.name("a name")?;
    parser.expect('{')?;
    let mut imports = Vec::new();
    while parser.at_word("use") {
        let line = parser.line();
        parser.advance();
        let Some(Token::Str(path)) = parser.peek().cloned() else {
            return Err(parser.unexpected("import path string"));
        };
        parser.advance();
        if !parser.at_word("as") {
            return Err(parser.unexpected("as"));
        }
        parser.advance();
        let alias = parser.name("import alias")?;
        imports.push(Import { path, alias, line });
    }
    let mut blocks = Vec::new();
    while !parser.at('}') {
        if parser.at_word("use") {
            return Err(parser.error("use must come before the blocks"));
        }
        blocks.push(parser.block()?);
    }
    parser.expect('}')?;
    if parser.peek().is_some() {
        return Err(parser.unexpected("end of file"));
    }
    Ok(File {
        path: path.into(),
        header,
        name,
        imports,
        blocks,
    })
}

/// Parse a ledger: records with no header, in file order. `path` names the
/// file in errors as `path:line: message`.
pub fn parse_ledger(source: &str, path: &str) -> Result<Vec<Record>, String> {
    let mut parser = Parser {
        tokens: lex(source, path)?,
        position: 0,
        path,
        ledger: true,
    };
    let mut records = Vec::new();
    while parser.peek().is_some() {
        records.push(parser.record()?);
    }
    Ok(records)
}

/// `text` as a string literal that reads back as `text`.
pub fn quote(text: &str) -> String {
    let mut quoted = String::from('"');
    for c in text.chars() {
        match c {
            '"' => quoted.push_str("\\\""),
            '\\' => quoted.push_str("\\\\"),
            '\n' => quoted.push_str("\\n"),
            '\r' => quoted.push_str("\\r"),
            '\t' => quoted.push_str("\\t"),
            c => quoted.push(c),
        }
    }
    quoted.push('"');
    quoted
}

struct Parser<'a> {
    tokens: Vec<(Token, usize)>,
    position: usize,
    path: &'a str,
    /// Whether integers and list entries are values.
    ledger: bool,
}

impl Parser<'_> {
    fn peek(&self) -> Option<&Token> {
        self.tokens.get(self.position).map(|(token, _)| token)
    }

    const fn advance(&mut self) {
        self.position += 1;
    }

    fn line(&self) -> usize {
        self.tokens
            .get(self.position)
            .or_else(|| self.tokens.last())
            .map_or(1, |(_, line)| *line)
    }

    fn at(&self, punct: char) -> bool {
        self.peek() == Some(&Token::Punct(punct))
    }

    fn at_word(&self, word: &str) -> bool {
        matches!(self.peek(), Some(Token::Word(found)) if found == word)
    }

    fn error(&self, message: impl fmt::Display) -> String {
        format!("{}:{}: {message}", self.path, self.line())
    }

    fn unexpected(&self, expected: &str) -> String {
        let found = match self.peek() {
            None => "end of file".into(),
            Some(Token::Word(word)) => word.clone(),
            Some(Token::Str(_)) => "a string".into(),
            Some(Token::Int(number)) => number.to_string(),
            Some(Token::Punct(punct)) => format!("'{punct}'"),
        };
        self.error(format!("expected {expected}, found {found}"))
    }

    fn expect(&mut self, punct: char) -> Result<(), String> {
        if !self.at(punct) {
            return Err(self.unexpected(&format!("'{punct}'")));
        }
        self.advance();
        Ok(())
    }

    fn name(&mut self, what: &str) -> Result<String, String> {
        match self.peek() {
            Some(Token::Word(word)) if !word.contains('.') => {
                let word = word.clone();
                self.advance();
                Ok(word)
            }
            _ => Err(self.unexpected(what)),
        }
    }

    fn block(&mut self) -> Result<Block, String> {
        let line = self.line();
        let kind = self.name("block kind")?;
        let name = self.name("block id")?;
        Ok(Block {
            kind,
            name,
            fields: self.fields()?,
            line,
        })
    }

    fn record(&mut self) -> Result<Record, String> {
        let line = self.line();
        let kind = self.name("entry kind")?;
        Ok(Record {
            kind,
            fields: self.fields()?,
            line,
        })
    }

    /// `{ field: value ... }`
    fn fields(&mut self) -> Result<Vec<Field>, String> {
        self.expect('{')?;
        let mut fields = Vec::new();
        while !self.at('}') {
            let line = self.line();
            let name = self.name("field name")?;
            self.expect(':')?;
            let value = self.value()?;
            fields.push(Field { name, value, line });
        }
        self.expect('}')?;
        Ok(fields)
    }

    fn value(&mut self) -> Result<Value, String> {
        if self.at('[') {
            self.advance();
            let mut items = Vec::new();
            while !self.at(']') {
                items.push(self.item()?);
                if !self.at(',') {
                    break;
                }
                self.advance();
            }
            self.expect(']')?;
            return Ok(Value::List(items));
        }
        let value = match self.peek() {
            Some(Token::Str(text)) => Value::Str(text.clone()),
            Some(Token::Int(number)) if self.ledger => Value::Int(*number),
            Some(Token::Word(word)) => match word.split_once('.') {
                Some((alias, id)) => Value::Qualified(alias.into(), id.into()),
                None => Value::Ident(word.clone()),
            },
            _ => return Err(self.unexpected("a value")),
        };
        self.advance();
        Ok(value)
    }

    /// A list item: a value, or in a ledger a `key: value` entry.
    fn item(&mut self) -> Result<Value, String> {
        let item = self.value()?;
        if !(self.ledger && self.at(':')) {
            return Ok(item);
        }
        let Value::Ident(key) = item else {
            return Err(self.error("a list entry key must be an identifier"));
        };
        self.advance();
        Ok(Value::Entry(key, Box::new(self.value()?)))
    }
}

fn lex(source: &str, path: &str) -> Result<Vec<(Token, usize)>, String> {
    let mut tokens = Vec::new();
    let mut chars = source.chars().peekable();
    let mut line = 1;
    let error = |line: usize, message: String| format!("{path}:{line}: {message}");
    while let Some(c) = chars.next() {
        match c {
            '\n' => line += 1,
            c if c.is_whitespace() => {}
            '/' if chars.peek() == Some(&'/') => while chars.next_if(|c| *c != '\n').is_some() {},
            '{' | '}' | '[' | ']' | ':' | ',' => tokens.push((Token::Punct(c), line)),
            '"' => {
                let mut text = String::new();
                loop {
                    match chars.next() {
                        Some('"') => break,
                        Some('\\') => text.push(match chars.next() {
                            Some('n') => '\n',
                            Some('r') => '\r',
                            Some('t') => '\t',
                            Some(c @ ('"' | '\\')) => c,
                            Some(c) if c != '\n' => {
                                return Err(error(line, format!("unsupported escape \\{c}")));
                            }
                            _ => return Err(error(line, "unterminated string".into())),
                        }),
                        Some('\n') | None => return Err(error(line, "unterminated string".into())),
                        Some(c) => text.push(c),
                    }
                }
                tokens.push((Token::Str(text), line));
            }
            c if c.is_ascii_alphanumeric() || c == '_' => {
                let mut word = String::from(c);
                while let Some(c) =
                    chars.next_if(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '.')
                {
                    word.push(c);
                }
                tokens.push((word_token(word).map_err(|e| error(line, e))?, line));
            }
            c => return Err(error(line, format!("unexpected character {c:?}"))),
        }
    }
    Ok(tokens)
}

/// A word starting with a digit is an integer; any other is an identifier
/// or `alias.id`.
fn word_token(word: String) -> Result<Token, String> {
    if word.starts_with(|c: char| c.is_ascii_digit()) {
        return match word.parse() {
            Ok(number) if word.bytes().all(|b| b.is_ascii_digit()) => Ok(Token::Int(number)),
            _ => Err(format!("invalid integer {word}")),
        };
    }
    let parts: Vec<_> = word.split('.').collect();
    if parts.len() > 2
        || parts
            .iter()
            .any(|part| !part.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_'))
    {
        return Err(format!("invalid identifier {word}"));
    }
    Ok(Token::Word(word))
}
