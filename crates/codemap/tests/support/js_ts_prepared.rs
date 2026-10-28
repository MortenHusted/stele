use codemap::js_ts_inputs::{DirectoryEntry, EntryKind, InputManifest, InputRoots};
use codemap::js_ts_semantic::HelperConfig;
use codemap::schema::sha256_hex;
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

#[allow(clippy::literal_string_with_formatting_args)]
pub fn prepared(source: &Path) -> HelperConfig {
    let bundle = fs::canonicalize(std::env::var_os("CODEMAP_JS_TS_PROBE_BUNDLE").unwrap()).unwrap();
    let typescript = bundle.join("node_modules/typescript/lib");
    let mut manifest = InputManifest {
        files: BTreeMap::new(),
        directories: BTreeMap::new(),
    };
    for (logical, root) in [("{source}", source), ("{typescript}", typescript.as_path())] {
        let mut entries = BTreeMap::new();
        for entry in fs::read_dir(root).unwrap() {
            let entry = entry.unwrap();
            let name = entry.file_name().into_string().unwrap();
            if name == ".git" {
                continue;
            }
            let directory = entry.file_type().unwrap().is_dir();
            entries.insert(
                name.clone(),
                DirectoryEntry {
                    kind: if directory {
                        EntryKind::Directory
                    } else {
                        EntryKind::File
                    },
                    target: None,
                },
            );
            if !directory && (logical == "{source}" || name.ends_with(".d.ts")) {
                manifest.files.insert(
                    format!("{logical}/{name}"),
                    sha256_hex(&fs::read(entry.path()).unwrap()),
                );
            }
        }
        manifest.directories.insert(logical.to_owned(), entries);
    }
    HelperConfig {
        roots: InputRoots {
            source: source.to_path_buf(),
            typescript,
        },
        inputs: manifest,
        runtime: serde_json::from_slice(
            &fs::read(std::env::var_os("CODEMAP_JS_TS_RUNTIME").unwrap()).unwrap(),
        )
        .unwrap(),
        bundle,
        project: "tsconfig.json".to_owned(),
    }
}

#[allow(dead_code)]
pub fn fixture() -> tempfile::TempDir {
    let scratch = tempfile::tempdir().unwrap();
    for entry in
        fs::read_dir(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/js_ts_calls"))
            .unwrap()
    {
        let entry = entry.unwrap();
        fs::copy(entry.path(), scratch.path().join(entry.file_name())).unwrap();
    }
    scratch
}
