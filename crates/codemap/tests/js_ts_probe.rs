//! U1 compiler evidence and binding oracle; this is not a snapshot producer.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, ensure};
use codemap::schema::sha256_hex;
use serde_json::{Value, json};

#[path = "support/js_ts_oracle.rs"]
mod oracle;
use oracle::assert_oracle;

const HELPER: &[u8] = include_bytes!("../../../toolchain/js_ts/probe.mjs");
const CANDIDATES: &[u8] = include_bytes!("../../../toolchain/js_ts/collect_candidates.mjs");
const LOCK: &[u8] = include_bytes!("../../../toolchain/js_ts/package-lock.json");
const MANIFEST: &str = include_str!("../../../toolchain/js_ts/manifest.json");

struct Tools {
    node: PathBuf,
    bundle: PathBuf,
    node_sha256: String,
}

impl Tools {
    fn explicit() -> Result<Self> {
        let node = fs::canonicalize(
            std::env::var_os("CODEMAP_JS_TS_PROBE_NODE").context("explicit probe Node")?,
        )?;
        let bundle = fs::canonicalize(
            std::env::var_os("CODEMAP_JS_TS_PROBE_BUNDLE").context("explicit probe bundle")?,
        )?;
        let runtime: Value = serde_json::from_slice(&fs::read(bundle.join("runtime.json"))?)?;
        let tools = Self {
            node,
            bundle,
            node_sha256: runtime["node_sha256"]
                .as_str()
                .context("Node content pin")?
                .to_owned(),
        };
        tools.verify()?;
        let manifest: Value = serde_json::from_str(MANIFEST)?;
        let version = bounded_output(
            tools.command().arg("--version"),
            &[],
            Duration::from_secs(60),
        )?;
        tools.verify()?;
        ensure!(
            String::from_utf8(version)?.trim() == manifest["node_version"].as_str().unwrap(),
            "wrong pinned Node version"
        );
        Ok(tools)
    }

    fn verify(&self) -> Result<()> {
        ensure!(
            sha256_hex(&fs::read(&self.node)?) == self.node_sha256,
            "Node content drift"
        );
        ensure!(
            fs::read(self.bundle.join("probe.mjs"))? == HELPER,
            "helper content drift"
        );
        ensure!(
            fs::read(self.bundle.join("collect_candidates.mjs"))? == CANDIDATES,
            "candidate helper content drift"
        );
        ensure!(
            fs::read(self.bundle.join("package-lock.json"))? == LOCK,
            "compiler lock drift"
        );
        let manifest: Value = serde_json::from_str(MANIFEST)?;
        ensure!(
            sha256_hex(&fs::read(
                self.bundle
                    .join("node_modules/typescript/lib/typescript.js")
            )?) == manifest["compiler_sha256"].as_str().unwrap(),
            "compiler content drift"
        );
        Ok(())
    }

    fn command(&self) -> Command {
        let mut command = Command::new(&self.node);
        command.env_clear().arg("--no-addons");
        command
    }

    fn run(&self, root: &Path) -> Result<Value> {
        self.run_project(root, "tsconfig.json")
    }

    fn run_project(&self, root: &Path, project: &str) -> Result<Value> {
        self.verify()?;
        let root = fs::canonicalize(root)?;
        ensure!(
            !self.node.starts_with(&root) && !self.bundle.starts_with(&root),
            "tool bundle must be outside source"
        );
        let output = bounded_output(
            self.command()
                .arg("--max-old-space-size=4096")
                .arg(self.bundle.join("probe.mjs")),
            &serde_json::to_vec(&json!({"root": root, "project": project}))?,
            Duration::from_secs(60),
        );
        self.verify()?;
        Ok(serde_json::from_slice(&output?)?)
    }
}

fn bounded_output(command: &mut Command, input: &[u8], timeout: Duration) -> Result<Vec<u8>> {
    let scratch = tempfile::tempdir()?;
    let stdin = scratch.path().join("request.json");
    let stdout = scratch.path().join("stdout.json");
    let stderr = scratch.path().join("stderr.txt");
    fs::write(&stdin, input)?;
    let mut child = command
        .current_dir(scratch.path())
        .stdin(fs::File::open(stdin)?)
        .stdout(fs::File::create(&stdout)?)
        .stderr(fs::File::create(&stderr)?)
        .spawn()?;
    let deadline = Instant::now() + timeout;
    let outcome = (|| -> Result<_> {
        loop {
            ensure!(
                fs::metadata(&stdout)?.len() <= 16 * 1024 * 1024
                    && fs::metadata(&stderr)?.len() <= 1024 * 1024,
                "oversized tool response"
            );
            if let Some(status) = child.try_wait()? {
                ensure!(
                    fs::metadata(&stdout)?.len() <= 16 * 1024 * 1024
                        && fs::metadata(&stderr)?.len() <= 1024 * 1024,
                    "oversized tool response"
                );
                ensure!(
                    status.success(),
                    "compiler probe failed: {}",
                    fs::read_to_string(&stderr)?
                );
                return Ok(fs::read(&stdout)?);
            }
            ensure!(Instant::now() < deadline, "compiler probe timed out");
            std::thread::sleep(Duration::from_millis(10));
        }
    })();
    if outcome.is_err() {
        let _ = child.kill();
        child.wait()?;
    }
    outcome
}

fn fixture() -> tempfile::TempDir {
    let directory = tempfile::tempdir().unwrap();
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/js_ts_calls");
    for entry in fs::read_dir(source).unwrap() {
        let entry = entry.unwrap();
        fs::copy(entry.path(), directory.path().join(entry.file_name())).unwrap();
    }
    directory
}

#[test]
#[ignore = "requires explicit pinned JS/TS U1 probe Node and bundle"]
fn compiler_probe_matches_the_independent_binding_oracle() {
    let tools = Tools::explicit().unwrap();
    let first = fixture();
    let result = tools.run(first.path()).unwrap();
    assert_oracle(first.path(), &result);
    let mut wrong_body = result.clone();
    let overload = wrong_body["sites"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|site| site["symbol"] == "overload")
        .unwrap();
    overload["admission"]["target_body"] = overload["admission"]["signature"].clone();
    assert!(std::panic::catch_unwind(|| assert_oracle(first.path(), &wrong_body)).is_err());
    let second = fixture();
    let relocated = tools.run(second.path()).unwrap();
    assert_eq!(result["sites"], relocated["sites"]);
    let input = fs::canonicalize(first.path().join("target-execution.js")).unwrap();
    assert!(result["inputs"].get(input.to_str().unwrap()).is_some());
}

#[test]
#[ignore = "requires explicit pinned JS/TS U1 probe Node and bundle"]
fn compiler_probe_ignores_plugins_and_refuses_missing_modules() {
    let tools = Tools::explicit().unwrap();
    let source = fixture();
    let marker = source.path().join("PLUGIN_EXECUTED");
    fs::create_dir(source.path().join("plugins")).unwrap();
    fs::write(
        source.path().join("plugins/trap.cjs"),
        format!(
            "require('node:fs').writeFileSync({}, 'executed'); module.exports = {{}};",
            serde_json::to_string(&marker).unwrap()
        ),
    )
    .unwrap();
    let config = source.path().join("tsconfig.json");
    let mut value: Value = serde_json::from_slice(&fs::read(&config).unwrap()).unwrap();
    value["compilerOptions"]["plugins"] = json!([{"name":"./plugins/trap.cjs"}]);
    value["exclude"] = json!(["plugins"]);
    fs::write(config, serde_json::to_vec(&value).unwrap()).unwrap();
    let result = tools.run(source.path()).unwrap();
    assert_oracle(source.path(), &result);
    assert!(!marker.exists());
    fs::write(
        source.path().join("missing.ts"),
        "import { absent } from './not-present'; export function run() { return absent(); }",
    )
    .unwrap();
    assert!(
        tools
            .run(source.path())
            .unwrap_err()
            .to_string()
            .contains("invalid semantic program")
    );
}

#[test]
#[ignore = "requires explicit pinned JS/TS U1 probe Node and bundle"]
fn compiler_probe_rejects_redeclarations_and_resolves_nested_configs() {
    let tools = Tools::explicit().unwrap();
    let source = fixture();
    let config = source.path().join("tsconfig.json");
    let mut value: Value = serde_json::from_slice(&fs::read(&config).unwrap()).unwrap();
    value["compilerOptions"]["checkJs"] = json!(false);
    fs::write(&config, serde_json::to_vec(&value).unwrap()).unwrap();
    assert_oracle(source.path(), &tools.run(source.path()).unwrap());
    fs::create_dir(source.path().join("configs")).unwrap();
    fs::write(source.path().join("configs/tsconfig.json"),
        br#"{"extends":"../tsconfig.json", "include":["../*"], "compilerOptions":{"baseUrl":".."}}"#).unwrap();
    assert_oracle(
        source.path(),
        &tools
            .run_project(source.path(), "configs/tsconfig.json")
            .unwrap(),
    );
}

#[test]
#[ignore = "requires explicit pinned JS/TS U1 probe Node and bundle"]
fn compiler_probe_refuses_invalid_config_and_outside_inputs() {
    let tools = Tools::explicit().unwrap();
    let source = fixture();
    let config = source.path().join("tsconfig.json");
    let original = fs::read(&config).unwrap();
    let mut value: Value = serde_json::from_slice(&original).unwrap();
    value["references"] = json!([{"path":"./other"}]);
    fs::write(&config, serde_json::to_vec(&value).unwrap()).unwrap();
    assert!(tools.run(source.path()).is_err());
    fs::write(&config, b"{ invalid config").unwrap();
    assert!(tools.run(source.path()).is_err());
    fs::write(&config, &original).unwrap();
    fs::write(
        source.path().join("syntax.ts"),
        b"export function broken( {",
    )
    .unwrap();
    assert!(tools.run(source.path()).is_err());
    fs::remove_file(source.path().join("syntax.ts")).unwrap();
    assert!(
        tools
            .run_project(source.path(), "../tsconfig.json")
            .is_err()
    );
    let outside = tempfile::tempdir().unwrap();
    fs::write(
        outside.path().join("outside.ts"),
        b"export function outside() {}",
    )
    .unwrap();
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(
            outside.path().join("outside.ts"),
            source.path().join("escape.ts"),
        )
        .unwrap();
        assert!(tools.run(source.path()).is_err());
    }
    let altered = tempfile::tempdir().unwrap();
    fs::write(altered.path().join("probe.mjs"), b"drift").unwrap();
    let drifted = Tools {
        node: tools.node,
        bundle: altered.path().to_owned(),
        node_sha256: tools.node_sha256,
    };
    assert!(
        drifted
            .run(source.path())
            .unwrap_err()
            .to_string()
            .contains("helper content drift")
    );
}

#[test]
#[ignore = "requires explicit pinned JS/TS U1 probe Node and bundle"]
fn compiler_probe_fingerprints_changed_config_and_declarations() {
    let tools = Tools::explicit().unwrap();
    let source = fixture();
    let before = tools.run(source.path()).unwrap();
    for relative in ["tsconfig.json", "globals.d.ts"] {
        let path = source.path().join(relative);
        let mut bytes = fs::read(&path).unwrap();
        bytes.extend_from_slice(b"\n// changed input\n");
        fs::write(&path, bytes).unwrap();
    }
    let after = tools.run(source.path()).unwrap();
    assert_eq!(before["sites"], after["sites"]);
    for relative in ["tsconfig.json", "globals.d.ts"] {
        let path = fs::canonicalize(source.path().join(relative)).unwrap();
        let key = path.to_str().unwrap();
        assert_ne!(
            before["inputs"][key]["sha256"],
            after["inputs"][key]["sha256"]
        );
        assert_eq!(
            after["inputs"][key]["sha256"],
            sha256_hex(&fs::read(path).unwrap())
        );
    }
}

#[test]
#[ignore = "requires explicit pinned JS/TS U1 probe Node and bundle"]
fn compiler_probe_launches_are_bounded_even_on_failure() {
    let tools = Tools::explicit().unwrap();
    let started = Instant::now();
    let error = bounded_output(
        tools.command().args(["--eval", "for (;;) {}"]),
        &[],
        Duration::from_millis(100),
    )
    .unwrap_err();
    assert!(error.to_string().contains("timed out"));
    assert!(started.elapsed() < Duration::from_secs(5));
    let error = bounded_output(
        tools.command().args([
            "--eval",
            "process.stderr.write('x'.repeat(2*1024*1024)); process.exitCode=1;",
        ]),
        &[],
        Duration::from_secs(5),
    )
    .unwrap_err();
    assert!(error.to_string().contains("oversized tool response"));
    tools.verify().unwrap();
}

#[test]
#[ignore = "requires explicit pinned JS/TS U1 probe Node and bundle"]
fn compiler_probe_withholds_nodenext_commonjs_imports() {
    let tools = Tools::explicit().unwrap();
    let source = tempfile::tempdir().unwrap();
    fs::write(source.path().join("package.json"), br#"{"type":"module"}"#).unwrap();
    fs::write(source.path().join("tsconfig.json"), br#"{"compilerOptions":{"target":"ES2022","module":"NodeNext","moduleResolution":"NodeNext","strict":true},"include":["*.mts","*.cts"]}"#).unwrap();
    fs::write(source.path().join("base.cts"), b"export function f() { return 1; }\ndeclare const exports: any;\nexports.f = () => 2;\nexport function local() { return f(); }\n").unwrap();
    fs::write(
        source.path().join("entry.mts"),
        b"import { f } from './base.cjs';\nexport function run() { return f(); }\n",
    )
    .unwrap();
    let result = tools.run(source.path()).unwrap();
    assert_eq!(result["diagnostics"], json!([]));
    let sites = result["sites"].as_array().unwrap();
    assert_eq!(sites.len(), 2);
    let imported = sites
        .iter()
        .find(|site| site["location"]["path"] == "entry.mts")
        .unwrap();
    assert_eq!(
        imported["admission"]["reason"],
        "unsupported_commonjs_inter_module"
    );
    let local = sites
        .iter()
        .find(|site| site["location"]["path"] == "base.cts")
        .unwrap();
    assert_eq!(local["admission"]["reason"], "proven_binding_candidate");
    assert_eq!(local["admission"]["target_body"]["start_byte"], 20);
    assert_eq!(local["admission"]["target_body"]["end_byte"], 33);
}

#[test]
#[ignore = "requires explicit pinned JS/TS U1 probe Node and bundle"]
fn compiler_probe_rejects_swallowed_host_read_failures() {
    let tools = Tools::explicit().unwrap();
    let invalid = fixture();
    fs::write(invalid.path().join("unreadable.ts"), [0xff]).unwrap();
    let error = tools.run(invalid.path()).unwrap_err().to_string();
    assert!(error.contains("compiler host failure"), "{error}");
    assert!(error.contains("valid UTF-8"), "{error}");

    let oversized = fixture();
    fs::write(
        oversized.path().join("oversized.ts"),
        vec![b' '; 8 * 1024 * 1024 + 1],
    )
    .unwrap();
    let error = tools.run(oversized.path()).unwrap_err().to_string();
    assert!(error.contains("compiler host failure"), "{error}");
    assert!(error.contains("oversized input"), "{error}");
}

#[test]
#[ignore = "requires explicit pinned JS/TS U1 probe Node and bundle"]
fn compiler_probe_withholds_wrapped_dynamic_evaluation() {
    let tools = Tools::explicit().unwrap();
    let source = fixture();
    for callee in ["(eval)", "((eval))", "(eval as typeof eval)"] {
        fs::write(source.path().join("wrapped-eval.ts"), format!(
            "export function target() {{ return 1; }}\nexport function caller() {{ {callee}('target = () => 2'); return target(); }}\n"
        )).unwrap();
        let result = tools.run(source.path()).unwrap();
        let target = result["sites"]
            .as_array()
            .unwrap()
            .iter()
            .find(|site| {
                site["location"]["path"] == "wrapped-eval.ts" && site["callee"] == "target"
            })
            .unwrap();
        assert_eq!(target["admission"]["reason"], "dynamic_scope", "{callee}");
    }
}
