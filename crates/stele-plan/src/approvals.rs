//! The approvals ledger beside a catalog root: who approved each statement
//! and example as it read then. [`approve`], [`reconfirm`] and [`remove`]
//! write its entries and [`append`] adds them to the file, which only ever
//! grows. Entries count in file order, and the last entry for an item wins.

use std::{collections::BTreeMap, fmt, fs::OpenOptions, io::Write, path::Path};

use crate::{
    APPROVALS_FILE, DirReader, Reader, Statement, StatementKind,
    digest::{DIGEST_VERSION, statement_digest},
    examples::{Example, packages, scan},
    load::{ledger_path, read_or_empty},
    load_catalog,
    syntax::{Field, Record, Value, parse_ledger, quote},
};

/// The kind of item an entry names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ItemKind {
    Intent,
    Definition,
    Exception,
    Channel,
    Example,
}

impl ItemKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Intent => "intent",
            Self::Definition => "definition",
            Self::Exception => "exception",
            Self::Channel => "channel",
            Self::Example => "example",
        }
    }

    fn parse(text: &str) -> Option<Self> {
        match text {
            "intent" => Some(Self::Intent),
            "definition" => Some(Self::Definition),
            "exception" => Some(Self::Exception),
            "channel" => Some(Self::Channel),
            "example" => Some(Self::Example),
            _ => None,
        }
    }

    pub(crate) const fn of(kind: &StatementKind) -> Self {
        match kind {
            StatementKind::Intent => Self::Intent,
            StatementKind::Definition { .. } => Self::Definition,
            StatementKind::Exception { .. } => Self::Exception,
            StatementKind::Channel => Self::Channel,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Approve,
    Remove,
}

impl Action {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Approve => "approve",
            Self::Remove => "remove",
        }
    }

    fn parse(text: &str) -> Option<Self> {
        match text {
            "approve" => Some(Self::Approve),
            "remove" => Some(Self::Remove),
            _ => None,
        }
    }
}

/// One entry of the ledger: an approval of an item, or the approved removal
/// of one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub item: String,
    pub kind: ItemKind,
    pub action: Action,
    /// The approver, as given; nothing authenticates it.
    pub by: String,
    /// The item's digest when approved; a removal repeats its last approved
    /// digest.
    pub digest: String,
    pub digest_version: u64,
    /// Each resolved link's target with that target's digest at this entry:
    /// a definition's refines, an exception's narrows, an example's
    /// statement.
    pub links: BTreeMap<String, String>,
    /// An example's channel tags, sorted.
    pub channels: Vec<String>,
    /// A statement's prose when approved.
    pub text: Option<String>,
}

impl fmt::Display for Entry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "approval {{")?;
        writeln!(f, "  item: {}", self.item)?;
        writeln!(f, "  kind: {}", self.kind.as_str())?;
        writeln!(f, "  action: {}", self.action.as_str())?;
        writeln!(f, "  by: {}", quote(&self.by))?;
        writeln!(f, "  digest: {}", quote(&self.digest))?;
        writeln!(f, "  digest_version: {}", self.digest_version)?;
        if !self.links.is_empty() {
            let links: Vec<String> = self
                .links
                .iter()
                .map(|(target, digest)| format!("{target}: {}", quote(digest)))
                .collect();
            writeln!(f, "  links: [{}]", links.join(", "))?;
        }
        if !self.channels.is_empty() {
            writeln!(f, "  channels: [{}]", self.channels.join(", "))?;
        }
        if let Some(text) = &self.text {
            writeln!(f, "  text: {}", quote(text))?;
        }
        writeln!(f, "}}")
    }
}

/// The ledger's entries in file order.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Log {
    pub entries: Vec<Entry>,
}

impl Log {
    /// Parse and check the entries of the ledger at `path`. Errors read
    /// `path:line: message`.
    pub fn parse(source: &str, path: &str) -> Result<Self, String> {
        let entries = parse_ledger(source, path)?
            .iter()
            .map(|record| entry(record, path))
            .collect::<Result<_, _>>()?;
        Ok(Self { entries })
    }

    /// Read the ledger at `path`; a missing ledger is empty.
    pub fn read(reader: &dyn Reader, path: &str) -> Result<Self, String> {
        Self::parse(&read_or_empty(reader, path)?, path)
    }

    /// The last entry for each item, whatever its action.
    pub fn latest(&self) -> BTreeMap<&str, &Entry> {
        self.entries
            .iter()
            .map(|entry| (entry.item.as_str(), entry))
            .collect()
    }

    /// The latest entries of approved statements and examples that are
    /// missing now, in item order. They still count until their removal is
    /// approved. An example counts as present only when the scan accepted it.
    pub fn reinstated(
        &self,
        catalog: &BTreeMap<String, Statement>,
        examples: &[Example],
    ) -> Vec<&Entry> {
        self.latest()
            .into_values()
            .filter(|entry| entry.action == Action::Approve)
            .filter(|entry| {
                !catalog.contains_key(&entry.item)
                    && !examples.iter().any(|example| example.id == entry.item)
            })
            .collect()
    }
}

const FIELDS: [&str; 9] = [
    "item",
    "kind",
    "action",
    "by",
    "digest",
    "digest_version",
    "links",
    "channels",
    "text",
];

fn entry(record: &Record, path: &str) -> Result<Entry, String> {
    let at = |line: usize| format!("{path}:{line}");
    if record.kind != "approval" {
        return Err(format!(
            "{}: expected an approval entry, found {}",
            at(record.line),
            record.kind
        ));
    }
    let item = item(record, path)?;
    let subject = format!("approval of {item}");
    let fields = record.fields_by_name(path, &FIELDS, &subject)?;
    let string = |field: &Field| match &field.value {
        Value::Str(text) => Ok(text.clone()),
        _ => Err(format!(
            "{}: {} of {item} must be a string",
            at(field.line),
            field.name
        )),
    };

    let field = fields.required("kind")?;
    let kind = field
        .value
        .as_ident()
        .and_then(ItemKind::parse)
        .ok_or_else(|| {
            format!(
                "{}: approval of {item} has unknown kind {}; expected intent, definition, exception, channel or example",
                at(field.line),
                field.value
            )
        })?;
    let field = fields.required("action")?;
    let action = field
        .value
        .as_ident()
        .and_then(Action::parse)
        .ok_or_else(|| {
            format!(
                "{}: approval of {item} has unknown action {}; expected approve or remove",
                at(field.line),
                field.value
            )
        })?;
    let by = string(fields.required("by")?)?;
    let digest = string(fields.required("digest")?)?;
    let field = fields.required("digest_version")?;
    let Value::Int(digest_version) = field.value else {
        return Err(format!(
            "{}: digest_version of {item} must be an integer",
            at(field.line)
        ));
    };
    let links = links(fields.get("links"), &item, path)?;
    let channels = channels(fields.get("channels"), &item, path)?;
    let text = fields.get("text").map(string).transpose()?;
    Ok(Entry {
        item,
        kind,
        action,
        by,
        digest,
        digest_version,
        links,
        channels,
        text,
    })
}

/// The ID in `item`, read before the other fields so their errors can name
/// it.
fn item(record: &Record, path: &str) -> Result<String, String> {
    match record.fields.iter().find(|field| field.name == "item") {
        None => Err(format!("{path}:{}: approval needs item", record.line)),
        Some(Field {
            value: Value::Ident(id),
            ..
        }) => Ok(id.clone()),
        Some(field) => Err(format!(
            "{path}:{}: item of an approval must be an item ID",
            field.line
        )),
    }
}

/// `links: [target: "digest", ...]`, naming each target once.
fn links(
    field: Option<&Field>,
    item: &str,
    path: &str,
) -> Result<BTreeMap<String, String>, String> {
    let mut links = BTreeMap::new();
    let Some(field) = field else {
        return Ok(links);
    };
    let at = format!("{path}:{}", field.line);
    let malformed =
        || format!("{at}: links of {item} must be a list of target: \"digest\" entries");
    let Value::List(values) = &field.value else {
        return Err(malformed());
    };
    for value in values {
        let Value::Entry(target, digest) = value else {
            return Err(malformed());
        };
        let Value::Str(digest) = digest.as_ref() else {
            return Err(malformed());
        };
        if links.insert(target.clone(), digest.clone()).is_some() {
            return Err(format!("{at}: links of {item} name {target} twice"));
        }
    }
    Ok(links)
}

/// `channels: [channel, ...]`.
fn channels(field: Option<&Field>, item: &str, path: &str) -> Result<Vec<String>, String> {
    let Some(field) = field else {
        return Ok(Vec::new());
    };
    let malformed = || {
        format!(
            "{path}:{}: channels of {item} must be a list of channel IDs",
            field.line
        )
    };
    let Value::List(values) = &field.value else {
        return Err(malformed());
    };
    values
        .iter()
        .map(|value| match value {
            Value::Ident(channel) => Ok(channel.clone()),
            _ => Err(malformed()),
        })
        .collect()
}

/// What an approval can name.
enum Item<'a> {
    Statement(&'a Statement),
    Example(&'a Example),
}

fn find<'a>(
    catalog: &'a BTreeMap<String, Statement>,
    examples: &'a [Example],
    id: &str,
) -> Result<Option<Item<'a>>, String> {
    let statement = catalog.get(id);
    let example = examples.iter().find(|example| example.id == id);
    match (statement, example) {
        (Some(_), Some(_)) => Err(format!("{id} names both a statement and an example")),
        (Some(statement), None) => Ok(Some(Item::Statement(statement))),
        (None, Some(example)) => Ok(Some(Item::Example(example))),
        (None, None) => Ok(None),
    }
}

fn current<'a>(
    catalog: &'a BTreeMap<String, Statement>,
    examples: &'a [Example],
    id: &str,
) -> Result<Item<'a>, String> {
    find(catalog, examples, id)?.ok_or_else(|| format!("unknown item {id}"))
}

fn approval(
    item: &Item<'_>,
    catalog: &BTreeMap<String, Statement>,
    by: &str,
) -> Result<Entry, String> {
    match item {
        Item::Statement(statement) => {
            let link = match &statement.kind {
                StatementKind::Definition { refines } => Some(refines.as_str()),
                StatementKind::Exception { narrows, .. } => Some(narrows.as_str()),
                StatementKind::Intent | StatementKind::Channel => None,
            };
            Ok(Entry {
                item: statement.id.clone(),
                kind: ItemKind::of(&statement.kind),
                action: Action::Approve,
                by: by.into(),
                digest: statement_digest(statement),
                digest_version: DIGEST_VERSION,
                links: current_links(catalog, &statement.id, link)?,
                channels: Vec::new(),
                text: Some(statement.text.clone()),
            })
        }
        Item::Example(example) => Ok(Entry {
            item: example.id.clone(),
            kind: ItemKind::Example,
            action: Action::Approve,
            by: by.into(),
            digest: example.digest.clone(),
            digest_version: DIGEST_VERSION,
            links: current_links(catalog, &example.id, Some(&example.statement))?,
            channels: example.channels.clone(),
            text: None,
        }),
    }
}

fn current_links(
    catalog: &BTreeMap<String, Statement>,
    id: &str,
    target: Option<&str>,
) -> Result<BTreeMap<String, String>, String> {
    let mut links = BTreeMap::new();
    if let Some(target) = target {
        let statement = catalog
            .get(target)
            .ok_or_else(|| format!("{id} links to {target}, which is not in the catalog"))?;
        links.insert(target.to_string(), statement_digest(statement));
    }
    Ok(links)
}

fn approver(by: &str) -> Result<(), String> {
    if by.trim().is_empty() {
        return Err("an approval needs the approver's name".into());
    }
    if by.chars().any(char::is_control) {
        return Err(format!(
            "the approver's name {by:?} has a control character"
        ));
    }
    Ok(())
}

/// The entries approving each named statement or example as it reads now,
/// with the current digest of each statement it links to.
pub fn approve(
    catalog: &BTreeMap<String, Statement>,
    examples: &[Example],
    ids: &[impl AsRef<str>],
    by: &str,
) -> Result<String, String> {
    approver(by)?;
    let mut text = String::new();
    for id in ids {
        let item = current(catalog, examples, id.as_ref())?;
        text += &approval(&item, catalog, by)?.to_string();
    }
    Ok(text)
}

/// Like [`approve`], for items whose own digest is unchanged since their
/// latest entry approved them, so only the linked digests can differ.
pub fn reconfirm(
    log: &Log,
    catalog: &BTreeMap<String, Statement>,
    examples: &[Example],
    ids: &[impl AsRef<str>],
    by: &str,
) -> Result<String, String> {
    approver(by)?;
    let latest = log.latest();
    let mut text = String::new();
    for id in ids {
        let id = id.as_ref();
        let entry = approval(&current(catalog, examples, id)?, catalog, by)?;
        let Some(approved) = latest.get(id).filter(|e| e.action == Action::Approve) else {
            return Err(format!("{id} is not approved; approve it instead"));
        };
        if (&approved.digest, approved.digest_version) != (&entry.digest, entry.digest_version) {
            return Err(format!(
                "{id} changed since its latest approval; approve it instead"
            ));
        }
        text += &entry.to_string();
    }
    Ok(text)
}

/// The entries removing each named approved item, refused while an item
/// that is approved, and not removed by this same call, recorded a link to
/// it.
pub fn remove(
    log: &Log,
    catalog: &BTreeMap<String, Statement>,
    examples: &[Example],
    ids: &[impl AsRef<str>],
    by: &str,
) -> Result<String, String> {
    approver(by)?;
    let latest = log.latest();
    let removing: Vec<&str> = ids.iter().map(AsRef::as_ref).collect();
    let mut text = String::new();
    for &id in &removing {
        let present = find(catalog, examples, id)?.is_some();
        let approved = match latest.get(id) {
            Some(entry) if entry.action == Action::Approve => *entry,
            None if !present => return Err(format!("unknown item {id}")),
            _ => {
                return Err(format!(
                    "{id} is not approved, so there is nothing to remove"
                ));
            }
        };
        let dependents: Vec<&str> = latest
            .values()
            .filter(|entry| entry.action == Action::Approve && entry.links.contains_key(id))
            .map(|entry| entry.item.as_str())
            .filter(|item| !removing.contains(item))
            .collect();
        if !dependents.is_empty() {
            return Err(format!(
                "cannot remove {id}: approved items still link to it: {}",
                dependents.join(", ")
            ));
        }
        text += &Entry {
            action: Action::Remove,
            by: by.into(),
            links: BTreeMap::new(),
            channels: Vec::new(),
            text: None,
            ..approved.clone()
        }
        .to_string();
    }
    Ok(text)
}

/// Load the catalog rooted at `catalog`, a path from `base`, and the
/// examples of the packages at `base`, then append the entries `change`
/// returns to the ledger beside the root. Nothing is written when the
/// catalog, the examples, the ledger or the change fails, and the ledger's
/// existing bytes always stay an unchanged prefix.
pub fn append(
    base: &Path,
    catalog: &str,
    change: impl FnOnce(&Log, &BTreeMap<String, Statement>, &[Example]) -> Result<String, String>,
) -> Result<(), String> {
    let reader = DirReader(base.into());
    let statements = load_catalog(&reader, catalog)?;
    let examples = scan(base, &packages(base)?, &statements)?.examples;
    let path = ledger_path(catalog, APPROVALS_FILE);
    let source = read_or_empty(&reader, &path)?;
    let mut text = change(&Log::parse(&source, &path)?, &statements, &examples)?;
    if !source.is_empty() && !source.ends_with('\n') {
        text.insert(0, '\n');
    }
    OpenOptions::new()
        .create(true)
        .append(true)
        .open(base.join(&path))
        .and_then(|mut file| file.write_all(text.as_bytes()))
        .map_err(|e| format!("{path}: {e}"))
}
