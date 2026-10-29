//! The run ledger beside a catalog root: one entry per example run on one
//! channel, appended by the recorder and never rewritten.

use std::{
    fs::OpenOptions,
    io::{self, Read, Seek, SeekFrom, Write},
    path::Path,
};

use crate::{
    Reader,
    load::read_or_empty,
    syntax::{Field, Value, parse_ledger, quote},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Pass,
    Fail,
    Error,
    Skip,
}

impl Outcome {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Pass => "pass",
            Self::Fail => "fail",
            Self::Error => "error",
            Self::Skip => "skip",
        }
    }

    fn parse(text: &str) -> Option<Self> {
        match text {
            "pass" => Some(Self::Pass),
            "fail" => Some(Self::Fail),
            "error" => Some(Self::Error),
            "skip" => Some(Self::Skip),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Run {
    pub example: String,
    /// The example's digest when it ran.
    pub digest: String,
    pub digest_version: u64,
    pub channel: String,
    /// The source identity it ran at.
    pub source: String,
    /// The commit HEAD pointed to, abbreviated or full, or an explicitly
    /// tagged `source:sha256:…` baseline recorded against that source tree.
    pub base: String,
    pub outcome: Outcome,
    /// For a failing run only: the tracked-file diff against `base`, without
    /// the two ledgers.
    pub diff: Option<String>,
    /// Absent records retain legacy Cargo interpretation.
    pub runner: Option<String>,
}

const FIELDS: [&str; 9] = [
    "example",
    "digest",
    "digest_version",
    "channel",
    "source",
    "base",
    "outcome",
    "diff",
    "runner",
];

/// The source identity of an explicitly tagged baseline, or `None` for a
/// legacy commit base. Reserved source tags must contain a full SHA-256.
pub fn source_baseline(base: &str) -> Result<Option<&str>, String> {
    let Some(identity) = base.strip_prefix("source:") else {
        return Ok(None);
    };
    let valid = identity.strip_prefix("sha256:").is_some_and(|digest| {
        digest.len() == 64
            && digest
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    });
    if !valid {
        return Err(
            "a source baseline must be source:sha256: followed by 64 lowercase hexadecimal digits"
                .into(),
        );
    }
    Ok(Some(identity))
}

/// Parse and check the runs of the ledger at `path`. Errors read
/// `path:line: message`.
pub fn parse(source: &str, path: &str) -> Result<Vec<Run>, String> {
    let mut runs = Vec::new();
    for record in &parse_ledger(source, path)? {
        let at = format!("{path}:{}", record.line);
        if record.kind != "run" {
            return Err(format!(
                "{at}: unknown block {}; the run ledger holds only run blocks",
                record.kind
            ));
        }
        let fields = record.fields_by_name(path, &FIELDS, "run")?;
        let identifier = |name: &str| match fields.required(name)? {
            Field {
                value: Value::Ident(value),
                ..
            } => Ok(value.clone()),
            field => Err(format!(
                "{path}:{}: {name} must be an identifier",
                field.line
            )),
        };
        let string = |field: &Field| match &field.value {
            Value::Str(value) => Ok(value.clone()),
            _ => Err(format!(
                "{path}:{}: {} must be a string",
                field.line, field.name
            )),
        };
        let outcome_field = fields.required("outcome")?;
        let outcome = outcome_field
            .value
            .as_ident()
            .and_then(Outcome::parse)
            .ok_or_else(|| {
                format!(
                    "{path}:{}: unknown outcome {}; expected pass, fail, error or skip",
                    outcome_field.line, outcome_field.value
                )
            })?;
        let diff = match (fields.get("diff"), outcome) {
            (Some(field), Outcome::Fail) => Some(string(field)?),
            (None, Outcome::Fail) => return Err(format!("{at}: a failing run needs diff")),
            (Some(field), _) => {
                return Err(format!(
                    "{path}:{}: only a failing run stores a diff",
                    field.line
                ));
            }
            (None, _) => None,
        };
        let version = fields.required("digest_version")?;
        let Value::Int(digest_version) = version.value else {
            return Err(format!(
                "{path}:{}: digest_version must be an integer",
                version.line
            ));
        };
        let base = string(fields.required("base")?)?;
        source_baseline(&base).map_err(|error| format!("{at}: {error}"))?;
        runs.push(Run {
            example: identifier("example")?,
            digest: string(fields.required("digest")?)?,
            digest_version,
            channel: identifier("channel")?,
            source: string(fields.required("source")?)?,
            base,
            outcome,
            diff,
            runner: fields.get("runner").map(string).transpose()?,
        });
    }
    Ok(runs)
}

/// Read the ledger at `path`; a missing ledger holds no runs.
pub fn read(reader: &dyn Reader, path: &str) -> Result<Vec<Run>, String> {
    parse(&read_or_empty(reader, path)?, path)
}

/// Append `runs` to the ledger at `path`, creating it if needed. The existing
/// bytes are never rewritten, so they stay a prefix of the new ledger.
pub fn append(path: &Path, runs: &[Run]) -> Result<(), String> {
    let failed = |e: io::Error| format!("cannot append to {}: {e}", path.display());
    let mut file = OpenOptions::new()
        .read(true)
        .append(true)
        .create(true)
        .open(path)
        .map_err(failed)?;
    let mut text = String::new();
    if file.metadata().map_err(failed)?.len() > 0 {
        let mut last = [0];
        file.seek(SeekFrom::End(-1)).map_err(failed)?;
        file.read_exact(&mut last).map_err(failed)?;
        if last != *b"\n" {
            text.push('\n');
        }
    }
    for run in runs {
        text.push_str(&render(run));
    }
    file.write_all(text.as_bytes()).map_err(failed)
}

fn render(run: &Run) -> String {
    let mut text = format!(
        "run {{\n  example: {}\n  digest: {}\n  digest_version: {}\n  channel: {}\n  source: {}\n  base: {}\n  outcome: {}\n",
        run.example,
        quote(&run.digest),
        run.digest_version,
        run.channel,
        quote(&run.source),
        quote(&run.base),
        run.outcome.as_str(),
    );
    if let Some(runner) = &run.runner {
        text.push_str("  runner: ");
        text.push_str(&quote(runner));
        text.push('\n');
    }
    if let Some(diff) = &run.diff {
        text.push_str("  diff: ");
        text.push_str(&quote(diff));
        text.push('\n');
    }
    text.push_str("}\n");
    text
}
