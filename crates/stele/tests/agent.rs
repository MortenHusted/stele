//! `stele intro` orients an agent and `stele agent install` gives it the
//! skill this binary was built with.

#![allow(clippy::doc_markdown)] // Example tags name catalog IDs, not code.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn stele(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_stele"))
        .args(args)
        .output()
        .expect("run stele")
}

fn install(args: &[&str], project: &Path) -> Output {
    let mut words = vec!["agent", "install"];
    words.extend(args);
    words.extend(["--dir", project.to_str().unwrap()]);
    stele(&words)
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

/// Every file under `dir`, by its `/`-separated path relative to `dir`.
fn files_under(dir: &Path) -> BTreeMap<String, String> {
    fn walk(root: &Path, dir: &Path, files: &mut BTreeMap<String, String>) {
        for entry in std::fs::read_dir(dir).expect("read skill directory") {
            let path = entry.expect("skill directory entry").path();
            if path.is_dir() {
                walk(root, &path, files);
            } else {
                let relative = path.strip_prefix(root).unwrap().to_str().unwrap();
                files.insert(
                    relative.replace('\\', "/"),
                    std::fs::read_to_string(&path).expect("read skill file"),
                );
            }
        }
    }
    let mut files = BTreeMap::new();
    walk(dir, dir, &mut files);
    files
}

/// Every skill as the repository carries it, by `<skill>/<path>`.
fn skill_files() -> BTreeMap<String, String> {
    files_under(&PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../plugins/stele/skills"))
}

#[test]
fn intro_orients_an_agent() {
    let output = stele(&["intro"]);
    assert_eq!(output.status.code(), Some(0));
    let intro = stdout(&output);
    for expected in [
        "stele check architecture|symbols|hotspots|citations",
        "stele code extract",
        "stele slop diff",
        "stele plan check",
        "stele intent report",
        "stele-onboard",
        "stele spec walk FILE --json",
        "stele spec mutate FILE --json",
        "stele spec render FILE",
        "assumption",
        "Never promote a guess to a fact",
        "stele agent install",
    ] {
        assert!(
            intro.contains(expected),
            "intro lacks {expected:?}:\n{intro}"
        );
    }
}

/// intent: example installed_skill_is_the_built_copy exemplifies skill_matches_binary on cli
#[test]
fn agent_install_writes_this_binarys_skill_into_a_project() {
    let project = tempfile::tempdir().expect("tempdir");
    let output = install(&[], project.path());
    assert_eq!(output.status.code(), Some(0), "{output:?}");

    let skills = skill_files();
    let mut claude_skills = skills.clone();
    claude_skills.retain(|path, _| !path.ends_with("/agents/openai.yaml"));
    assert!(claude_skills.contains_key("stele-onboard/SKILL.md"));
    let claude = project.path().join(".claude/skills");
    let codex = project.path().join(".codex/skills");
    assert_eq!(files_under(&claude), claude_skills);
    assert_eq!(files_under(&codex), skills);

    let wrote = stdout(&output);
    assert!(wrote.contains("Claude Code: wrote"), "{wrote}");
    assert!(wrote.contains("Codex: wrote"), "{wrote}");
    assert!(wrote.contains("skill version matches stele"), "{wrote}");

    let again = stdout(&install(&[], project.path()));
    assert!(again.contains("Claude Code: unchanged"), "{again}");
    assert!(!again.contains("wrote"), "{again}");

    std::fs::write(codex.join("stele/SKILL.md"), "stale").expect("age the codex skill");
    let refreshed = stdout(&install(&[], project.path()));
    assert!(refreshed.contains("Codex: updated"), "{refreshed}");
    assert_eq!(files_under(&codex), skills);
}

#[test]
fn agent_install_limits_itself_to_the_named_agent() {
    let project = tempfile::tempdir().expect("tempdir");
    let output = install(&["--codex"], project.path());
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    assert!(project.path().join(".codex/skills/stele/SKILL.md").exists());
    assert!(!project.path().join(".claude").exists());
}

#[test]
fn agent_install_refuses_a_missing_project() {
    let project = tempfile::tempdir().expect("tempdir");
    let output = install(&[], &project.path().join("missing"));
    assert_eq!(output.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.starts_with("stele agent: "), "{stderr}");
    assert!(stderr.contains("is not a directory"), "{stderr}");
}

#[test]
fn intro_and_agent_live_on_stele_not_on_spec() {
    let help = stdout(&stele(&["--help"]));
    for group in ["intro", "agent"] {
        assert!(help.contains(&format!("  {group} ")), "{help}");
    }

    let spec_help = stdout(&stele(&["spec", "--help"]));
    assert!(!spec_help.contains("intro"), "{spec_help}");
    assert!(!spec_help.contains("agent"), "{spec_help}");
    for retired in ["intro", "agent", "upgrade"] {
        let output = stele(&["spec", retired]);
        assert_eq!(output.status.code(), Some(2), "{retired}");
        assert!(
            String::from_utf8_lossy(&output.stderr)
                .contains(&format!("unknown command `{retired}`")),
            "{retired}"
        );
    }
}

#[test]
fn plugin_manifests_carry_this_version() {
    let plugin = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../plugins/stele");
    let manifest = |path: &str| -> serde_json::Value {
        let text = std::fs::read_to_string(plugin.join(path)).expect("read manifest");
        serde_json::from_str(&text).expect("parse manifest")
    };
    assert_eq!(
        manifest(".codex-plugin/plugin.json")["version"],
        env!("CARGO_PKG_VERSION")
    );
    // Claude Code keeps a pinned version until the string changes; without
    // one it tracks the commit, so a skill fix reaches agents before a release.
    assert!(
        manifest(".claude-plugin/plugin.json")
            .get("version")
            .is_none()
    );
    let marketplace = std::fs::read_to_string(plugin.join("../../.claude-plugin/marketplace.json"))
        .expect("read marketplace");
    assert!(!marketplace.contains("\"version\""), "{marketplace}");
}

/// The groups `stele` has, and whether each takes a command word.
const GROUPS: [(&str, bool); 9] = [
    ("check", true),
    ("code", true),
    ("plan", true),
    ("intent", true),
    ("spec", true),
    ("slop", true),
    ("topology", false),
    ("intro", false),
    ("agent", true),
];

/// A word as it stands in prose or a usage line, without the backticks and
/// punctuation around it; `None` for a placeholder, path or flag.
fn command_word(token: &str) -> Option<&str> {
    if token.contains('<') {
        return None;
    }
    let word = token.trim_matches(|c: char| !c.is_ascii_alphanumeric() && c != '-');
    (!word.is_empty() && word.chars().all(|c| c.is_ascii_lowercase() || c == '-'))
        .then_some(word)
        .filter(|word| !word.starts_with('-'))
}

/// Each `stele GROUP [WORD]` the text names, with `a|b` alternatives split.
fn named_commands(text: &str) -> BTreeSet<(String, Option<String>)> {
    let mut named = BTreeSet::new();
    for (index, _) in text.match_indices("stele ") {
        let before = text[..index].chars().next_back();
        if before.is_some_and(|c| !(c.is_whitespace() || "`(\"".contains(c))) {
            continue;
        }
        let mut tokens = text[index + "stele ".len()..].split_whitespace();
        let Some(group) = tokens.next() else { continue };
        if group.starts_with('-') {
            continue;
        }
        // A code span closing on the group leaves the next word as prose.
        let span_closed = group
            .trim_end_matches(|c: char| c.is_ascii_punctuation() && c != '`')
            .ends_with('`');
        let group = command_word(group).unwrap_or(group).to_string();
        let takes_word = !span_closed
            && GROUPS
                .iter()
                .find(|(name, _)| *name == group)
                .is_some_and(|(_, takes_word)| *takes_word);
        let words: Vec<String> = tokens
            .next()
            .filter(|_| takes_word)
            .map(|token| {
                token
                    .split('|')
                    .filter_map(command_word)
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default();
        if words.is_empty() {
            named.insert((group, None));
        } else {
            for word in words {
                named.insert((group.clone(), Some(word)));
            }
        }
    }
    named
}

/// intent: example named_commands_exist exemplifies skill_matches_binary on cli
#[test]
fn every_command_the_skill_and_intro_name_exists() {
    let mut texts: Vec<String> = skill_files().into_values().collect();
    texts.push(stdout(&stele(&["intro"])));
    let named: BTreeSet<_> = texts.iter().flat_map(|text| named_commands(text)).collect();
    assert!(named.len() > 20, "too few commands found: {named:?}");

    let scratch = tempfile::tempdir().expect("tempdir");
    for (group, word) in &named {
        assert!(
            GROUPS.iter().any(|(name, _)| name == group),
            "`stele {group}` is not a group"
        );
        let mut args = vec![group.as_str()];
        args.extend(word.as_deref());
        if group == "spec" {
            // The engine answers --help with its overview for any word, so
            // ask for the command itself and require it to be known.
            let output = Command::new(env!("CARGO_BIN_EXE_stele"))
                .args(&args)
                .current_dir(scratch.path())
                .output()
                .expect("run stele spec");
            let stderr = String::from_utf8_lossy(&output.stderr);
            assert!(
                !stderr.contains("unknown command"),
                "`stele {}`: {stderr}",
                args.join(" ")
            );
        } else {
            args.push("--help");
            let output = stele(&args);
            assert!(
                output.status.success(),
                "`stele {}`: {}",
                args.join(" "),
                String::from_utf8_lossy(&output.stderr)
            );
        }
    }
}
