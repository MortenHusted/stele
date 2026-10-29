//! Standalone Minitest evidence profile: static direct subclasses only.
//! Discovery parses tracked source; only the runner executes trusted project code.
use crate::{
    Statement,
    digest::sha256,
    examples::{Example, ExampleIssue, Execution, Scan, cannot_exemplify, parse_tag},
};
use ruby_prism::Node;
use std::{collections::BTreeMap, fs, path::Path, process::Command};

struct Candidate {
    class: String,
    method: String,
    definition_line: usize,
    tag_line: usize,
}

fn text<'a>(source: &'a str, node: &Node<'_>) -> &'a str {
    &source[node.location().start_offset()..node.location().end_offset()]
}
fn line(source: &str, offset: usize) -> usize {
    source[..offset].bytes().filter(|b| *b == b'\n').count() + 1
}
fn constant(name: &str) -> bool {
    name.trim_start_matches("::").split("::").all(|part| {
        part.starts_with(|c: char| c.is_ascii_uppercase())
            && part.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
    })
}
fn qualified(namespace: &str, name: &str) -> String {
    if name.starts_with("::") || namespace.is_empty() {
        name.trim_start_matches("::").into()
    } else {
        format!("{namespace}::{name}")
    }
}

fn collect(
    node: &Node<'_>,
    source: &str,
    namespace: &str,
    class: Option<&str>,
    candidates: &mut Vec<Candidate>,
    classes: &mut BTreeMap<String, usize>,
) {
    match node {
        Node::ProgramNode { .. } => collect(
            &node.as_program_node().unwrap().statements().as_node(),
            source,
            namespace,
            class,
            candidates,
            classes,
        ),
        Node::StatementsNode { .. } => {
            for child in &node.as_statements_node().unwrap().body() {
                collect(&child, source, namespace, class, candidates, classes);
            }
        }
        Node::ModuleNode { .. } if class.is_none() => {
            let module = node.as_module_node().unwrap();
            let path = module.constant_path();
            let name = text(source, &path);
            if constant(name)
                && let Some(body) = module.body()
            {
                collect(
                    &body,
                    source,
                    &qualified(namespace, name),
                    None,
                    candidates,
                    classes,
                );
            }
        }
        Node::ClassNode { .. } if class.is_none() => {
            let definition = node.as_class_node().unwrap();
            let path = definition.constant_path();
            let name = text(source, &path);
            if !constant(name) {
                return;
            }
            let name = qualified(namespace, name);
            *classes.entry(name.clone()).or_default() += 1;
            let direct = definition.superclass().is_some_and(|parent| {
                matches!(text(source, &parent), "Minitest::Test" | "::Minitest::Test")
            });
            if !direct {
                return;
            }
            if let Some(body) = definition.body() {
                let Some(statements) = body.as_statements_node() else {
                    return;
                };
                if statements
                    .body()
                    .iter()
                    .any(|n| !matches!(n, Node::DefNode { .. }))
                {
                    return;
                }
                collect(&body, source, namespace, Some(&name), candidates, classes);
            }
        }
        Node::DefNode { .. } => {
            let Some(class) = class else {
                return;
            };
            let definition = node.as_def_node().unwrap();
            let method = String::from_utf8_lossy(definition.name().as_slice()).into_owned();
            let definition_line = line(source, definition.location().start_offset());
            if definition.receiver().is_none() && method.starts_with("test_") {
                candidates.push(Candidate {
                    class: class.into(),
                    method,
                    definition_line,
                    tag_line: definition_line - 1,
                });
            }
        }
        _ => {}
    }
}

struct TestFile {
    file: String,
    source: String,
    tags: BTreeMap<usize, String>,
    candidates: Vec<Candidate>,
}

fn read_test(
    root: &Path,
    file: String,
    tracked: bool,
    classes: &mut BTreeMap<String, usize>,
) -> Result<TestFile, String> {
    let mut path = root.to_path_buf();
    for part in Path::new(&file).components() {
        path.push(part);
        if fs::symlink_metadata(&path)
            .map_err(|e| format!("{file}: {e}"))?
            .file_type()
            .is_symlink()
        {
            return Err(format!("{file}: Minitest evidence refuses symlinks"));
        }
    }
    if !fs::metadata(&path)
        .map_err(|e| format!("{file}: {e}"))?
        .is_file()
    {
        return Err(format!("{file}: Minitest evidence requires a regular file"));
    }
    let source = fs::read_to_string(&path)
        .map_err(|e| format!("{file}: {e}"))?
        .replace("\r\n", "\n");
    let parsed = ruby_prism::parse(source.as_bytes());
    if let Some(error) = parsed.errors().next() {
        return Err(format!(
            "{file}:{}: invalid Ruby: {}",
            line(&source, error.location().start_offset()),
            error.message()
        ));
    }
    let tags: BTreeMap<usize, String> = parsed
        .comments()
        .filter_map(|comment| {
            let location = comment.location();
            let raw = source[location.start_offset()..location.end_offset()]
                .strip_prefix('#')?
                .trim();
            raw.starts_with("intent:")
                .then(|| (line(&source, location.start_offset()), raw.into()))
        })
        .collect();
    if !tags.is_empty() && !tracked {
        return Err(format!("{file}: tagged Minitest evidence must be tracked"));
    }
    let mut candidates = Vec::new();
    collect(&parsed.node(), &source, "", None, &mut candidates, classes);
    drop(parsed);
    Ok(TestFile {
        file,
        source,
        tags,
        candidates,
    })
}

fn git_paths(root: &Path, args: &[&str]) -> Result<Vec<String>, String> {
    let output = Command::new("git")
        .current_dir(root)
        .args(args)
        .output()
        .map_err(|e| e.to_string())?;
    if !output.status.success() {
        return Err("Minitest discovery requires a Git worktree".into());
    }
    String::from_utf8(output.stdout)
        .map_err(|e| e.to_string())
        .map(|s| {
            s.split('\0')
                .filter(|s| !s.is_empty())
                .map(str::to_owned)
                .collect()
        })
}

pub(crate) fn scan(
    root: &Path,
    catalog: &BTreeMap<String, Statement>,
    claimed: &mut BTreeMap<String, String>,
    scan: &mut Scan,
) -> Result<(), String> {
    if !root.join("test").exists() {
        return Ok(());
    }
    let tracked = git_paths(root, &["ls-files", "-z", "--", "test"])?;
    let mut files = tracked.clone();
    files.extend(git_paths(
        root,
        &[
            "ls-files",
            "-z",
            "--others",
            "--exclude-standard",
            "--",
            "test",
        ],
    )?);
    files.sort();
    files.dedup();
    let mut pending = Vec::new();
    let mut classes = BTreeMap::new();
    for file in files.into_iter().filter(|p| p.ends_with("_test.rb")) {
        let is_tracked = tracked.contains(&file);
        pending.push(read_test(root, file, is_tracked, &mut classes)?);
    }
    for TestFile {
        file,
        source,
        tags,
        candidates,
    } in pending
    {
        for (tag_line, tag) in tags {
            let location = format!("{file}:{tag_line}");
            let parsed = parse_tag(&tag);
            if let Some((id, _, _)) = &parsed
                && let Some(other) = claimed.insert(id.clone(), location.clone())
            {
                return Err(format!(
                    "duplicate example id {id} in {other} and {location}"
                ));
            }
            let standalone = source
                .lines()
                .nth(tag_line - 1)
                .is_some_and(|line| line.trim_start().starts_with('#'));
            let candidate = candidates
                .iter()
                .find(|candidate| standalone && candidate.tag_line == tag_line);
            let reason = match (&parsed, candidate) {
                (None, _) => Some("malformed intent example tag".into()),
                (_, None) => Some("unsupported Minitest form; expected a direct Minitest::Test subclass and immediately tagged def test_*".into()),
                (Some((_, statement, channels)), Some(candidate)) => {
                    if classes[&candidate.class] != 1 || candidates.iter().filter(|other| other.class == candidate.class && other.method == candidate.method).count() != 1 {
                        Some("ambiguous or reopened Minitest class/method".into())
                    } else { cannot_exemplify(catalog, statement, channels) }
                }
            };
            if let Some(reason) = reason {
                scan.issues.push(ExampleIssue {
                    example: parsed.map(|(id, _, _)| id),
                    file: file.clone(),
                    test: candidate
                        .map_or_else(String::new, |c| format!("{}#{}", c.class, c.method)),
                    line: tag_line,
                    reason,
                });
            } else {
                let (id, statement, channels) = parsed.unwrap();
                let candidate = candidate.unwrap();
                let test = format!("{}#{}", candidate.class, candidate.method);
                let digest = sha256(&format!(
                    "minitest-example/1\nprofile: standalone-direct/1\ntest: {test}\n{source}"
                ));
                scan.examples.push(Example {
                    id,
                    statement,
                    channels,
                    file: file.clone(),
                    test,
                    line: tag_line,
                    digest,
                    execution: Execution::Minitest {
                        class: candidate.class.clone(),
                        method: candidate.method.clone(),
                        definition_line: candidate.definition_line,
                    },
                });
            }
        }
    }
    scan.examples
        .sort_by(|a, b| (&a.file, a.line).cmp(&(&b.file, b.line)));
    Ok(())
}

use crate::{
    examples::Execution as Target,
    recorder::{CHANNEL_VAR, ExecutionResult, Runner},
    runs::Outcome,
};
use std::{
    path::PathBuf,
    process::Stdio,
    time::{Duration, Instant},
};

/// Executes trusted standalone tests on Unix with locally installed Minitest. This
/// does not evaluate Gemfiles, install dependencies or boot Rails. External
/// gems are runtime inputs, not source-addressed repository facts.
#[derive(Debug)]
pub struct Minitest {
    pub dir: PathBuf,
    pub timeout: Duration,
}

impl Minitest {
    pub const fn new(dir: PathBuf) -> Self {
        Self {
            dir,
            timeout: Duration::from_secs(60),
        }
    }

    pub fn command(
        &self,
        example: &Example,
        channel: &str,
        reporter: &Path,
        receipt: &Path,
    ) -> Result<Command, String> {
        self.validate(example)?;
        let Target::Minitest {
            class,
            method,
            definition_line,
        } = &example.execution
        else {
            unreachable!()
        };
        let mut command = Command::new("ruby");
        command
            .args(["-Ilib", "-Itest", "-r"])
            .arg(reporter)
            .arg(&example.file)
            .args(["--name", &format!("{class}#{method}")])
            .current_dir(&self.dir)
            .env(CHANNEL_VAR, channel)
            .env("MT_NO_PLUGINS", "1")
            .env("STELE_MINITEST_RECEIPT", receipt)
            .env("STELE_MINITEST_CLASS", class)
            .env("STELE_MINITEST_METHOD", method)
            .env("STELE_MINITEST_FILE", self.dir.join(&example.file))
            .env("STELE_MINITEST_LINE", definition_line.to_string());
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            command.process_group(0);
        }
        Ok(command)
    }
}

impl Runner for Minitest {
    fn validate(&self, example: &Example) -> Result<(), String> {
        if !cfg!(unix) {
            return Err(
                "standalone Minitest execution currently requires Unix process-group cleanup"
                    .into(),
            );
        }
        let Target::Minitest { class, method, .. } = &example.execution else {
            return Err(format!("example {} requires --runner cargo", example.id));
        };
        if ["config/application.rb", "config/database.yml"]
            .iter()
            .any(|path| fs::symlink_metadata(self.dir.join(path)).is_ok())
        {
            return Err("the standalone Minitest runner refuses Rails roots; Rails requires a separate execution profile".into());
        }
        let tracked = Command::new("git")
            .current_dir(&self.dir)
            .args(["ls-files", "--error-unmatch", "--", &example.file])
            .output()
            .map_err(|e| e.to_string())?;
        if !tracked.status.success() {
            return Err(format!("{} must remain tracked", example.file));
        }
        let mut path = self.dir.clone();
        for part in Path::new(&example.file).components() {
            if !matches!(part, std::path::Component::Normal(_)) {
                return Err("Minitest file must be repository-relative".into());
            }
            path.push(part);
            if fs::symlink_metadata(&path)
                .map_err(|e| e.to_string())?
                .file_type()
                .is_symlink()
            {
                return Err("Minitest evidence refuses symlinks".into());
            }
        }
        if !fs::metadata(&path).map_err(|e| e.to_string())?.is_file() {
            return Err("Minitest evidence requires a regular file".into());
        }
        let source = fs::read_to_string(path)
            .map_err(|e| e.to_string())?
            .replace("\r\n", "\n");
        let digest = sha256(&format!(
            "minitest-example/1\nprofile: standalone-direct/1\ntest: {class}#{method}\n{source}"
        ));
        if digest != example.digest {
            return Err(format!(
                "{} changed since discovery; no runs were recorded",
                example.file
            ));
        }
        Ok(())
    }

    fn run(&mut self, example: &Example, channel: &str) -> Result<ExecutionResult, String> {
        let temp = tempfile::tempdir().map_err(|e| e.to_string())?;
        let reporter = temp.path().join("reporter.rb");
        let receipt = temp.path().join("receipt.json");
        fs::write(&reporter, include_str!("runtime/minitest_reporter.rb"))
            .map_err(|e| e.to_string())?;
        let stdout = fs::File::create(temp.path().join("stdout")).map_err(|e| e.to_string())?;
        let stderr = fs::File::create(temp.path().join("stderr")).map_err(|e| e.to_string())?;
        let mut child = self
            .command(example, channel, &reporter, &receipt)?
            .stdin(Stdio::null())
            .stdout(stdout)
            .stderr(stderr)
            .spawn()
            .map_err(|e| format!("cannot run ruby: {e}"))?;
        let started = Instant::now();
        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break Some(status),
                Ok(None) => {}
                Err(error) => {
                    let cleanup = terminate_owned_run(&mut child);
                    return Err(match cleanup {
                        Ok(()) => format!("cannot wait for ruby: {error}"),
                        Err(cleanup) => format!("cannot wait for ruby: {error}; {cleanup}"),
                    });
                }
            }
            if started.elapsed() >= self.timeout {
                terminate_owned_run(&mut child)?;
                break None;
            }
            std::thread::sleep(Duration::from_millis(10));
        };
        let data = fs::read(&receipt).ok();
        Ok(receipt_result(
            data.as_deref(),
            status.and_then(|status| status.code()),
            example,
            &self.dir,
        ))
    }
}

#[cfg(unix)]
fn terminate_owned_run(child: &mut std::process::Child) -> Result<(), String> {
    let pid = libc::pid_t::try_from(child.id()).map_err(|error| error.to_string())?;
    // SAFETY: Command::process_group(0) gives this child its own process group.
    // The negative, nonzero child PID addresses only that owned group.
    let killed = unsafe { libc::kill(-pid, libc::SIGKILL) };
    let error = (killed == -1)
        .then(std::io::Error::last_os_error)
        .filter(|error| error.raw_os_error() != Some(libc::ESRCH));
    if error.is_some() {
        let _ = child.kill();
    }
    let reaped = child.wait();
    if let Some(error) = error {
        return Err(format!("cannot terminate ruby process group: {error}"));
    }
    reaped
        .map(|_| ())
        .map_err(|error| format!("cannot reap ruby: {error}"))
}

#[cfg(not(unix))]
fn terminate_owned_run(_: &mut std::process::Child) -> Result<(), String> {
    Err("standalone Minitest execution requires Unix process-group cleanup".into())
}

fn receipt_result(
    data: Option<&[u8]>,
    exit: Option<i32>,
    example: &Example,
    root: &Path,
) -> ExecutionResult {
    let error = || ExecutionResult {
        outcome: Outcome::Error,
        runner: Some("minitest/1 unavailable".into()),
    };
    let Some(data) = data else {
        return error();
    };
    let Ok(value) = serde_json::from_slice::<serde_json::Value>(data) else {
        return error();
    };
    if value["protocol"] != "minitest/1" || value["completed"] != true {
        return error();
    }
    let Some(version) = value["minitest"].as_str() else {
        return error();
    };
    let Some(gem) = value["minitest_gem"].as_str() else {
        return error();
    };
    if !reviewed_version(gem, version) {
        return error();
    }
    let Some(engine) = value["ruby_engine"].as_str().filter(|s| !s.is_empty()) else {
        return error();
    };
    let Some(ruby) = value["ruby_version"].as_str().filter(|s| !s.is_empty()) else {
        return error();
    };
    let Some(platform) = value["ruby_platform"].as_str().filter(|s| !s.is_empty()) else {
        return error();
    };
    let stamp = Some(format!(
        "minitest/1 ruby={engine}/{ruby}/{platform} gem={gem} minitest={version}"
    ));
    let outcome = (|| {
        let Target::Minitest {
            class,
            method,
            definition_line,
        } = &example.execution
        else {
            return Outcome::Error;
        };
        let Some(results) = value["results"].as_array().filter(|r| r.len() == 1) else {
            return Outcome::Error;
        };
        let result = &results[0];
        if result["class"].as_str() != Some(class) || result["method"].as_str() != Some(method) {
            return Outcome::Error;
        }
        let Some(location) = result["location"].as_array().filter(|l| l.len() == 2) else {
            return Outcome::Error;
        };
        let Some(file) = location[0].as_str() else {
            return Outcome::Error;
        };
        let actual = root.join(file).canonicalize();
        let expected = root.join(&example.file).canonicalize();
        if actual.is_err()
            || expected.is_err()
            || actual.ok() != expected.ok()
            || location[1].as_u64() != Some(*definition_line as u64)
        {
            return Outcome::Error;
        }
        let Some(failures) = result["failures"].as_array() else {
            return Outcome::Error;
        };
        if failures
            .iter()
            .any(|failure| !matches!(failure.as_str(), Some("assertion" | "skip")))
        {
            return Outcome::Error;
        }
        if failures.is_empty() {
            return if exit == Some(0) {
                Outcome::Pass
            } else {
                Outcome::Error
            };
        }
        if failures.iter().all(|failure| failure == "skip") {
            return if exit == Some(0) {
                Outcome::Skip
            } else {
                Outcome::Error
            };
        }
        if failures.iter().all(|failure| failure == "assertion") && exit == Some(1) {
            Outcome::Fail
        } else {
            Outcome::Error
        }
    })();
    ExecutionResult {
        outcome,
        runner: stamp,
    }
}

/// Legacy Cargo records need no stamp. New Ruby evidence requires the
/// reviewed receipt protocol and an actual runtime identity.
pub fn compatible_run(example: &Example, run: &crate::runs::Run) -> bool {
    let runner = run.runner.as_deref();
    if matches!(example.execution, Target::Minitest { .. })
        && run.outcome == Outcome::Error
        && runner == Some("minitest/1 unavailable")
    {
        return true;
    }
    match example.execution {
        Target::Cargo { .. } => runner.is_none(),
        Target::Minitest { .. } => runner.is_some_and(|stamp| {
            let fields: Vec<_> = stamp.split_whitespace().collect();
            match fields.as_slice() {
                ["minitest/1", runtime, gem, version] => {
                    runtime.strip_prefix("ruby=").is_some_and(|ruby| {
                        ruby.split('/').count() == 3 && ruby.split('/').all(|part| !part.is_empty())
                    }) && gem
                        .strip_prefix("gem=")
                        .zip(version.strip_prefix("minitest="))
                        .is_some_and(|(gem, version)| reviewed_version(gem, version))
                }
                _ => false,
            }
        }),
    }
}

// The published 5.27.0 archive declares VERSION 5.26.2. Preserve both
// observed identities rather than rewriting the runtime's reported version.
fn reviewed_version(gem: &str, runtime: &str) -> bool {
    matches!((gem, runtime), ("5.27.0", "5.26.2") | ("6.0.6", "6.0.6"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn receipts_require_one_completed_expected_result_and_consistent_status() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("sample.rb"), "def test_value; end\n").unwrap();
        let example = Example {
            id: "sample".into(),
            statement: "value".into(),
            channels: vec!["cli".into()],
            execution: Target::Minitest {
                class: "ValueTest".into(),
                method: "test_value".into(),
                definition_line: 1,
            },
            file: "sample.rb".into(),
            test: "ValueTest#test_value".into(),
            line: 1,
            digest: String::new(),
        };
        let valid = json!({"protocol": "minitest/1", "completed": true, "ruby_engine": "ruby", "ruby_version": "4.0.9",
            "ruby_platform": "test-platform", "minitest": "6.0.6", "minitest_gem": "6.0.6", "results": [{"class": "ValueTest", "method": "test_value", "location": ["sample.rb", 1], "failures": []}]});
        let outcome = |value: &serde_json::Value, exit| {
            receipt_result(
                Some(value.to_string().as_bytes()),
                exit,
                &example,
                dir.path(),
            )
            .outcome
        };
        assert_eq!(outcome(&valid, Some(0)), Outcome::Pass);
        for exit in [Some(1), Some(2), None] {
            assert_eq!(outcome(&valid, exit), Outcome::Error);
        }
        for (field, value) in [
            ("protocol", json!("minitest/2")),
            ("completed", json!(false)),
            ("minitest", json!("unknown")),
            ("results", json!([])),
            (
                "results",
                json!([valid["results"][0].clone(), valid["results"][0].clone()]),
            ),
            ("ruby_engine", json!(null)),
        ] {
            let mut changed = valid.clone();
            changed[field] = value;
            assert_eq!(outcome(&changed, Some(0)), Outcome::Error);
        }
        for (field, value) in [
            ("class", json!("OtherTest")),
            ("method", json!("test_value_suffix")),
            ("location", json!(["sample.rb", 2])),
            ("location", json!(["missing.rb", 1])),
            ("failures", json!(["error"])),
            ("failures", json!(["assertion", "error"])),
            ("failures", json!(["skip", "assertion"])),
            ("failures", json!(["unknown"])),
        ] {
            let mut changed = valid.clone();
            changed["results"][0][field] = value;
            for exit in [Some(0), Some(1)] {
                assert_eq!(outcome(&changed, exit), Outcome::Error);
            }
        }
        let mut failed = valid.clone();
        failed["results"][0]["failures"] = json!(["assertion"]);
        assert_eq!(outcome(&failed, Some(1)), Outcome::Fail);
        assert_eq!(outcome(&failed, Some(0)), Outcome::Error);
        let mut skipped = valid;
        skipped["results"][0]["failures"] = json!(["skip"]);
        assert_eq!(outcome(&skipped, Some(0)), Outcome::Skip);
        assert_eq!(outcome(&skipped, Some(1)), Outcome::Error);
        assert_eq!(
            receipt_result(None, Some(0), &example, dir.path()).outcome,
            Outcome::Error
        );
        for raw in [
            "{}{}",
            "not json",
            "1 runs, 1 assertions, 0 failures, 0 errors",
            "test result: ok. 1 passed; 0 failed; 0 ignored",
        ] {
            assert_eq!(
                receipt_result(Some(raw.as_bytes()), Some(0), &example, dir.path()).outcome,
                Outcome::Error
            );
        }
    }
}
