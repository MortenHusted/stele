//! The ledger syntax beside a catalog: records with no header and no ID,
//! with integers and `key: value` list entries, which catalogs and plans
//! still refuse.

use stele_plan::syntax::{Value, parse, parse_ledger, quote};

const APPROVALS: &str = r#"approval { item: walk_exit_status action: approve by: "owner"
           digest: "sha256:abc" digest_version: 1
           links: [results_are_honest: "sha256:def"] text: "walk exits 0 when every expectation holds." }
"#;

#[test]
fn a_ledger_reads_as_records_with_integers_and_list_entries() {
    let records = parse_ledger(APPROVALS, "intent/approvals.stele").unwrap();
    assert_eq!(records.len(), 1);
    let record = &records[0];
    assert_eq!((record.kind.as_str(), record.line), ("approval", 1));
    let fields: Vec<_> = record
        .fields
        .iter()
        .map(|f| (f.name.as_str(), f.value.clone(), f.line))
        .collect();
    assert_eq!(
        fields,
        [
            ("item", Value::Ident("walk_exit_status".into()), 1),
            ("action", Value::Ident("approve".into()), 1),
            ("by", Value::Str("owner".into()), 1),
            ("digest", Value::Str("sha256:abc".into()), 2),
            ("digest_version", Value::Int(1), 2),
            (
                "links",
                Value::List(vec![Value::Entry(
                    "results_are_honest".into(),
                    Box::new(Value::Str("sha256:def".into()))
                )]),
                3
            ),
            (
                "text",
                Value::Str("walk exits 0 when every expectation holds.".into()),
                3
            ),
        ]
    );
    assert_eq!(
        parse_ledger("run { tags: [a, b.c, \"d \\\"e\\\"\", 7,] }", "x.stele").unwrap()[0].fields
            [0]
        .value,
        Value::List(vec![
            Value::Ident("a".into()),
            Value::Qualified("b".into(), "c".into()),
            Value::Str("d \"e\"".into()),
            Value::Int(7),
        ])
    );
    assert_eq!(parse_ledger("// Nothing yet.\n", "x.stele").unwrap(), []);
}

#[test]
fn ledger_errors_name_the_file_and_line() {
    for (source, expected) in [
        (
            "approval {\n  digest_version: 1x\n}\n",
            "x.stele:2: invalid integer 1x",
        ),
        (
            "approval {\n  links: [\"key\": \"value\"]\n}\n",
            "x.stele:2: a list entry key must be an identifier",
        ),
        ("approval x {\n}\n", "x.stele:1: expected '{', found x"),
        (
            "approval {\n}\napproval\n",
            "x.stele:3: expected '{', found end of file",
        ),
    ] {
        assert_eq!(parse_ledger(source, "x.stele").unwrap_err(), expected);
    }
}

#[test]
fn catalogs_and_plans_still_need_a_header_and_ids_and_refuse_ledger_values() {
    for (source, expected) in [
        (
            "approval {\n  item: x\n}\n",
            "x.stele:1: expected catalog NAME or plan NAME, found approval",
        ),
        (
            "catalog walk {\n  intent {\n  }\n}\n",
            "x.stele:2: expected block id, found '{'",
        ),
        (
            "catalog walk {\n  intent x {\n    text: 7\n  }\n}\n",
            "x.stele:3: expected a value, found 7",
        ),
        (
            "plan next {\n  change x {\n    serves: [a: b]\n  }\n}\n",
            "x.stele:3: expected ']', found ':'",
        ),
        (
            "catalog walk {\n  intent x { text: \"X.\" }\n}\napproval { item: x }\n",
            "x.stele:4: expected end of file, found approval",
        ),
    ] {
        assert_eq!(parse(source, "x.stele").unwrap_err(), expected);
    }
}

#[test]
fn quoted_text_reads_back_unchanged() {
    let text = "A \"quoted\" word,\ta tab, a backslash \\ and\r\na new line.";
    let quoted = quote(text);
    assert_eq!(
        quoted,
        r#""A \"quoted\" word,\ta tab, a backslash \\ and\r\na new line.""#
    );
    let records = parse_ledger(&format!("note {{ text: {quoted} }}"), "x.stele").unwrap();
    assert_eq!(records[0].fields[0].value, Value::Str(text.into()));
}
