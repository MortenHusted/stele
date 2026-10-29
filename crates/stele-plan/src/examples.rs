//! Examples: tagged Cargo integration tests and tracked standalone Minitest
//! tests, each naming the statement it exemplifies and its channels. Discovery
//! never executes Ruby.

use std::{collections::BTreeMap, fs, io, path::Path, process::Command};

use syn::{Attribute, Expr, ExprLit, Item, ItemFn, ItemMod, Lit, Meta};

use crate::{ExceptionKind, Statement, StatementKind, digest::example_digest};

const TAG: &str = "intent: example <id> exemplifies <statement> on <channel>[, <channel>...]";

/// A package of the Cargo workspace at the repository root.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Package {
    /// As `cargo test -p` takes it.
    pub name: String,
    /// The package's directory, by its `/`-separated path from the
    /// repository root; empty for a package at the root.
    pub dir: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Example {
    pub id: String,
    pub statement: String,
    /// Sorted.
    pub channels: Vec<String>,
    /// The provider-specific execution candidate.
    pub execution: Execution,
    /// The test file, by its `/`-separated path from the repository root.
    pub file: String,
    /// Exact provider selector: `module::function` or `Class#method`.
    pub test: String,
    /// The tag's line in the file.
    pub line: usize,
    pub digest: String,
}

/// A source-addressed execution candidate; discovery never executes Ruby.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Execution {
    Cargo {
        package: String,
        target: String,
    },
    Minitest {
        class: String,
        method: String,
        definition_line: usize,
    },
}

impl Example {
    pub fn target(&self) -> &str {
        match &self.execution {
            Execution::Cargo { target, .. } => target,
            Execution::Minitest { .. } => &self.test,
        }
    }
}

/// A tagged test that is not an example, and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExampleIssue {
    /// `None` when the tag is too malformed to name one.
    pub example: Option<String>,
    pub file: String,
    pub test: String,
    pub line: usize,
    pub reason: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Scan {
    pub examples: Vec<Example>,
    pub issues: Vec<ExampleIssue>,
}

/// The packages of the Cargo workspace whose manifest is `Cargo.toml` at
/// `root`, as `cargo metadata` lists them, in directory order. A root with
/// no `Cargo.toml` has none.
pub fn packages(root: &Path) -> Result<Vec<Package>, String> {
    let manifest = root.join("Cargo.toml");
    if !manifest.is_file() {
        return Ok(Vec::new());
    }
    let output = Command::new("cargo")
        .args(["metadata", "--no-deps", "--format-version", "1"])
        .arg("--manifest-path")
        .arg(&manifest)
        .current_dir(root)
        .output()
        .map_err(|e| format!("cannot run cargo metadata in {}: {e}", root.display()))?;
    if !output.status.success() {
        return Err(format!(
            "`cargo metadata` failed in {}: {}",
            root.display(),
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    let metadata: serde_json::Value = serde_json::from_slice(&output.stdout)
        .map_err(|e| format!("cannot read what cargo metadata printed: {e}"))?;
    let canonical = |path: &Path| {
        path.canonicalize()
            .map_err(|e| format!("cannot read {}: {e}", path.display()))
    };
    let root = canonical(root)?;
    let mut packages = Vec::new();
    for package in metadata["packages"].as_array().into_iter().flatten() {
        let (Some(name), Some(manifest)) =
            (package["name"].as_str(), package["manifest_path"].as_str())
        else {
            return Err("cargo metadata listed a package without a name or manifest".into());
        };
        let dir = canonical(Path::new(manifest).parent().unwrap_or(&root))?;
        let dir = dir
            .strip_prefix(&root)
            .map_err(|_| format!("package {name} is outside {}", root.display()))?;
        packages.push(Package {
            name: name.into(),
            dir: dir
                .components()
                .map(|part| part.as_os_str().to_string_lossy())
                .collect::<Vec<_>>()
                .join("/"),
        });
    }
    packages.sort_by(|a, b| a.dir.cmp(&b.dir));
    Ok(packages)
}

/// What one test file holds: its tagged tests, and how many `#[test]` fns,
/// tagged or not, have each `module::function` path.
struct Collected<'a> {
    package: &'a str,
    file: String,
    tagged: Vec<Found>,
    paths: BTreeMap<String, usize>,
}

struct Found {
    test: String,
    line: usize,
    tag: String,
    digest: String,
}

/// Scan the `*.rs` files directly inside each package's `tests` directory,
/// in path order; a package without one has no examples. Tag problems become
/// issues. An unreadable file, a parse error or two tags claiming one
/// example ID fail, with locations given as `path:line` from `root`. Tracked
/// `test/**/*_test.rb` candidates share the same admission and ID namespace.
pub fn scan(
    root: &Path,
    packages: &[Package],
    catalog: &BTreeMap<String, Statement>,
) -> Result<Scan, String> {
    let mut files = Vec::new();
    for package in packages {
        let dir = if package.dir.is_empty() {
            "tests".to_string()
        } else {
            format!("{}/tests", package.dir)
        };
        for file in test_files(root, &dir)? {
            files.push((file, package.name.as_str()));
        }
    }
    files.sort_unstable();
    let mut scan = Scan::default();
    let mut claimed = BTreeMap::new();
    for (file, package) in files {
        let collected = read_tests(root, file, package)?;
        for found in &collected.tagged {
            admit(&collected, found, catalog, &mut claimed, &mut scan)?;
        }
    }
    crate::minitest::scan(root, catalog, &mut claimed, &mut scan)?;
    Ok(scan)
}

/// The `*.rs` files directly inside `dir`, by their paths from `root`.
fn test_files(root: &Path, dir: &str) -> Result<Vec<String>, String> {
    let unreadable = |e| format!("cannot read {dir}: {e}");
    let entries = match fs::read_dir(root.join(dir)) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        entries => entries.map_err(unreadable)?,
    };
    let mut files = Vec::new();
    for entry in entries {
        let path = entry.map_err(unreadable)?.path();
        if path.is_file() && path.extension().is_some_and(|extension| extension == "rs") {
            let name = path.file_name().unwrap_or_default().to_string_lossy();
            files.push(format!("{dir}/{name}"));
        }
    }
    Ok(files)
}

fn read_tests<'a>(root: &Path, file: String, package: &'a str) -> Result<Collected<'a>, String> {
    let source = fs::read_to_string(root.join(&file))
        .map_err(|e| format!("{file}: {e}"))?
        .replace("\r\n", "\n");
    let parsed =
        syn::parse_file(&source).map_err(|e| format!("{file}:{}: {e}", e.span().start().line))?;
    let mut collected = Collected {
        package,
        file,
        tagged: Vec::new(),
        paths: BTreeMap::new(),
    };
    let context: Vec<&Attribute> = parsed.attrs.iter().collect();
    collect(&parsed.items, "", &context, &mut collected);
    Ok(collected)
}

/// Add `found` to the scan as an example, or as an issue saying why it is
/// not one.
fn admit(
    collected: &Collected<'_>,
    found: &Found,
    catalog: &BTreeMap<String, Statement>,
    claimed: &mut BTreeMap<String, String>,
    scan: &mut Scan,
) -> Result<(), String> {
    let location = format!("{}:{}", collected.file, found.line);
    let issue = |example: Option<&str>, reason: String| ExampleIssue {
        example: example.map(Into::into),
        file: collected.file.clone(),
        test: found.test.clone(),
        line: found.line,
        reason,
    };
    let Some((id, statement, channels)) = parse_tag(&found.tag) else {
        let reason = format!("malformed tag {:?}; expected {TAG}", found.tag);
        scan.issues.push(issue(None, reason));
        return Ok(());
    };
    if let Some(other) = claimed.insert(id.clone(), location.clone()) {
        return Err(format!(
            "duplicate example id {id} in {other} and {location}"
        ));
    }
    if collected.paths[&found.test] > 1 {
        let reason = format!(
            "another test in {} has the path {}, so cargo test --exact cannot tell them apart",
            collected.file, found.test
        );
        scan.issues.push(issue(Some(&id), reason));
        return Ok(());
    }
    if let Some(reason) = cannot_exemplify(catalog, &statement, &channels) {
        scan.issues.push(issue(Some(&id), reason));
        return Ok(());
    }
    scan.examples.push(Example {
        id,
        statement,
        channels,
        execution: Execution::Cargo {
            package: collected.package.into(),
            target: Path::new(&collected.file)
                .file_stem()
                .unwrap()
                .to_string_lossy()
                .into_owned(),
        },
        file: collected.file.clone(),
        test: found.test.clone(),
        line: found.line,
        digest: found.digest.clone(),
    });
    Ok(())
}

/// `context` holds the attributes of the file and of every module around
/// `items`, outermost first: a `#[cfg]` there decides whether a test is
/// compiled at all, so it counts toward each example's digest.
fn collect(items: &[Item], module: &str, context: &[&Attribute], collected: &mut Collected<'_>) {
    for item in items {
        match item {
            Item::Fn(function) if is_test(function) => {
                let test = format!("{module}{}", function.sig.ident);
                *collected.paths.entry(test.clone()).or_default() += 1;
                let tags: Vec<_> = doc_lines(&function.attrs)
                    .into_iter()
                    .filter(|(_, text)| text.starts_with("intent:"))
                    .collect();
                if tags.is_empty() {
                    continue;
                }
                let digest = example_digest(context, function);
                for (line, tag) in tags {
                    collected.tagged.push(Found {
                        test: test.clone(),
                        line,
                        tag,
                        digest: digest.clone(),
                    });
                }
            }
            Item::Mod(ItemMod {
                attrs,
                ident,
                content: Some((_, items)),
                ..
            }) => {
                let context: Vec<&Attribute> = context.iter().copied().chain(attrs).collect();
                collect(items, &format!("{module}{ident}::"), &context, collected);
            }
            _ => {}
        }
    }
}

fn is_test(function: &ItemFn) -> bool {
    function
        .attrs
        .iter()
        .any(|attribute| attribute.path().is_ident("test"))
}

fn doc_lines(attributes: &[Attribute]) -> Vec<(usize, String)> {
    let mut lines = Vec::new();
    for attribute in attributes {
        let Meta::NameValue(meta) = &attribute.meta else {
            continue;
        };
        let Expr::Lit(ExprLit {
            lit: Lit::Str(text),
            ..
        }) = &meta.value
        else {
            continue;
        };
        if meta.path.is_ident("doc") {
            let first = attribute.pound_token.spans[0].start().line;
            for (offset, line) in text.value().lines().enumerate() {
                lines.push((first + offset, line.trim().to_string()));
            }
        }
    }
    lines
}

pub(crate) fn parse_tag(tag: &str) -> Option<(String, String, Vec<String>)> {
    let words: Vec<&str> = tag.split_whitespace().collect();
    let [
        "intent:",
        "example",
        id,
        "exemplifies",
        statement,
        "on",
        channels @ ..,
    ] = words.as_slice()
    else {
        return None;
    };
    let mut channels: Vec<String> = channels
        .join(" ")
        .split(',')
        .map(|channel| channel.trim().to_string())
        .collect();
    channels.sort();
    let distinct = channels.windows(2).all(|pair| pair[0] != pair[1]);
    let named = [id, statement]
        .into_iter()
        .copied()
        .chain(channels.iter().map(String::as_str))
        .all(is_identifier);
    (distinct && named).then(|| ((*id).to_string(), (*statement).to_string(), channels))
}

fn is_identifier(word: &str) -> bool {
    word.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_')
        && word.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

pub(crate) fn cannot_exemplify(
    catalog: &BTreeMap<String, Statement>,
    statement: &str,
    channels: &[String],
) -> Option<String> {
    let kind = |id: &str| catalog.get(id).map(|found| &found.kind);
    let scope = match kind(statement) {
        None => return Some(format!("unknown statement {statement}")),
        Some(StatementKind::Definition { .. }) => None,
        Some(StatementKind::Exception {
            kind: ExceptionKind::HoldsInstead,
            channels,
            ..
        }) => Some(channels),
        Some(other) => {
            return Some(format!(
                "{statement} is {}; an example exemplifies a definition or a holds_instead exception",
                described(other)
            ));
        }
    };
    channels.iter().find_map(|channel| match kind(channel) {
        None => Some(format!("unknown channel {channel}")),
        Some(StatementKind::Channel) => {
            let scope = scope.filter(|scope| !scope.contains(channel))?;
            Some(format!(
                "{statement} applies only on {}, not on {channel}",
                scope.join(", ")
            ))
        }
        Some(other) => Some(format!("{channel} is {}, not a channel", described(other))),
    })
}

fn described(kind: &StatementKind) -> String {
    match kind {
        StatementKind::Intent => "an intent".into(),
        StatementKind::Definition { .. } => "a definition".into(),
        StatementKind::Exception { kind, .. } => format!("a {} exception", kind.as_str()),
        StatementKind::Channel => "a channel".into(),
    }
}
