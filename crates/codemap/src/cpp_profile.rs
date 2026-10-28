//! One C++ compilation profile.
//!
//! Semantic identity follows the profile, not the machine it was prepared
//! on. Declared source, build, dependency, and sysroot roots become logical
//! names. Parse-input paths outside those roots are rejected; a macOS SDK
//! and a Linux sysroot remain different declared inputs.
//!
//! The fingerprint covers normalized commands, working directories, and all
//! translation units. Header and sysroot contents are not hashed yet, so
//! this is not an input-closure proof.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Component, Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use crate::schema::sha256_hex;

/// Roots the operator declares for one profile.
///
/// `source` is the checkout whose translation units can become snapshot
/// files. `build`, `sysroot`, and `dependencies` are inputs. A path that
/// sits under none of them is unmanifested and rejected.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProfileRoots {
    pub source: PathBuf,
    pub build: Option<PathBuf>,
    pub sysroot: Option<PathBuf>,
    pub dependencies: Vec<(String, PathBuf)>,
}

/// Application translation units and the profile fingerprint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompilationProfile {
    /// Repo-relative path to the normalized command, for files under `source`.
    pub application_units: BTreeMap<String, CompilationCommand>,
    /// SHA-256 of normalized commands, working directories, and source bytes.
    pub fingerprint: String,
}

/// A normalized command retains its working directory and argv boundaries.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CompilationCommand {
    pub directory: String,
    pub arguments: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct DatabaseEntry {
    directory: String,
    file: String,
    command: Option<String>,
    arguments: Option<Vec<String>>,
}

#[derive(Debug, Serialize)]
struct Row {
    command: CompilationCommand,
    source_sha256: String,
}

/// Load `compile_commands.json` without executing any recorded command.
pub fn load_profile(database: &Path, roots: &ProfileRoots) -> Result<CompilationProfile> {
    let text = fs::read_to_string(database)
        .with_context(|| format!("read compilation database {}", database.display()))?;
    let entries: Vec<DatabaseEntry> =
        serde_json::from_str(&text).context("parse compilation database")?;
    if entries.is_empty() {
        bail!(
            "compilation database {} has no translation units",
            database.display()
        );
    }
    let logical_roots = logical_roots(roots)?;
    let mut rows: BTreeMap<String, Row> = BTreeMap::new();
    let mut application_units = BTreeMap::new();

    for entry in entries {
        let directory = PathBuf::from(&entry.directory);
        if !directory.is_absolute() {
            bail!("compilation command directory must be absolute");
        }
        let logical_directory = map_path(&directory, &logical_roots)
            .context("compilation command directory is outside the declared profile roots")?;
        let file = resolve_file(&directory, &entry.file);
        let logical = map_path(&file, &logical_roots).with_context(|| {
            format!(
                "translation unit {} is outside the declared profile roots",
                file.display()
            )
        })?;
        let tokens = entry_tokens(&entry, &directory, &file, &logical_roots)?;
        let command = CompilationCommand {
            directory: logical_directory,
            arguments: tokens,
        };
        if let Some(previous) = rows.get(&logical) {
            if previous.command != command {
                bail!("translation unit {logical} has conflicting compilation commands");
            }
            continue;
        }
        // The longest declared root wins, so a dependency checked out inside
        // the source tree stays a dependency input.
        let bytes = fs::read(&file).with_context(|| format!("read {}", file.display()))?;
        let source_sha256 = sha256_hex(&bytes);
        if let Some(rel) = logical.strip_prefix("{source}/") {
            application_units.insert(rel.to_string(), command.clone());
        }
        rows.insert(
            logical,
            Row {
                command,
                source_sha256,
            },
        );
    }

    Ok(CompilationProfile {
        application_units,
        fingerprint: fingerprint(&rows),
    })
}

pub fn logical_roots(roots: &ProfileRoots) -> Result<Vec<(&str, &Path)>> {
    let mut named = vec![("source", roots.source.as_path())];
    if let Some(build) = &roots.build {
        named.push(("build", build.as_path()));
    }
    if let Some(sysroot) = &roots.sysroot {
        named.push(("sysroot", sysroot.as_path()));
    }
    for (name, path) in &roots.dependencies {
        if name.is_empty()
            || !name
                .chars()
                .all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
            || matches!(name.as_str(), "source" | "build" | "sysroot")
        {
            bail!("dependency root name `{name}` is not a logical name");
        }
        named.push((name.as_str(), path.as_path()));
    }
    let mut seen = BTreeSet::new();
    let mut paths = BTreeSet::new();
    for (name, path) in &named {
        if !path.is_absolute() {
            bail!("profile root `{name}` must be absolute");
        }
        if !seen.insert(*name) {
            bail!("profile root `{name}` is declared more than once");
        }
        if !paths.insert(normalize_path(path)) {
            bail!("profile roots must not name the same directory");
        }
    }
    Ok(named)
}

fn entry_tokens(
    entry: &DatabaseEntry,
    directory: &Path,
    file: &Path,
    logical_roots: &[(&str, &Path)],
) -> Result<Vec<String>> {
    let raw = if let Some(arguments) = &entry.arguments {
        if arguments.is_empty() {
            bail!("compilation command for {} is empty", entry.file);
        }
        arguments.clone()
    } else if let Some(command) = &entry.command {
        split_command(command)
            .with_context(|| format!("split compilation command for {}", entry.file))?
    } else {
        bail!(
            "compilation command for {} has neither command nor arguments",
            entry.file
        );
    };
    let expanded = expand_response_files(raw, directory, logical_roots, 0)?;
    for token in &expanded {
        for (name, _) in logical_roots {
            if token.contains(&format!("{{{name}}}")) {
                bail!("compilation command contains reserved logical root {{{name}}}");
            }
        }
    }
    normalize_arguments(&expanded, directory, file, logical_roots)
}

fn resolve_file(directory: &Path, file: &str) -> PathBuf {
    let path = PathBuf::from(file);
    if path.is_absolute() {
        path
    } else {
        directory.join(path)
    }
}

fn relative_to(path: &Path, root: &Path) -> Option<String> {
    path.strip_prefix(root).ok().map(|rest| {
        rest.components()
            .map(|component| component.as_os_str().to_string_lossy())
            .collect::<Vec<_>>()
            .join("/")
    })
}

pub fn map_path(path: &Path, roots: &[(&str, &Path)]) -> Option<String> {
    if !path.is_absolute() {
        return None;
    }
    let path = normalize_path(path);
    let mut best: Option<(usize, &str, String)> = None;
    for (name, root) in roots {
        let root = normalize_path(root);
        if let Some(rest) = relative_to(&path, &root) {
            let len = root.as_os_str().len();
            let replace = best.as_ref().is_none_or(|(best_len, _, _)| len > *best_len);
            if replace {
                best = Some((len, name, rest));
            }
        }
    }
    best.map(|(_, name, rest)| {
        if rest.is_empty() {
            format!("{{{name}}}")
        } else {
            format!("{{{name}}}/{rest}")
        }
    })
}

fn normalize_path(path: &Path) -> PathBuf {
    if let Ok(canonical) = fs::canonicalize(path) {
        return canonical;
    }
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            other => normalized.push(other),
        }
    }
    for ancestor in normalized.ancestors() {
        if let Ok(canonical) = fs::canonicalize(ancestor) {
            return canonical.join(normalized.strip_prefix(ancestor).expect("path ancestor"));
        }
    }
    normalized
}

fn normalize_arguments(
    tokens: &[String],
    directory: &Path,
    file: &Path,
    roots: &[(&str, &Path)],
) -> Result<Vec<String>> {
    let compiler = tokens.first().context("compilation command is empty")?;
    let compiler_name = Path::new(compiler)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default();
    if !matches!(compiler_name, "c++" | "g++" | "gcc" | "clang++" | "clang") {
        bail!("unsupported compiler driver `{compiler_name}`; prepare a command without wrappers");
    }
    if tokens
        .iter()
        .any(|token| token.contains('\0') || token.is_empty())
    {
        bail!("compilation command contains an empty argument or NUL byte");
    }
    let mut normalized =
        vec![map_path(Path::new(compiler), roots).unwrap_or_else(|| compiler.clone())];
    let mut arguments = tokens[1..].iter();
    let expected_file = map_path(file, roots).context("unmanifested translation unit")?;
    let mut has_source = false;
    let mut options = true;
    while let Some(token) = arguments.next() {
        if options && token == "--" {
            normalized.push(token.clone());
            options = false;
            continue;
        }
        if !options || !token.starts_with('-') {
            let source = normalize_argument_path(token, directory, roots)?;
            if has_source || source != expected_file {
                bail!("compilation command must name exactly its declared translation unit");
            }
            normalized.push(source);
            has_source = true;
            continue;
        }
        if let Some((prefix, value)) = path_option(token) {
            let value = match value {
                Some(value) => value,
                None => arguments
                    .next()
                    .with_context(|| format!("{token} requires a path"))?,
            };
            if matches!(prefix, "-include" | "-imacros") && !Path::new(value).is_absolute() {
                bail!("{prefix} requires an absolute path; prepare the forced header explicitly");
            }
            let path = normalize_argument_path(value, directory, roots)?;
            if token == prefix {
                normalized.extend([token.clone(), path]);
            } else {
                normalized.push(format!("{prefix}{path}"));
            }
            continue;
        }
        if matches!(
            token.as_str(),
            "-D" | "-U" | "-x" | "-target" | "--target" | "-arch"
        ) {
            let value = arguments
                .next()
                .with_context(|| format!("{token} requires a value"))?;
            normalized.extend([token.clone(), value.clone()]);
            continue;
        }
        if !supported_flag(token) {
            bail!(
                "unsupported compilation flag `{token}`; prepare a profile using supported flags"
            );
        }
        normalized.push(token.clone());
    }
    if !has_source {
        bail!("compilation command does not name its declared translation unit");
    }
    Ok(normalized)
}

fn supported_flag(token: &str) -> bool {
    matches!(
        token,
        "-c" | "-fsyntax-only"
            | "--no-default-config"
            | "-nostdinc"
            | "-nostdinc++"
            | "-fPIC"
            | "-fpic"
            | "-fPIE"
            | "-fpie"
            | "-fno-omit-frame-pointer"
            | "-fomit-frame-pointer"
            | "-fexceptions"
            | "-fno-exceptions"
            | "-frtti"
            | "-fno-rtti"
            | "-pthread"
            | "-pedantic"
            | "-pedantic-errors"
            | "-g"
            | "-g0"
            | "-g1"
            | "-g2"
            | "-g3"
            | "-O0"
            | "-O1"
            | "-O2"
            | "-O3"
            | "-Og"
            | "-Os"
            | "-Oz"
            | "-Ofast"
            | "-m32"
            | "-m64"
    ) || token.starts_with("-D")
        || token.starts_with("-U")
        || [
            "-std=",
            "--target=",
            "-target=",
            "-stdlib=",
            "-fsanitize=",
            "-fno-sanitize=",
            "-march=",
            "-mcpu=",
            "-mtune=",
            "-mmacosx-version-min=",
        ]
        .iter()
        .any(|prefix| token.starts_with(prefix) && token.len() > prefix.len())
        || (token.starts_with("-W")
            && !["-Wl,", "-Wp,", "-Wa,"]
                .iter()
                .any(|prefix| token.starts_with(prefix)))
}

fn normalize_argument_path(
    value: &str,
    directory: &Path,
    roots: &[(&str, &Path)],
) -> Result<String> {
    if value == "-" {
        bail!("compilation path `-` is unsupported; prepare an explicit path");
    }
    if value.starts_with('=') || value.starts_with("$SYSROOT") {
        bail!("sysroot-relative compilation paths are unsupported; prepare explicit paths");
    }
    map_path(&resolve_file(directory, value), roots).with_context(|| {
        format!("compilation path `{value}` is outside the declared profile roots")
    })
}

pub fn path_option(token: &str) -> Option<(&str, Option<&str>)> {
    const OPTIONS: &[&str] = &[
        "--sysroot",
        "-resource-dir",
        "-isystem",
        "-iframework",
        "-cxx-isystem",
        "-isysroot",
        "-idirafter",
        "-iquote",
        "-imacros",
        "-include",
        "-I",
        "-o",
    ];
    for prefix in OPTIONS {
        if token == *prefix {
            return Some((prefix, None));
        }
        let joined = if matches!(*prefix, "--sysroot" | "-resource-dir") {
            token
                .strip_prefix(prefix)
                .and_then(|rest| rest.strip_prefix('='))
        } else if matches!(*prefix, "-I" | "-iquote" | "-isysroot") {
            token.strip_prefix(prefix)
        } else {
            None
        };
        if let Some(value) = joined.filter(|value| !value.is_empty()) {
            let length = token.len() - value.len();
            return Some((&token[..length], Some(value)));
        }
    }
    None
}

fn expand_response_files(
    tokens: Vec<String>,
    directory: &Path,
    logical_roots: &[(&str, &Path)],
    depth: u8,
) -> Result<Vec<String>> {
    if depth > 4 {
        bail!("response file nesting is deeper than 4");
    }
    let mut expanded = Vec::with_capacity(tokens.len());
    for token in tokens {
        let Some(path) = token.strip_prefix('@') else {
            expanded.push(token);
            continue;
        };
        if path.is_empty() {
            bail!("compilation command names an empty response file");
        }
        let full = resolve_file(directory, path);
        if map_path(&full, logical_roots).is_none() {
            bail!(
                "response file {} is outside the declared profile roots",
                full.display()
            );
        }
        let text = fs::read_to_string(&full)
            .with_context(|| format!("read response file {}", full.display()))?;
        let inner = split_command(&text)
            .with_context(|| format!("split response file {}", full.display()))?;
        expanded.extend(expand_response_files(
            inner,
            directory,
            logical_roots,
            depth + 1,
        )?);
    }
    Ok(expanded)
}

fn fingerprint(rows: &BTreeMap<String, Row>) -> String {
    let material = serde_json::to_vec(rows).expect("compilation profile contains only strings");
    sha256_hex(&material)
}

fn split_command(command: &str) -> Result<Vec<String>> {
    let mut args = Vec::new();
    let mut current = String::new();
    let mut chars = command.chars();
    let mut in_token = false;
    let mut quote: Option<char> = None;
    while let Some(ch) = chars.next() {
        if let Some(active) = quote {
            if ch == active {
                quote = None;
            } else if ch == '\\' && active == '"' {
                let Some(escaped) = chars.next() else {
                    bail!("compilation command ends with a trailing escape");
                };
                if !matches!(escaped, '$' | '`' | '"' | '\\' | '\n') {
                    current.push('\\');
                }
                if escaped == '\n' {
                    continue;
                }
                current.push(escaped);
            } else {
                current.push(ch);
            }
            in_token = true;
            continue;
        }
        match ch {
            '\\' => {
                let Some(escaped) = chars.next() else {
                    bail!("compilation command ends with a trailing escape");
                };
                current.push(escaped);
                in_token = true;
            }
            '"' | '\'' => {
                quote = Some(ch);
                in_token = true;
            }
            ch if ch.is_whitespace() => {
                if in_token {
                    args.push(std::mem::take(&mut current));
                    in_token = false;
                }
            }
            _ => {
                current.push(ch);
                in_token = true;
            }
        }
    }
    if quote.is_some() {
        bail!("compilation command has an unterminated quote");
    }
    if in_token {
        args.push(current);
    }
    if args.is_empty() {
        bail!("compilation command is empty");
    }
    Ok(args)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_unit(dir: &Path, rel: &str, body: &str) -> PathBuf {
        let path = dir.join(rel);
        fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        fs::write(&path, body).expect("write unit");
        path
    }

    fn database(dir: &Path, entries: &str) -> PathBuf {
        let path = dir.join("compile_commands.json");
        fs::write(&path, entries).expect("write database");
        path
    }

    #[test]
    fn unsupported_flags_and_wrappers_fail_closed() {
        let root = tempfile::tempdir().expect("root");
        let unit = write_unit(root.path(), "unit.cpp", "int value() { return 1; }\n");
        let roots = [("source", root.path())];
        for flag in [
            "-Xclang",
            "-Xclang=-load",
            "-fplugin=plugin.so",
            "-fpass-plugin=plugin.so",
            "-ivfsoverlay",
            "-fmodules",
            "-fmodule-file=module.pcm",
            "--config=config.cfg",
            "-include-pch",
            "-isystem-after",
            "-Wl,-load,plugin.so",
            "-Wp,-include,outside.h",
            "-Wa,option",
            "-unknown-flag",
        ] {
            let arguments = vec!["c++".to_string(), flag.to_string(), "unit.cpp".to_string()];
            let error = normalize_arguments(&arguments, root.path(), &unit, &roots)
                .expect_err("unsupported flag");
            assert!(
                error.to_string().contains("unsupported compilation flag"),
                "{flag}: {error:#}"
            );
        }
        for compiler in ["ccache", "sccache", "env", "/operator/custom-driver"] {
            let arguments = vec![compiler.to_string(), "unit.cpp".to_string()];
            assert!(
                normalize_arguments(&arguments, root.path(), &unit, &roots)
                    .expect_err("wrapper")
                    .to_string()
                    .contains("without wrappers")
            );
        }
    }

    #[test]
    fn required_values_and_exact_source_identity_are_checked() {
        let root = tempfile::tempdir().expect("root");
        let unit = write_unit(root.path(), "unit.cpp", "int value() { return 1; }\n");
        let roots = [("source", root.path())];
        for arguments in [
            vec!["c++"],
            vec!["c++", "-I"],
            vec!["c++", "-D"],
            vec!["c++", "other.cpp"],
            vec!["c++", "unit.cpp", "unit.cpp"],
            vec!["c++", "-I", "/unmanifested", "unit.cpp"],
            vec!["c++", "-I-", "unit.cpp"],
            vec!["c++", "-I=/outside", "unit.cpp"],
            vec!["c++", "-I$SYSROOT/outside", "unit.cpp"],
            vec!["c++", "-include", "../outside.h", "unit.cpp"],
            vec!["c++", "-include", "ops.hpp", "unit.cpp"],
            vec!["c++", "-imacros", "macros.h", "unit.cpp"],
        ] {
            let arguments = arguments
                .into_iter()
                .map(str::to_string)
                .collect::<Vec<_>>();
            assert!(
                normalize_arguments(&arguments, root.path(), &unit, &roots).is_err(),
                "{arguments:?}"
            );
        }
        let arguments = ["c++", "-c", "--", "unit.cpp"].map(str::to_string);
        assert_eq!(
            normalize_arguments(&arguments, root.path(), &unit, &roots).expect("source"),
            ["c++", "-c", "--", "{source}/unit.cpp"]
        );
    }

    #[test]
    fn path_values_normalize_without_changing_search_order_or_argument_forms() {
        let root = tempfile::tempdir().expect("root");
        let unit = write_unit(root.path(), "unit.cpp", "int value() { return 1; }\n");
        let roots = [("source", root.path())];
        let forced_header = root.path().join("config.h");
        let arguments = [
            "c++",
            "-Ifirst",
            "-isystem",
            "second",
            "-iquotethird",
            "-include",
            forced_header.to_str().expect("UTF-8 path"),
            "--sysroot=SDK",
            "-resource-dir",
            "resource",
            "-o",
            "unit.o",
            "-D",
            "FEATURE=1",
            "-arch",
            "arm64",
            "-Wconversion",
            "-fsanitize=address",
            "-c",
            "unit.cpp",
        ]
        .map(str::to_string);
        assert_eq!(
            normalize_arguments(&arguments, root.path(), &unit, &roots).expect("normalized"),
            [
                "c++",
                "-I{source}/first",
                "-isystem",
                "{source}/second",
                "-iquote{source}/third",
                "-include",
                "{source}/config.h",
                "--sysroot={source}/SDK",
                "-resource-dir",
                "{source}/resource",
                "-o",
                "{source}/unit.o",
                "-D",
                "FEATURE=1",
                "-arch",
                "arm64",
                "-Wconversion",
                "-fsanitize=address",
                "-c",
                "{source}/unit.cpp"
            ]
        );
    }

    #[test]
    fn relocation_keeps_the_fingerprint_and_a_define_changes_it() {
        let first = tempfile::tempdir().expect("first root");
        let second = tempfile::tempdir().expect("second root");
        let unit_a = write_unit(
            first.path(),
            "src/unit.cpp",
            "int pick(int value) { return value; }\n",
        );
        let _unit_b = write_unit(
            second.path(),
            "src/unit.cpp",
            "int pick(int value) { return value; }\n",
        );
        let sdk_a = first.path().join("sdk");
        let sdk_b = second.path().join("sdk");
        fs::create_dir(&sdk_a).expect("sdk a");
        fs::create_dir(&sdk_b).expect("sdk b");

        let command = |unit: &Path, sdk: &Path, define: &str| {
            format!(
                "{} -std=c++20 -DSOURCE_PATH_SIZE={define} -isysroot {} -c {} -o {}/unit.o",
                "/usr/bin/c++",
                sdk.display(),
                unit.display(),
                first.path().join("build").display()
            )
        };
        // The object path is under each checkout's build root. The first
        // command's `-o` uses `first`; the relocated command must use `second`.
        let command_b = format!(
            "/usr/bin/c++ -std=c++20 -DSOURCE_PATH_SIZE=11 -isysroot {} -c {} -o {}/unit.o",
            sdk_b.display(),
            second.path().join("src/unit.cpp").display(),
            second.path().join("build").display()
        );
        let db_a = database(
            first.path(),
            &format!(
                r#"[{{"directory":"{}","file":"{}","command":"{}"}}]"#,
                first.path().display(),
                unit_a.display(),
                command(&unit_a, &sdk_a, "11").replace('\\', "\\\\")
            ),
        );
        let db_b = database(
            second.path(),
            &format!(
                r#"[{{"directory":"{}","file":"{}","command":"{}"}}]"#,
                second.path().display(),
                second.path().join("src/unit.cpp").display(),
                command_b.replace('\\', "\\\\")
            ),
        );
        let profile = |root: &Path, db: &Path| {
            load_profile(
                db,
                &ProfileRoots {
                    source: root.to_path_buf(),
                    build: Some(root.join("build")),
                    sysroot: Some(root.join("sdk")),
                    dependencies: Vec::new(),
                },
            )
            .expect("load profile")
        };
        let a = profile(first.path(), &db_a);
        let b = profile(second.path(), &db_b);
        assert_eq!(a.fingerprint, b.fingerprint);
        assert_eq!(
            a.application_units.get("src/unit.cpp").expect("unit"),
            b.application_units.get("src/unit.cpp").expect("unit")
        );

        let changed = database(
            first.path(),
            &format!(
                r#"[{{"directory":"{}","file":"{}","command":"{}"}}]"#,
                first.path().display(),
                unit_a.display(),
                command(&unit_a, &sdk_a, "12").replace('\\', "\\\\")
            ),
        );
        let c = profile(first.path(), &changed);
        assert_ne!(
            a.fingerprint, c.fingerprint,
            "a define change is a different profile"
        );
    }

    #[test]
    fn an_undeclared_sysroot_is_rejected() {
        let root = tempfile::tempdir().expect("root");
        let unit = write_unit(root.path(), "src/unit.cpp", "int value() { return 1; }\n");
        let load = |sysroot: &str| {
            let db = database(
                root.path(),
                &format!(
                    r#"[{{"directory":"{}","file":"{}","command":"/usr/bin/c++ -std=c++20 -isysroot {sysroot} -c {}"}}]"#,
                    root.path().display(),
                    unit.display(),
                    unit.display()
                ),
            );
            load_profile(
                &db,
                &ProfileRoots {
                    source: root.path().to_path_buf(),
                    build: None,
                    sysroot: None,
                    dependencies: Vec::new(),
                },
            )
            .expect_err("undeclared sysroot")
        };
        for sysroot in [
            "/Library/Developer/CommandLineTools/SDKs/MacOSX.sdk",
            "/usr",
        ] {
            assert!(load(sysroot).to_string().contains("outside"));
        }
    }

    #[test]
    fn conflicting_commands_and_plugins_are_rejected() {
        let root = tempfile::tempdir().expect("root");
        let unit = write_unit(root.path(), "src/unit.cpp", "int value() { return 1; }\n");
        let roots = ProfileRoots {
            source: root.path().to_path_buf(),
            build: None,
            sysroot: None,
            dependencies: Vec::new(),
        };
        let conflict = database(
            root.path(),
            &format!(
                r#"[
                    {{"directory":"{}","file":"{}","command":"/usr/bin/c++ -std=c++20 -c {}"}},
                    {{"directory":"{}","file":"{}","command":"/usr/bin/c++ -std=c++17 -c {}"}}
                ]"#,
                root.path().display(),
                unit.display(),
                unit.display(),
                root.path().display(),
                unit.display(),
                unit.display()
            ),
        );
        let error = load_profile(&conflict, &roots).expect_err("conflict");
        assert!(error.to_string().contains("conflicting"), "{error:#}");

        let plugin = database(
            root.path(),
            &format!(
                r#"[{{"directory":"{}","file":"{}","command":"/usr/bin/c++ -fplugin=/tmp/p.so -c {}"}}]"#,
                root.path().display(),
                unit.display(),
                unit.display()
            ),
        );
        let error = load_profile(&plugin, &roots).expect_err("plugin");
        assert!(error.to_string().contains("plugin"), "{error:#}");
    }

    #[test]
    fn a_dependency_command_changes_the_fingerprint_without_becoming_an_application_unit() {
        let root = tempfile::tempdir().expect("root");
        let unit = write_unit(root.path(), "src/unit.cpp", "int value() { return 1; }\n");
        let dep_root = root.path().join("deps");
        let dep = write_unit(&dep_root, "simdjson.cpp", "void parse() {}\n");
        let load = |standard: &str| {
            let db = database(
                root.path(),
                &format!(
                    r#"[
                        {{"directory":"{}","file":"{}","arguments":["/usr/bin/c++","-std=c++20","-c","{}"]}},
                        {{"directory":"{}","file":"{}","arguments":["/usr/bin/c++","-std={standard}","-c","{}"]}}
                    ]"#,
                    root.path().display(),
                    unit.display(),
                    unit.display(),
                    dep_root.display(),
                    dep.display(),
                    dep.display()
                ),
            );
            load_profile(
                &db,
                &ProfileRoots {
                    source: root.path().to_path_buf(),
                    build: None,
                    sysroot: None,
                    dependencies: vec![("simdjson".to_string(), dep_root.clone())],
                },
            )
            .expect("load")
        };
        let a = load("c++17");
        let b = load("c++20");
        assert_eq!(a.application_units.len(), 1);
        assert!(a.application_units.contains_key("src/unit.cpp"));
        assert_ne!(a.fingerprint, b.fingerprint);
    }

    #[test]
    fn an_unmanifested_file_is_rejected_and_a_response_file_is_data() {
        let root = tempfile::tempdir().expect("root");
        let unit = write_unit(root.path(), "src/unit.cpp", "int value() { return 1; }\n");
        let roots = ProfileRoots {
            source: root.path().to_path_buf(),
            build: Some(root.path().join("build")),
            sysroot: None,
            dependencies: Vec::new(),
        };
        let outside = database(
            root.path(),
            r#"[{"directory":"/tmp","file":"/tmp/missing.cpp","command":"/usr/bin/c++ -c /tmp/missing.cpp"}]"#,
        );
        let error = load_profile(&outside, &roots).expect_err("outside");
        assert!(error.to_string().contains("outside"), "{error:#}");

        let response = root.path().join("build");
        fs::create_dir_all(&response).expect("build");
        fs::write(
            response.join("unit.rsp"),
            format!("-std=c++20\n-c\n{}\n", unit.display()),
        )
        .expect("rsp");
        let db = database(
            root.path(),
            &format!(
                r#"[{{"directory":"{}","file":"{}","command":"/usr/bin/c++ @{}/unit.rsp"}}]"#,
                root.path().display(),
                unit.display(),
                response.display()
            ),
        );
        let profile = load_profile(&db, &roots).expect("expanded response");
        let command = profile.application_units.get("src/unit.cpp").expect("unit");
        assert!(command.arguments.iter().any(|arg| arg == "-std=c++20"));
        assert!(!command.arguments.iter().any(|arg| arg.contains(".rsp")));
    }

    #[test]
    fn working_directory_and_argument_boundaries_change_identity() {
        let root = tempfile::tempdir().expect("root");
        let unit = write_unit(root.path(), "src/unit.cpp", "int value() { return 1; }\n");
        let roots = ProfileRoots {
            source: root.path().to_path_buf(),
            build: None,
            sysroot: None,
            dependencies: Vec::new(),
        };
        let load = |directory: &Path, arguments: Vec<&str>| {
            let mut arguments: Vec<String> = arguments.into_iter().map(str::to_string).collect();
            arguments.push(unit.to_string_lossy().into_owned());
            let entries = serde_json::json!([{
                "directory": directory,
                "file": unit,
                "arguments": arguments,
            }]);
            let db = database(root.path(), &entries.to_string());
            load_profile(&db, &roots).expect("profile").fingerprint
        };
        assert_ne!(
            load(root.path(), vec!["/usr/bin/c++", "-Irelative"]),
            load(
                &root.path().join("build"),
                vec!["/usr/bin/c++", "-Irelative"]
            ),
            "relative include roots depend on the working directory"
        );
        assert_ne!(
            load(root.path(), vec!["/usr/bin/c++", "-DFIRST=1\n-DSECOND=2"]),
            load(root.path(), vec!["/usr/bin/c++", "-DFIRST=1", "-DSECOND=2"]),
            "a newline inside an argument is not an argument separator"
        );
    }

    #[test]
    fn dependency_translation_unit_bytes_change_identity() {
        let root = tempfile::tempdir().expect("root");
        let dep_root = root.path().join("deps");
        let dep = write_unit(&dep_root, "library.cpp", "int value() { return 1; }\n");
        let db = database(
            root.path(),
            &serde_json::json!([{
                "directory": dep_root,
                "file": dep,
                "arguments": ["/usr/bin/c++", "-c", dep],
            }])
            .to_string(),
        );
        let roots = ProfileRoots {
            source: root.path().to_path_buf(),
            build: None,
            sysroot: None,
            dependencies: vec![("library".to_string(), dep_root)],
        };
        let before = load_profile(&db, &roots).expect("before");
        fs::write(&dep, "int value() { return 2; }\n").expect("change dependency");
        let after = load_profile(&db, &roots).expect("after");
        assert!(before.application_units.is_empty());
        assert_ne!(before.fingerprint, after.fingerprint);
    }

    #[test]
    fn quoted_command_arguments_preserve_literal_backslashes() {
        assert_eq!(
            split_command(r#"c++ '-DPATH=C:\data' "-DPATH=C:\data""#).expect("split"),
            ["c++", r"-DPATH=C:\data", r"-DPATH=C:\data"]
        );
    }

    #[test]
    fn parent_paths_and_symlinks_cannot_escape_profile_roots() {
        let parent = tempfile::tempdir().expect("parent");
        let source = parent.path().join("source");
        fs::create_dir(&source).expect("source");
        let outside = write_unit(
            parent.path(),
            "outside.cpp",
            "int outside() { return 1; }\n",
        );
        let roots = ProfileRoots {
            source: source.clone(),
            build: None,
            sysroot: None,
            dependencies: Vec::new(),
        };
        let load = |file: &str| {
            let db = database(
                &source,
                &serde_json::json!([{
                    "directory": source,
                    "file": file,
                    "arguments": ["c++", "-c", file],
                }])
                .to_string(),
            );
            load_profile(&db, &roots).expect_err("outside root")
        };
        assert!(load("../outside.cpp").to_string().contains("outside"));
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(outside, source.join("alias.cpp")).expect("symlink");
            assert!(load("alias.cpp").to_string().contains("outside"));
        }
    }

    #[test]
    fn attached_paths_normalize_and_literal_root_markers_are_rejected() {
        let root = tempfile::tempdir().expect("root");
        let unit = write_unit(root.path(), "unit.cpp", "int value() { return 1; }\n");
        let roots = ProfileRoots {
            source: root.path().to_path_buf(),
            build: None,
            sysroot: None,
            dependencies: Vec::new(),
        };
        let load = |mut arguments: Vec<String>| {
            arguments.push(unit.to_string_lossy().into_owned());
            let db = database(
                root.path(),
                &serde_json::json!([{
                    "directory": root.path(),
                    "file": unit,
                    "arguments": arguments,
                }])
                .to_string(),
            );
            load_profile(&db, &roots)
        };
        let profile = load(vec![
            "c++".to_string(),
            format!("--sysroot={}", root.path().display()),
            format!("-iquote{}", root.path().display()),
            format!("-resource-dir={}", root.path().display()),
        ])
        .expect("normalized paths");
        assert_eq!(
            profile.application_units["unit.cpp"].arguments,
            [
                "c++",
                "--sysroot={source}",
                "-iquote{source}",
                "-resource-dir={source}",
                "{source}/unit.cpp",
            ]
        );
        let error = load(vec!["c++".to_string(), "-I{source}/inc".to_string()])
            .expect_err("literal logical root");
        assert!(error.to_string().contains("reserved logical root"));
    }
}
