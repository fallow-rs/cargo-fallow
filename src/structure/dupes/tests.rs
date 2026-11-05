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
    let tokens = with_parsed(source, Edition::Edition2024, |parsed, text| {
        let index = LineIndex::new(text);
        tokens::tokens(parsed.syntax(), &index, mode, true)
    })
    .unwrap_or_default();
    File {
        path: path.to_owned(),
        lines: count_lines(source),
        skip_metrics: false,
        test_only: false,
        functions: Vec::new(),
        tokens,
    }
}

fn run(files: &[File], settings: &Duplication) -> Found {
    run_limited(files, settings, &Limits::default()).unwrap_or_default()
}

fn run_limited(files: &[File], settings: &Duplication, limits: &Limits) -> Result<Found, TimedOut> {
    let refs: Vec<&File> = files.iter().collect();
    detect(&refs, settings, limits, &|_, _| false)
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
    let refs: Vec<&File> = files.iter().collect();
    // The comment is on line 1 of `x.rs`, so the copy at line 2 is hidden.
    let hides = |path: &str, line: u32| path == "x.rs" && line == 2;
    let found = detect(&refs, &settings, &Limits::default(), &hides).unwrap_or_default();
    assert!(found.groups.is_empty());
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

/// A data table: one long run of the same two tokens, `1` and `,`.
fn table(values: usize) -> String {
    format!("static T: &[u32] = &[{}];\n", "1, ".repeat(values))
}

#[test]
fn a_long_periodic_run_keeps_the_search_fast_and_finds_the_real_clone() {
    let settings = Duplication::default();
    let files = [
        file(
            "a/x.rs",
            &format!("{}{}", table(40_000), body("one", 2)),
            settings.mode,
        ),
        file("b/y.rs", &body("one", 2), settings.mode),
    ];
    let started = std::time::Instant::now();
    let found = run(&files, &settings);
    let elapsed = started.elapsed();
    assert!(elapsed.as_secs() < 10, "the search took {elapsed:?}");
    let across = found
        .groups
        .iter()
        .filter(|group| group.instances.iter().any(|i| i.file == "b/y.rs"))
        .count();
    assert_eq!(across, 1, "{:?}", found.groups);
    // The table gives at most a few groups, not one group for each length.
    assert!(found.groups.len() <= 4, "{} groups", found.groups.len());
}

#[test]
fn a_file_over_the_token_budget_is_skipped_with_a_reason() {
    let settings = Duplication::default();
    let limits = Limits {
        max_file_tokens: 500,
        ..Limits::default()
    };
    let files = [
        file("big.rs", &table(1000), settings.mode),
        file("x.rs", &body("a", 2), settings.mode),
        file("y.rs", &body("a", 2), settings.mode),
    ];
    let Ok(found) = run_limited(&files, &settings, &limits) else {
        panic!("the search finishes");
    };
    assert_eq!(found.groups.len(), 1, "{:?}", found.groups);
    assert_eq!(found.skipped.len(), 1, "{:?}", found.skipped);
    assert_eq!(found.skipped[0].path, "big.rs");
    assert!(
        found.skipped[0].reason.contains("over the limit of 500"),
        "{}",
        found.skipped[0].reason
    );
    assert_eq!(found.stats.total_files, 2);
}

#[test]
fn a_repeat_with_many_places_keeps_the_first_places() {
    let settings = Duplication::default();
    let limits = Limits {
        max_places: 3,
        ..Limits::default()
    };
    let files: Vec<File> = (0..6)
        .map(|index| file(&format!("f{index}.rs"), &body("a", 2), settings.mode))
        .collect();
    let Ok(found) = run_limited(&files, &settings, &limits) else {
        panic!("the search finishes");
    };
    assert_eq!(found.groups.len(), 1, "{:?}", found.groups);
    assert_eq!(found.groups[0].instances.len(), 3);
    let again = run_limited(&files, &settings, &limits).unwrap_or_default();
    assert_eq!(found.groups[0].instances, again.groups[0].instances);
}

#[test]
fn the_search_stops_at_the_deadline() {
    let settings = Duplication::default();
    let limits = Limits {
        deadline: std::time::Duration::ZERO,
        ..Limits::default()
    };
    let files = [
        file("x.rs", &body("a", 2), settings.mode),
        file("y.rs", &body("a", 2), settings.mode),
    ];
    assert!(run_limited(&files, &settings, &limits).is_err());
}

#[test]
fn columns_on_a_long_line_cost_no_rescan_of_the_line() {
    let line = "1, ".repeat(100_000);
    let index = LineIndex::new(&line);
    let started = std::time::Instant::now();
    let mut last = 0;
    for offset in (0..line.len()).step_by(3) {
        last = index
            .position(ra_ap_syntax::TextSize::from(
                u32::try_from(offset).unwrap_or(0),
            ))
            .1;
    }
    assert_eq!(last as usize, line.len() - 2);
    assert!(started.elapsed().as_secs() < 2, "{:?}", started.elapsed());
    let wide = "é, 1";
    let index = LineIndex::new(wide);
    assert_eq!(index.position(ra_ap_syntax::TextSize::from(4)), (1, 4));
}
