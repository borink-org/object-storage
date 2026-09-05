//! The recorded corpus under `tests/fixtures` agrees with itself.
//!
//! `tests/azure-record` writes every file and every set of notes in one run,
//! so a file without a row in its notes, or a row without a file, means the
//! directory was edited by hand or a run was cut short. A file no test reads
//! is a recording that proves nothing. Each of those is caught here, offline,
//! rather than the next time someone records.

mod recorded;

use std::collections::BTreeSet;
use std::fs;
use std::path::Path;

use borink_object_storage_proto::VERSION;
use test_support::recorded::{RecordedResponse, corpus_dir};

/// The groups whose files no test reads, and the notes' own words for why.
/// A group is exempt from `every_recorded_response_is_read_by_a_test` only
/// while its notes still say so.
const UNREAD: &[(&str, &str)] = &[("azure-multipart", "Nothing reads these files.")];

fn groups() -> Vec<String> {
    let mut groups: Vec<String> = fs::read_dir(corpus_dir())
        .unwrap()
        .map(|entry| entry.unwrap())
        .filter(|entry| entry.file_type().unwrap().is_dir())
        .map(|entry| entry.file_name().into_string().unwrap())
        .collect();
    groups.sort();
    assert!(!groups.is_empty());
    groups
}

fn files(group: &str) -> BTreeSet<String> {
    fs::read_dir(corpus_dir().join(group))
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .filter(|name| name != "README.md")
        .collect()
}

fn notes(group: &str) -> String {
    let path = corpus_dir().join(group).join("README.md");
    fs::read_to_string(&path).unwrap_or_else(|error| panic!("{}: {error}", path.display()))
}

// The first cell of every row of the table in the notes: the file it names.
fn named_in_notes(notes: &str) -> BTreeSet<String> {
    notes
        .lines()
        .filter_map(|line| line.strip_prefix("| `"))
        .filter_map(|rest| rest.split_once('`'))
        .map(|(file, _)| file.to_owned())
        .collect()
}

#[test]
fn every_file_in_a_group_is_a_recorded_response() {
    for group in groups() {
        for file in files(&group) {
            assert!(
                file.ends_with(".http"),
                "{group}/{file} is not a recorded response"
            );
            let path = corpus_dir().join(&group).join(&file);
            let response = RecordedResponse::parse(&fs::read(&path).unwrap())
                .unwrap_or_else(|error| panic!("{group}/{file}: {error}"));
            assert!(
                response.status_line.starts_with("HTTP/1.1 "),
                "{group}/{file}: {}",
                response.status_line
            );
            // Every response that names the service version it answered under
            // names the one this crate asks for. A response that names none
            // was refused before the service read the request.
            if let Some(version) = response.header("x-ms-version") {
                assert_eq!(
                    version,
                    VERSION.as_bytes(),
                    "{group}/{file} was recorded under another service version"
                );
            }
        }
    }
}

#[test]
fn the_notes_of_a_group_name_its_files_and_nothing_else() {
    for group in groups() {
        let notes = notes(&group);
        let files = files(&group);
        let named = named_in_notes(&notes);
        assert!(!named.is_empty(), "{group}/README.md has no table");
        assert_eq!(
            named, files,
            "{group}: the notes and the files disagree; record the corpus again"
        );
        assert!(
            notes.contains(&format!("service version `{VERSION}`")),
            "{group}/README.md names another service version than this crate asks for"
        );
    }
}

#[test]
fn every_recorded_response_is_read_by_a_test() {
    // The test sources beside this one, as text. A test reads a file by
    // naming it: `Recorded::load("group/file")`, or in a table of such names.
    let tests = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests");
    let mut sources = String::new();
    for entry in fs::read_dir(&tests).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().is_some_and(|extension| extension == "rs") {
            sources.push_str(&fs::read_to_string(path).unwrap());
        }
    }

    for group in groups() {
        if let Some((_, reason)) = UNREAD.iter().find(|(unread, _)| *unread == group) {
            assert!(
                notes(&group).contains(reason),
                "{group} is exempt from being read only while its notes say {reason:?}"
            );
            continue;
        }
        for file in files(&group) {
            let name = format!("\"{group}/{}\"", file.trim_end_matches(".http"));
            assert!(
                sources.contains(&name),
                "{group}/{file} is recorded but no test reads it; assert it or drop the capture"
            );
        }
    }
}
