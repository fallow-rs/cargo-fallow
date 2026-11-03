use ra_ap_syntax::AstNode;

use super::*;
use crate::parse::{Edition, with_parsed};
use crate::structure::source::{LineIndex, count_lines};

/// A function with about 70 tokens over 8 lines. `name` and `value` change the
/// names and a number.
fn body(name: &str, value: u32) -> String {
    format!(
        "fn {name}(items: &[u32], limit: u32) -> u32 {{\n    let mut total = 0;\n    for item in items {{\n        if *item > limit {{\n            total += item * {value};\n        }} else {{\n            total -= 1;\n        }}\n    }}\n    total + limit * 3 - items.len() as u32\n}}\n"
    )
}

fn file(path: &str, source: &str, mode: Mode) -> File {
    let (tokens, suppressions) = with_parsed(source, Edition::Edition2024, |parsed, text| {
        let index = LineIndex::new(text);
        let found = tokens::tokens(parsed.syntax(), &index, mode, true);
        let notes: Vec<(u32, String)> = text
            .lines()
            .enumerate()
            .filter(|(_, line)| line.contains("fallow-ignore-next-line code-duplication"))
            .map(|(at, _)| {
                (
                    u32::try_from(at + 1).unwrap_or(0),
                    "code-duplication".to_owned(),
                )
            })
            .collect();
        (found, notes)
    })
    .unwrap_or_default();
    File {
        path: path.to_owned(),
        lines: count_lines(source),
        generated: false,
        skip_metrics: false,
        functions: Vec::new(),
        tokens,
        suppressions,
    }
}

fn run(files: &[File], settings: &Duplication) -> Found {
    let refs: Vec<&File> = files.iter().collect();
    detect(&refs, settings)
}

#[test]
fn two_copies_in_two_files_form_one_group() {
    let settings = Duplication::default();
    let files = [
        file("a/x.rs", &body("one", 2), settings.mode),
        file(
            "b/y.rs",
            &format!("const A: u8 = 1;\n{}", body("one", 2)),
            settings.mode,
        ),
    ];
    let found = run(&files, &settings);
    assert_eq!(found.groups.len(), 1, "{:?}", found.groups);
    let group = &found.groups[0];
    let places: Vec<(&str, u32, u32)> = group
        .instances
        .iter()
        .map(|i| (i.file.as_str(), i.start_line, i.end_line))
        .collect();
    assert_eq!(places, vec![("a/x.rs", 1, 11), ("b/y.rs", 2, 12)]);
    assert!(group.fingerprint.starts_with("dup:"));
    assert_eq!(group.actions[0].kind, "extract-shared");
    assert_eq!(found.stats.duplicated_lines, 22);
    assert_eq!(found.stats.total_lines, 23);
    assert_eq!(found.families.len(), 1);
}

#[test]
fn three_copies_give_one_group_with_three_instances() {
    let settings = Duplication::default();
    // The names differ, so the clone starts after the name. A different item
    // after each copy keeps the clone to the function.
    let source = format!(
        "{}struct P;\n{}enum Q {{}}\n{}",
        body("a", 2),
        body("b", 2),
        body("c", 2)
    );
    let found = run(&[file("x.rs", &source, settings.mode)], &settings);
    assert_eq!(found.groups.len(), 1, "{:?}", found.groups);
    assert_eq!(found.groups[0].instances.len(), 3);
}

#[test]
fn a_clone_under_the_line_limit_is_dropped() {
    let settings = Duplication {
        min_lines: 20,
        ..Duplication::default()
    };
    let files = [
        file("x.rs", &body("a", 2), settings.mode),
        file("y.rs", &body("a", 2), settings.mode),
    ];
    assert!(run(&files, &settings).groups.is_empty());
}

#[test]
fn semantic_mode_matches_renamed_copies_and_mild_mode_does_not() {
    let mild = Duplication::default();
    let semantic = Duplication {
        mode: Mode::Semantic,
        ..Duplication::default()
    };
    let make = |mode| {
        [
            file("x.rs", &body("a", 2).replace("total", "sum"), mode),
            file("y.rs", &body("a", 7), mode),
        ]
    };
    assert_eq!(run(&make(Mode::Mild), &mild).groups.len(), 0);
    assert_eq!(run(&make(Mode::Semantic), &semantic).groups.len(), 1);
}

#[test]
fn a_suppression_comment_removes_the_instance_and_the_group() {
    let settings = Duplication::default();
    let files = [
        file(
            "x.rs",
            &format!(
                "// fallow-ignore-next-line code-duplication\n{}",
                body("a", 2)
            ),
            settings.mode,
        ),
        file("y.rs", &body("a", 2), settings.mode),
    ];
    assert!(run(&files, &settings).groups.is_empty());
}

#[test]
fn skip_local_drops_a_group_in_one_folder() {
    let settings = Duplication {
        skip_local: true,
        ..Duplication::default()
    };
    let files = [
        file("src/x.rs", &body("a", 2), settings.mode),
        file("src/y.rs", &body("a", 2), settings.mode),
    ];
    assert!(run(&files, &settings).groups.is_empty());
}

#[test]
fn merged_ranges_join_overlaps_and_touching_lines_only_on_request() {
    let mut ranges = Vec::new();
    insert_merged(&mut ranges, 1, 3, false);
    insert_merged(&mut ranges, 4, 6, false);
    assert_eq!(ranges, vec![(1, 3), (4, 6)]);
    insert_merged(&mut ranges, 2, 5, false);
    assert_eq!(ranges, vec![(1, 6)]);
    let mut lines = vec![(1, 3)];
    insert_merged(&mut lines, 4, 6, true);
    assert_eq!(lines, vec![(1, 6)]);
    assert!(contains(&lines, 2, 6));
    assert!(!contains(&lines, 2, 7));
}
