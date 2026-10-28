//! Reading a catalog or a plan from files and checking every link.

use std::{
    collections::BTreeMap,
    fs, io,
    path::{Path, PathBuf},
};

use crate::{
    Change, Decision, Document, ExceptionKind, Plan, Proposal, State, Statement, StatementKind,
    syntax::{Block, Field, Fields, File, Header, Value, parse},
};

/// The ledger of approvals, beside the catalog root. It is read by this
/// fixed name and never imported.
pub const APPROVALS_FILE: &str = "approvals.stele";
/// The ledger of runs, beside the catalog root, read the same way.
pub const RUNS_FILE: &str = "runs.stele";

/// Files addressed by `/`-separated paths from one base directory.
pub trait Reader {
    fn read(&self, path: &str) -> io::Result<String>;
    /// Every file under `dir`, by its path from the base.
    fn list(&self, dir: &str) -> io::Result<Vec<String>>;
}

impl Reader for BTreeMap<String, String> {
    fn read(&self, path: &str) -> io::Result<String> {
        self.get(path)
            .cloned()
            .ok_or_else(|| io::ErrorKind::NotFound.into())
    }

    fn list(&self, dir: &str) -> io::Result<Vec<String>> {
        Ok(self
            .keys()
            .filter(|path| path.starts_with(&format!("{dir}/")))
            .cloned()
            .collect())
    }
}

/// Files under a directory on disk.
pub struct DirReader(pub PathBuf);

impl Reader for DirReader {
    fn read(&self, path: &str) -> io::Result<String> {
        fs::read_to_string(self.0.join(path))
    }

    fn list(&self, dir: &str) -> io::Result<Vec<String>> {
        let mut files = Vec::new();
        list_dir(&self.0.join(dir), dir, &mut files)?;
        Ok(files)
    }
}

/// The file at `path`, or nothing when there is none.
pub fn read_or_empty(reader: &dyn Reader, path: &str) -> Result<String, String> {
    match reader.read(path) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(String::new()),
        read => read.map_err(|e| format!("{path}: {e}")),
    }
}

fn list_dir(dir: &Path, prefix: &str, files: &mut Vec<String>) -> io::Result<()> {
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = format!("{prefix}/{}", entry.file_name().to_string_lossy());
        if entry.file_type()?.is_dir() {
            list_dir(&entry.path(), &path, files)?;
        } else {
            files.push(path);
        }
    }
    Ok(())
}

/// Load the catalog rooted at `path`, or the plan at `path` with the catalog
/// it imports, and check IDs, references and link kinds. Fails on the first
/// error, as `path:line: message`.
pub fn load(reader: &dyn Reader, path: &str) -> Result<Document, String> {
    let file = read_file(reader, path, None)?;
    match file.header {
        Header::Catalog => Ok(Document {
            catalog: catalog(reader, file)?,
            plan: None,
        }),
        Header::Plan => load_plan(reader, file),
    }
}

/// Load the catalog rooted at `path` as [`load`] does, refusing a plan.
pub fn load_catalog(
    reader: &dyn Reader,
    path: &str,
) -> Result<BTreeMap<String, Statement>, String> {
    let file = read_file(reader, path, None)?;
    if file.header == Header::Plan {
        return Err(format!("{path} is a plan, not a catalog"));
    }
    catalog(reader, file)
}

fn catalog(reader: &dyn Reader, root: File) -> Result<BTreeMap<String, Statement>, String> {
    let files = catalog_files(reader, root)?;
    let mut declared = BTreeMap::new();
    declare(&files, true, &mut declared)?;
    catalog_statements(&files, &declared)
}

fn read_file(reader: &dyn Reader, path: &str, imported_at: Option<&str>) -> Result<File, String> {
    let source = reader
        .read(path)
        .map_err(|e| match (e.kind(), imported_at) {
            (io::ErrorKind::NotFound, Some(at)) => format!("{at}: imported file {path} not found"),
            (io::ErrorKind::NotFound, None) => format!("{path} not found"),
            _ => format!("{path}: {e}"),
        })?;
    parse(&source, path)
}

/// Where an import alias points: one catalog file, or, from a plan, the
/// whole catalog it imports.
enum Scope {
    File(String),
    Catalog,
}

struct Loaded {
    file: File,
    scopes: BTreeMap<String, Scope>,
}

/// The catalog root and every file it imports. Every `.stele` file in the
/// root's directory must be among them, apart from the two ledgers beside
/// the root.
fn catalog_files(reader: &dyn Reader, root: File) -> Result<Vec<Loaded>, String> {
    let dir = parent(&root.path).to_string();
    if dir.is_empty() {
        return Err(format!(
            "{}:1: a catalog root must sit in a directory of its own",
            root.path
        ));
    }
    let root_path = root.path.clone();
    let mut files = vec![Loaded {
        file: root,
        scopes: BTreeMap::new(),
    }];
    walk_imports(reader, &dir, 0, &mut files, &mut vec![root_path.clone()])?;
    let mut paths = reader
        .list(&dir)
        .map_err(|e| format!("cannot list the catalog directory {dir}: {e}"))?;
    paths.sort();
    let ledgers = [APPROVALS_FILE, RUNS_FILE].map(|name| format!("{dir}/{name}"));
    for path in paths {
        let is_stele = Path::new(&path)
            .extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("stele"));
        if is_stele
            && !ledgers.contains(&path)
            && !files.iter().any(|loaded| loaded.file.path == path)
        {
            return Err(format!("{path}: not imported from {root_path}"));
        }
    }
    Ok(files)
}

fn walk_imports(
    reader: &dyn Reader,
    dir: &str,
    index: usize,
    files: &mut Vec<Loaded>,
    chain: &mut Vec<String>,
) -> Result<(), String> {
    let importer = files[index].file.path.clone();
    for import in files[index].file.imports.clone() {
        let at = format!("{importer}:{}", import.line);
        let target = import_target(&importer, &import.path)
            .filter(|target| target.starts_with(&format!("{dir}/")))
            .ok_or_else(|| {
                format!(
                    "{at}: import {:?} must be a relative path inside the catalog directory",
                    import.path
                )
            })?;
        if chain.contains(&target) {
            return Err(format!("{at}: import cycle through {target}"));
        }
        if files[index]
            .scopes
            .insert(import.alias.clone(), Scope::File(target.clone()))
            .is_some()
        {
            return Err(format!("{at}: duplicate import alias {}", import.alias));
        }
        if files.iter().any(|loaded| loaded.file.path == target) {
            continue;
        }
        let file = read_file(reader, &target, Some(&at))?;
        if file.header != Header::Catalog {
            return Err(format!("{at}: {target} is not a catalog file"));
        }
        files.push(Loaded {
            file,
            scopes: BTreeMap::new(),
        });
        chain.push(target);
        walk_imports(reader, dir, files.len() - 1, files, chain)?;
        chain.pop();
    }
    Ok(())
}

pub fn parent(path: &str) -> &str {
    path.rsplit_once('/').map_or("", |(dir, _)| dir)
}

/// The path of the ledger `name` beside the catalog root at `catalog`.
pub fn ledger_path(catalog: &str, name: &str) -> String {
    format!("{}/{name}", parent(catalog))
}

/// `target` resolved against the importing file's directory, or `None` when
/// it is absolute or climbs above the base.
fn import_target(importer: &str, target: &str) -> Option<String> {
    let target = target.replace('\\', "/");
    if target.starts_with('/') || target.contains(':') {
        return None;
    }
    let mut parts: Vec<&str> = importer.split('/').collect();
    parts.pop();
    for part in target.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop()?;
            }
            part => parts.push(part),
        }
    }
    Some(parts.join("/"))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Intent,
    Definition,
    Exception,
    Channel,
    Decision,
    Change,
}

const STATEMENTS: &[Kind] = &[
    Kind::Intent,
    Kind::Definition,
    Kind::Exception,
    Kind::Channel,
];

impl Kind {
    fn parse(text: &str) -> Option<Self> {
        match text {
            "intent" => Some(Self::Intent),
            "definition" => Some(Self::Definition),
            "exception" => Some(Self::Exception),
            "channel" => Some(Self::Channel),
            "decision" => Some(Self::Decision),
            "change" => Some(Self::Change),
            _ => None,
        }
    }

    const fn as_str(self) -> &'static str {
        match self {
            Self::Intent => "intent",
            Self::Definition => "definition",
            Self::Exception => "exception",
            Self::Channel => "channel",
            Self::Decision => "decision",
            Self::Change => "change",
        }
    }

    fn article(self) -> String {
        let kind = self.as_str();
        let article = if kind.starts_with(['a', 'e', 'i', 'o', 'u']) {
            "an"
        } else {
            "a"
        };
        format!("{article} {kind}")
    }
}

struct Declaration {
    kind: Kind,
    file: String,
    line: usize,
    in_catalog: bool,
}

/// Record every block's ID and kind, refusing duplicates across all files
/// declared so far, and anything but statements in a catalog.
fn declare(
    files: &[Loaded],
    in_catalog: bool,
    declared: &mut BTreeMap<String, Declaration>,
) -> Result<(), String> {
    for loaded in files {
        let path = loaded.file.path.as_str();
        for block in &loaded.file.blocks {
            let at = format!("{path}:{}", block.line);
            let kind = Kind::parse(&block.kind)
                .ok_or_else(|| format!("{at}: unknown kind {}", block.kind))?;
            if in_catalog && !STATEMENTS.contains(&kind) {
                return Err(format!(
                    "{at}: a catalog holds statements; {} {} belongs in a plan",
                    block.kind, block.name
                ));
            }
            let declaration = Declaration {
                kind,
                file: path.into(),
                line: block.line,
                in_catalog,
            };
            if let Some(other) = declared.insert(block.name.clone(), declaration) {
                return Err(format!(
                    "duplicate id {} in {}:{} and {at}",
                    block.name, other.file, other.line
                ));
            }
        }
    }
    Ok(())
}

fn catalog_statements(
    files: &[Loaded],
    declared: &BTreeMap<String, Declaration>,
) -> Result<BTreeMap<String, Statement>, String> {
    let mut statements = BTreeMap::new();
    for loaded in files {
        let links = Links { loaded, declared };
        for block in &loaded.file.blocks {
            let (statement, _) = links.statement(block, false)?;
            statements.insert(statement.id.clone(), statement);
        }
    }
    Ok(statements)
}

fn load_plan(reader: &dyn Reader, file: File) -> Result<Document, String> {
    if let Some(extra) = file.imports.get(1) {
        return Err(format!(
            "{}:{}: a plan imports one catalog",
            file.path, extra.line
        ));
    }
    let mut scopes = BTreeMap::new();
    let (catalog_root, catalog_files) = match file.imports.first() {
        Some(import) => {
            let at = format!("{}:{}", file.path, import.line);
            let target = import_target(&file.path, &import.path).ok_or_else(|| {
                format!(
                    "{at}: import {:?} must be a relative path that stays inside the repository",
                    import.path
                )
            })?;
            let root = read_file(reader, &target, Some(&at))?;
            if root.header != Header::Catalog {
                return Err(format!("{at}: {target} is not a catalog"));
            }
            scopes.insert(import.alias.clone(), Scope::Catalog);
            (Some(target), catalog_files(reader, root)?)
        }
        None => (None, Vec::new()),
    };
    let mut declared = BTreeMap::new();
    declare(&catalog_files, true, &mut declared)?;
    let catalog = catalog_statements(&catalog_files, &declared)?;
    let loaded = Loaded { file, scopes };
    declare(std::slice::from_ref(&loaded), false, &mut declared)?;

    let links = Links {
        loaded: &loaded,
        declared: &declared,
    };
    let mut plan = Plan {
        name: loaded.file.name.clone(),
        file: loaded.file.path.clone(),
        catalog: catalog_root,
        proposals: BTreeMap::new(),
        decisions: BTreeMap::new(),
        changes: BTreeMap::new(),
    };
    for block in &loaded.file.blocks {
        match declared[&block.name].kind {
            Kind::Decision => {
                plan.decisions
                    .insert(block.name.clone(), links.decision(block)?);
            }
            Kind::Change => {
                plan.changes
                    .insert(block.name.clone(), links.change(block)?);
            }
            _ => {
                let (statement, replaces) = links.statement(block, true)?;
                plan.proposals.insert(
                    statement.id.clone(),
                    Proposal {
                        statement,
                        replaces,
                    },
                );
            }
        }
    }
    Ok(Document {
        catalog,
        plan: Some(plan),
    })
}

/// Resolves the references of one file's blocks.
struct Links<'a> {
    loaded: &'a Loaded,
    declared: &'a BTreeMap<String, Declaration>,
}

impl Links<'_> {
    fn path(&self) -> &str {
        &self.loaded.file.path
    }

    fn at(&self, line: usize) -> String {
        format!("{}:{line}", self.path())
    }

    /// A bare ID names a block of the same file; `alias.id` names one in
    /// the file, or for a plan the catalog, that the alias imports.
    fn link(
        &self,
        field: &Field,
        value: &Value,
        kinds: &[Kind],
        what: &str,
        id: &str,
    ) -> Result<String, String> {
        let at = self.at(field.line);
        let (target, found) = match value {
            Value::Ident(target) => (
                target,
                self.declared
                    .get(target)
                    .filter(|found| found.file == self.path()),
            ),
            Value::Qualified(alias, target) => (
                target,
                match self.loaded.scopes.get(alias) {
                    Some(Scope::File(file)) => self
                        .declared
                        .get(target)
                        .filter(|found| &found.file == file),
                    Some(Scope::Catalog) => {
                        self.declared.get(target).filter(|found| found.in_catalog)
                    }
                    None => None,
                },
            ),
            _ => {
                return Err(format!("{at}: {} of {id} must be a reference", field.name));
            }
        };
        let found = found
            .ok_or_else(|| format!("{at}: unknown reference {value} in {} of {id}", field.name))?;
        if !kinds.contains(&found.kind) {
            return Err(format!(
                "{at}: {} of {id} must name {what}, but {value} is {}",
                field.name,
                found.kind.article()
            ));
        }
        Ok(target.clone())
    }

    /// A list of distinct references, sorted.
    fn links(
        &self,
        field: Option<&Field>,
        kinds: &[Kind],
        what: &str,
        id: &str,
    ) -> Result<Vec<String>, String> {
        let Some(field) = field else {
            return Ok(Vec::new());
        };
        let items = list(field, id, self.path())?;
        let mut targets = Vec::new();
        for item in items {
            let target = self.link(field, item, kinds, what, id)?;
            if targets.contains(&target) {
                return Err(format!(
                    "{}: {} of {id} names {target} twice",
                    self.at(field.line),
                    field.name
                ));
            }
            targets.push(target);
        }
        targets.sort();
        Ok(targets)
    }

    fn text(&self, field: &Field, id: &str) -> Result<String, String> {
        match &field.value {
            Value::Str(prose) if !prose.trim().is_empty() => Ok(prose.clone()),
            _ => Err(format!(
                "{}: {} of {id} must be a nonempty string",
                self.at(field.line),
                field.name
            )),
        }
    }

    fn statement(
        &self,
        block: &Block,
        proposed: bool,
    ) -> Result<(Statement, Option<String>), String> {
        let id = block.name.as_str();
        let kind = self.declared[id].kind;
        let mut allowed = match kind {
            Kind::Definition => vec!["text", "refines"],
            Kind::Exception => vec!["text", "narrows", "kind", "channels"],
            _ => vec!["text"],
        };
        if proposed {
            allowed.push("replaces");
        } else if matches!(kind, Kind::Definition | Kind::Exception) {
            allowed.push("realized_by");
        }
        let fields =
            block.fields_by_name(self.path(), &allowed, &format!("{} {id}", block.kind))?;
        let text = self.text(fields.required("text")?, id)?;
        let statement_kind = match kind {
            Kind::Definition => {
                let refines = fields.required("refines")?;
                StatementKind::Definition {
                    refines: self.link(
                        refines,
                        &refines.value,
                        &[Kind::Intent],
                        "an intent",
                        id,
                    )?,
                }
            }
            Kind::Exception => self.exception(&fields, id)?,
            Kind::Channel => StatementKind::Channel,
            _ => StatementKind::Intent,
        };
        let replaces = match fields.get("replaces") {
            Some(field) => {
                let target = self.link(field, &field.value, &[kind], &kind.article(), id)?;
                if !self.declared[&target].in_catalog {
                    return Err(format!(
                        "{}: replaces of {id} must name a catalog statement",
                        self.at(field.line)
                    ));
                }
                Some(target)
            }
            None => None,
        };
        let statement = Statement {
            id: id.into(),
            text,
            file: self.path().into(),
            line: block.line,
            kind: statement_kind,
            realized_by: self.anchors(fields.get("realized_by"), id)?,
        };
        Ok((statement, replaces))
    }

    fn exception(&self, fields: &Fields<'_>, id: &str) -> Result<StatementKind, String> {
        let narrows = fields.required("narrows")?;
        let narrows = self.link(
            narrows,
            &narrows.value,
            &[Kind::Definition],
            "a definition",
            id,
        )?;
        let kind_field = fields.required("kind")?;
        let kind = kind_field
            .value
            .as_ident()
            .and_then(ExceptionKind::parse)
            .ok_or_else(|| {
                format!(
                    "{}: exception {id} has unknown kind {}; expected does_not_apply or holds_instead",
                    self.at(kind_field.line),
                    kind_field.value
                )
            })?;
        let field = fields.required("channels")?;
        let channels = self.links(Some(field), &[Kind::Channel], "a channel", id)?;
        if channels.is_empty() {
            return Err(format!(
                "{}: exception {id} names no channels",
                self.at(field.line)
            ));
        }
        Ok(StatementKind::Exception {
            narrows,
            kind,
            channels,
        })
    }

    fn decision(&self, block: &Block) -> Result<Decision, String> {
        let id = block.name.as_str();
        let fields = block.fields_by_name(
            self.path(),
            &["text", "governs", "rejected"],
            &format!("decision {id}"),
        )?;
        let governs = fields.required("governs")?;
        let governs = self.links(Some(governs), STATEMENTS, "a statement", id)?;
        if governs.is_empty() {
            return Err(format!(
                "{}: decision {id} governs no statement",
                self.at(block.line)
            ));
        }
        Ok(Decision {
            id: id.into(),
            text: self.text(fields.required("text")?, id)?,
            line: block.line,
            governs,
            rejected: fields
                .get("rejected")
                .map(|field| self.text(field, id))
                .transpose()?,
        })
    }

    fn change(&self, block: &Block) -> Result<Change, String> {
        let id = block.name.as_str();
        let fields = block.fields_by_name(
            self.path(),
            &[
                "text", "serves", "follows", "after", "touches", "creates", "state",
            ],
            &format!("change {id}"),
        )?;
        let state = match fields.get("state") {
            Some(field) => field
                .value
                .as_ident()
                .and_then(State::parse)
                .ok_or_else(|| {
                    format!(
                        "{}: change {id} has unknown state {}; expected planned or done",
                        self.at(field.line),
                        field.value
                    )
                })?,
            None => State::Planned,
        };
        Ok(Change {
            id: id.into(),
            text: self.text(fields.required("text")?, id)?,
            line: block.line,
            serves: self.links(fields.get("serves"), STATEMENTS, "a statement", id)?,
            follows: self.links(fields.get("follows"), &[Kind::Decision], "a decision", id)?,
            after: self.links(fields.get("after"), &[Kind::Change], "a change", id)?,
            touches: self.anchors(fields.get("touches"), id)?,
            creates: self.anchors(fields.get("creates"), id)?,
            state,
        })
    }

    /// Code anchors: a symbol key `language:path#name` or a source path,
    /// relative to the repository.
    fn anchors(&self, field: Option<&Field>, id: &str) -> Result<Vec<String>, String> {
        let Some(field) = field else {
            return Ok(Vec::new());
        };
        let at = self.at(field.line);
        let mut anchors = Vec::new();
        for item in list(field, id, self.path())? {
            let Value::Str(anchor) = item else {
                return Err(format!("{at}: {} of {id} must list strings", field.name));
            };
            if !valid_anchor(anchor) {
                return Err(format!(
                    "{at}: {anchor:?} in {} of {id} is neither a symbol key (language:path#name) nor a repository-relative path",
                    field.name
                ));
            }
            if anchors.contains(anchor) {
                return Err(format!("{at}: {} of {id} names {anchor} twice", field.name));
            }
            anchors.push(anchor.clone());
        }
        anchors.sort();
        Ok(anchors)
    }
}

fn list<'a>(field: &'a Field, id: &str, path: &str) -> Result<&'a [Value], String> {
    match &field.value {
        Value::List(items) => Ok(items),
        _ => Err(format!(
            "{path}:{}: {} of {id} must be a list",
            field.line, field.name
        )),
    }
}

fn valid_anchor(anchor: &str) -> bool {
    let path = match anchor.split_once(':') {
        Some((language, symbol)) => {
            let Some((path, name)) = symbol.split_once('#') else {
                return false;
            };
            if language.is_empty()
                || !language.chars().all(|c| c.is_ascii_lowercase() || c == '_')
                || name.trim().is_empty()
            {
                return false;
            }
            path
        }
        None => anchor,
    };
    !path.is_empty()
        && !path.starts_with('/')
        && !path.contains(['\\', '#'])
        && path
            .split('/')
            .all(|part| !part.is_empty() && part != "." && part != "..")
}
