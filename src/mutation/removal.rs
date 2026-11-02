//! The removal loop: compile, remove the mutants that break the build, repeat.
//!
//! A compiler error span selects a mutant through the regions of the generated
//! file. When no error span selects a mutant, a bisection in the style of the
//! Stryker grouping finds one mutant that breaks the build: it compiles a prefix
//! of the enabled mutants and halves the range until one mutant stays.

use std::collections::BTreeSet;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::mutation::files::{CrateFile, normalize};
use crate::mutation::schema::{Mode, Owner, PROBE_PREFIX, Rendered, render};

/// The largest number of compile rounds. A crate over this number counts as a
/// crate that never compiles, and its mutants go to the rebuild tier.
pub const MAX_ROUNDS: u32 = 15;

/// One compiler error.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CompilerError {
    /// The error code, for example `E0308`.
    pub code: Option<String>,
    /// The first line of the message.
    pub message: String,
    /// For each primary span, the span and then its macro call sites.
    pub spans: Vec<Vec<SpanAt>>,
}

/// A byte range in one file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpanAt {
    /// The file, as rustc writes it.
    pub file: String,
    /// The first byte.
    pub start: usize,
    /// The byte after the last byte.
    pub end: usize,
}

/// The result of one build.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Build {
    /// True when cargo reports success.
    pub success: bool,
    /// The compiler errors.
    pub errors: Vec<CompilerError>,
}

#[derive(Deserialize)]
struct CargoLine {
    reason: String,
    message: Option<RustcMessage>,
    success: Option<bool>,
}

#[derive(Deserialize)]
struct RustcMessage {
    level: String,
    message: String,
    code: Option<RustcCode>,
    spans: Vec<RustcSpan>,
}

#[derive(Deserialize)]
struct RustcCode {
    code: String,
}

#[derive(Clone, Deserialize)]
struct RustcSpan {
    file_name: String,
    byte_start: usize,
    byte_end: usize,
    is_primary: bool,
    expansion: Option<Box<RustcExpansion>>,
}

#[derive(Clone, Deserialize)]
struct RustcExpansion {
    span: RustcSpan,
}

/// Reads the output of `cargo --message-format=json`. A line that is not JSON
/// or that has another shape does not count.
pub fn parse_build(stdout: &str) -> Build {
    let mut build = Build::default();
    for line in stdout.lines() {
        let Ok(parsed) = serde_json::from_str::<CargoLine>(line) else {
            continue;
        };
        match (parsed.reason.as_str(), parsed.message) {
            ("compiler-message", Some(message)) if is_error(&message) => {
                build.errors.push(convert(message));
            }
            ("build-finished", _) => build.success = parsed.success.unwrap_or(false),
            _ => {}
        }
    }
    build
}

/// True for an error with content. The closing summary line has no span and
/// no code, and cargo repeats it for each target.
fn is_error(message: &RustcMessage) -> bool {
    let error = matches!(
        message.level.as_str(),
        "error" | "error: internal compiler error"
    );
    error && !message.message.starts_with("aborting due to")
}

fn convert(message: RustcMessage) -> CompilerError {
    let spans = message
        .spans
        .iter()
        .filter(|span| span.is_primary)
        .map(span_chain)
        .collect();
    CompilerError {
        code: message.code.map(|code| code.code),
        message: message
            .message
            .lines()
            .next()
            .unwrap_or_default()
            .to_owned(),
        spans,
    }
}

fn span_chain(span: &RustcSpan) -> Vec<SpanAt> {
    let mut chain = Vec::new();
    let mut current = Some(span);
    while let Some(step) = current {
        chain.push(SpanAt {
            file: step.file_name.clone(),
            start: step.byte_start,
            end: step.byte_end,
        });
        current = step.expansion.as_deref().map(|expansion| &expansion.span);
    }
    chain
}

/// The way that the loop found a broken mutant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Via {
    /// An error span is inside the mutant arm.
    Arm,
    /// An error span is inside the switch, outside each mutant arm.
    Site,
    /// No error span selects a mutant, so a bisection found it.
    Bisection,
}

/// One removed mutant.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Removal {
    /// The mutant.
    pub id: u64,
    /// The compile round that removed it.
    pub round: u32,
    /// How the loop found it.
    pub how: Via,
    /// The compiler error code.
    pub code: Option<String>,
    /// The compiler message.
    pub message: String,
}

/// A record of one compile round.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RoundRecord {
    /// The round number, from 1.
    pub round: u32,
    /// `full` or `bisection`.
    pub phase: &'static str,
    /// The number of mutants in this build.
    pub mutants: usize,
    /// The number of compiler errors.
    pub errors: usize,
    /// True when the build passed.
    pub success: bool,
}

/// The result of the loop.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LoopResult {
    /// The compile rounds until the schema compiled, or `None` when it never
    /// compiled.
    pub rounds: Option<u32>,
    /// The mutants in the final schema. When the schema never compiled, the
    /// mutants that the loop did not remove.
    pub kept: BTreeSet<u64>,
    /// The removed mutants, in order.
    pub removed: Vec<Removal>,
    /// One record for each round.
    pub log: Vec<RoundRecord>,
    /// The errors of the last failed round.
    pub last_errors: Vec<CompilerError>,
}

/// Renders all files for one set of enabled mutants.
pub fn render_all(files: &[CrateFile], enabled: &BTreeSet<u64>, mode: Mode) -> Vec<Rendered> {
    let any = !enabled.is_empty();
    files
        .iter()
        .map(|file| {
            let is_root = file.is_root && any;
            render(&file.original, &file.found.sites, enabled, is_root, mode)
        })
        .collect()
}

/// The mutants that a build of a [`Mode::Probe`] schema reports.
pub fn probed_mutants(build: &Build) -> BTreeSet<u64> {
    build
        .errors
        .iter()
        .filter_map(|error| {
            let (_, tail) = error.message.split_once(PROBE_PREFIX)?;
            let digits = tail.split(|c: char| !c.is_ascii_hexdigit()).next()?;
            u64::from_str_radix(digits, 16).ok()
        })
        .collect()
}

/// Runs the loop. `build` writes the rendered files and compiles them.
pub fn run_loop<B>(files: &[CrateFile], mut build: B) -> LoopResult
where
    B: FnMut(&[Rendered]) -> Build,
{
    let order: Vec<u64> = files
        .iter()
        .flat_map(|file| &file.found.sites)
        .flat_map(|site| &site.mutants)
        .map(|mutant| mutant.id)
        .collect();
    let mut state = LoopState {
        files,
        build: &mut build,
        result: LoopResult {
            kept: order.iter().copied().collect(),
            ..LoopResult::default()
        },
        round: 0,
    };
    state.run(&order);
    state.result
}

struct LoopState<'a, B> {
    files: &'a [CrateFile],
    build: &'a mut B,
    result: LoopResult,
    round: u32,
}

/// The mutants that one set of errors selects.
type Selection = Vec<(u64, Via, CompilerError)>;

impl<B> LoopState<'_, B>
where
    B: FnMut(&[Rendered]) -> Build,
{
    fn run(&mut self, order: &[u64]) {
        while self.round < MAX_ROUNDS {
            let enabled = self.result.kept.clone();
            let Some((build, rendered)) = self.compile(&enabled, "full") else {
                return;
            };
            if build.success {
                self.result.rounds = Some(self.round);
                self.result.last_errors.clear();
                return;
            }
            let selected = select(self.files, &rendered, &enabled, &build.errors);
            let selected = if selected.is_empty() {
                let list: Vec<u64> = order
                    .iter()
                    .copied()
                    .filter(|id| enabled.contains(id))
                    .collect();
                self.bisect(&list, &build)
            } else {
                selected
            };
            if selected.is_empty() {
                return;
            }
            self.remove(selected);
        }
    }

    fn compile(
        &mut self,
        enabled: &BTreeSet<u64>,
        phase: &'static str,
    ) -> Option<(Build, Vec<Rendered>)> {
        if self.round >= MAX_ROUNDS {
            return None;
        }
        self.round += 1;
        let rendered = render_all(self.files, enabled, Mode::Schema);
        let build = (self.build)(&rendered);
        self.result.log.push(RoundRecord {
            round: self.round,
            phase,
            mutants: enabled.len(),
            errors: build.errors.len(),
            success: build.success,
        });
        if !build.success {
            self.result.last_errors.clone_from(&build.errors);
        }
        Some((build, rendered))
    }

    /// Finds one mutant that breaks the build. A build of a prefix can also
    /// give an error that selects a mutant: then the search stops there.
    fn bisect(&mut self, list: &[u64], failed: &Build) -> Selection {
        let (mut low, mut high) = (0, list.len());
        let mut last_error = failed.errors.first().cloned().unwrap_or_default();
        while high - low > 1 {
            let middle = low + (high - low) / 2;
            let enabled: BTreeSet<u64> = list[..middle].iter().copied().collect();
            let Some((build, rendered)) = self.compile(&enabled, "bisection") else {
                return Vec::new();
            };
            if build.success {
                low = middle;
                continue;
            }
            let selected = select(self.files, &rendered, &enabled, &build.errors);
            if !selected.is_empty() {
                return selected;
            }
            if let Some(error) = build.errors.first() {
                last_error.clone_from(error);
            }
            high = middle;
        }
        list.get(high.wrapping_sub(1))
            .map(|&id| (id, Via::Bisection, last_error))
            .into_iter()
            .collect()
    }

    fn remove(&mut self, selected: Selection) {
        for (id, how, error) in selected {
            if self.result.kept.remove(&id) {
                self.result.removed.push(Removal {
                    id,
                    round: self.round,
                    how,
                    code: error.code,
                    message: error.message,
                });
            }
        }
    }
}

/// The mutants that the errors select. A span in a mutant arm selects that
/// mutant. A span in the rest of a switch selects all enabled mutants of the
/// site, because the switch itself does not compile there.
fn select(
    files: &[CrateFile],
    rendered: &[Rendered],
    enabled: &BTreeSet<u64>,
    errors: &[CompilerError],
) -> Selection {
    let mut selected = Vec::new();
    for error in errors {
        for chain in &error.spans {
            let Some((index, owner)) = owner_of_chain(files, rendered, chain) else {
                continue;
            };
            match owner {
                Owner::Arm(id) => selected.push((id, Via::Arm, error.clone())),
                Owner::Site(site) => {
                    let mutants = &files[index].found.sites[site].mutants;
                    selected.extend(
                        mutants
                            .iter()
                            .filter(|mutant| enabled.contains(&mutant.id))
                            .map(|mutant| (mutant.id, Via::Site, error.clone())),
                    );
                }
            }
        }
    }
    selected
}

/// Walks from a span up through its macro call sites to the first span that is
/// in a generated file, and returns the owner of that span.
fn owner_of_chain(
    files: &[CrateFile],
    rendered: &[Rendered],
    chain: &[SpanAt],
) -> Option<(usize, Owner)> {
    for span in chain {
        let path = span.file.replace('\\', "/");
        let Some(key) = normalize(Path::new(&path)) else {
            continue;
        };
        if let Some(index) = files.iter().position(|file| file.path == key) {
            let owner = rendered[index].owner_of(span.start..span.end)?;
            return Some((index, owner));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use std::fmt::Write;

    use super::*;
    use crate::mutation::sites::find_sites;
    use crate::parse::Edition;

    const PATH: &str = "src/lib.rs";

    fn files(source: &str) -> Vec<CrateFile> {
        let Ok(found) = find_sites(source, Edition::Edition2021, PATH) else {
            panic!("the source parses");
        };
        vec![CrateFile {
            path: PATH.to_owned(),
            original: source.to_owned(),
            edition: Edition::Edition2021,
            found,
            is_root: true,
            under_cfg: false,
        }]
    }

    fn error_at(rendered: &str, needle: &str) -> CompilerError {
        let Some(start) = rendered.find(needle) else {
            panic!("{needle} is not in {rendered}");
        };
        CompilerError {
            code: Some("E0001".to_owned()),
            message: "broken".to_owned(),
            spans: vec![vec![SpanAt {
                file: PATH.to_owned(),
                start,
                end: start + needle.len(),
            }]],
        }
    }

    fn fail(errors: Vec<CompilerError>) -> Build {
        Build {
            success: false,
            errors,
        }
    }

    fn pass() -> Build {
        Build {
            success: true,
            errors: Vec::new(),
        }
    }

    #[test]
    fn clean_build_takes_one_round() {
        let files = files("fn f(a: u8) -> bool { a == 1 }");
        let result = run_loop(&files, |_| pass());
        assert_eq!(result.rounds, Some(1));
        assert_eq!(result.kept.len(), 1);
        assert!(result.removed.is_empty());
    }

    #[test]
    fn error_in_an_arm_removes_only_that_mutant() {
        let files = files("fn f(a: u8) -> bool { a < 1 }");
        let result = run_loop(&files, |rendered| {
            if rendered[0].text.contains("a >= 1") {
                fail(vec![error_at(&rendered[0].text, "a >= 1")])
            } else {
                pass()
            }
        });
        assert_eq!(result.rounds, Some(2));
        assert_eq!(result.kept.len(), 1);
        assert_eq!(result.removed.len(), 1);
        assert_eq!(result.removed[0].how, Via::Arm);
        assert_eq!(result.removed[0].code.as_deref(), Some("E0001"));
    }

    #[test]
    fn error_in_the_switch_removes_each_mutant_of_the_site() {
        let files = files("fn f(a: u8) -> bool { a < 1 }");
        let result = run_loop(&files, |rendered| {
            if rendered[0]
                .text
                .contains("crate::__fallow_schema::active() {")
            {
                fail(vec![error_at(
                    &rendered[0].text,
                    "crate::__fallow_schema::active() {",
                )])
            } else {
                pass()
            }
        });
        assert_eq!(result.rounds, Some(2));
        assert!(result.kept.is_empty());
        assert_eq!(result.removed.len(), 2);
        assert!(
            result
                .removed
                .iter()
                .all(|removal| removal.how == Via::Site)
        );
    }

    #[test]
    fn nested_error_keeps_the_outer_site() {
        let files = files("fn f(a: u8) -> bool { a == 1 && a == 2 }");
        let result = run_loop(&files, |rendered| {
            if rendered[0].text.contains("a != 2") {
                fail(vec![error_at(&rendered[0].text, "a != 2")])
            } else {
                pass()
            }
        });
        assert_eq!(result.kept.len(), 2);
        assert_eq!(result.removed.len(), 1);
    }

    #[test]
    fn macro_span_walks_up_to_its_call_site() {
        let files = files("fn f(a: u8) -> bool { a == 1 }");
        let result = run_loop(&files, |rendered| {
            if !rendered[0].text.contains("a != 1") {
                return pass();
            }
            let mut error = error_at(&rendered[0].text, "a != 1");
            error.spans[0].insert(
                0,
                SpanAt {
                    file: "/rustc/abc/library/core/src/macros/mod.rs".to_owned(),
                    start: 1,
                    end: 2,
                },
            );
            fail(vec![error])
        });
        assert_eq!(result.removed[0].how, Via::Arm);
        assert_eq!(result.rounds, Some(2));
    }

    /// A build that fails while `broken` is in the enabled set, with an error
    /// that no region holds. The key of a mutant is its arm text.
    fn bisection_build(broken: &'static str) -> impl FnMut(&[Rendered]) -> Build {
        move |rendered| {
            if rendered[0].text.contains(broken) {
                fail(vec![error_at(&rendered[0].text, "fn f")])
            } else {
                pass()
            }
        }
    }

    #[test]
    fn unmapped_error_starts_the_bisection_and_finds_the_mutant() {
        let source = "fn f(a: u8) -> bool { a == 1 }\nfn g(a: u8) -> bool { a == 2 }\n\
                      fn h(a: u8) -> bool { a == 3 }\nfn i(a: u8) -> bool { a == 4 }\n\
                      fn j(a: u8) -> bool { a == 5 }\n";
        let files = files(source);
        let result = run_loop(&files, bisection_build("a != 4"));
        assert_eq!(result.kept.len(), 4);
        assert_eq!(result.removed.len(), 1);
        assert_eq!(result.removed[0].how, Via::Bisection);
        assert_eq!(result.removed[0].code.as_deref(), Some("E0001"));
        let id = files[0].found.sites[3].mutants[0].id;
        assert_eq!(result.removed[0].id, id);
        // One full round, the prefixes, and one full round that passes.
        let Some(rounds) = result.rounds else {
            panic!("the schema compiles");
        };
        assert!((3..=6).contains(&rounds), "{rounds}");
        assert_eq!(rounds as usize, result.log.len());
        assert!(result.log.iter().any(|record| record.phase == "bisection"));
    }

    #[test]
    fn loop_stops_at_the_round_cap() {
        let mut source = String::new();
        for index in 0..64 {
            // A write to a `String` cannot fail.
            let _ = writeln!(source, "fn f{index}(a: u8) -> bool {{ a == {index} }}");
        }
        let files = files(&source);
        let mut calls = 0;
        let result = run_loop(&files, |rendered| {
            calls += 1;
            fail(vec![error_at(&rendered[0].text, "fn f0")])
        });
        assert_eq!(result.rounds, None);
        assert_eq!(calls, 15);
        assert_eq!(result.log.len(), MAX_ROUNDS as usize);
    }

    #[test]
    fn error_without_a_span_fails_at_the_cap_or_when_no_mutant_stays() {
        let files = files("fn f(a: u8) -> bool { a == 1 }");
        let result = run_loop(&files, |_| fail(vec![CompilerError::default()]));
        assert_eq!(result.rounds, None);
        assert!(result.kept.is_empty());
        assert!(!result.last_errors.is_empty());
    }

    #[test]
    fn probed_mutants_are_read_from_the_messages() {
        let build = fail(vec![
            CompilerError {
                message: format!("cannot find value `{PROBE_PREFIX}ab12` in this scope"),
                ..CompilerError::default()
            },
            CompilerError {
                message: "another error".to_owned(),
                ..CompilerError::default()
            },
        ]);
        assert_eq!(probed_mutants(&build), [0xab12].into());
    }

    #[test]
    fn cargo_lines_are_parsed() {
        let stdout = concat!(
            "not json\n",
            r#"{"reason":"compiler-artifact","package_id":"x"}"#,
            "\n",
            r#"{"reason":"compiler-message","message":{"level":"warning","message":"w","code":null,"spans":[]}}"#,
            "\n",
            r#"{"reason":"compiler-message","message":{"level":"error","message":"aborting due to 1 previous error","code":null,"spans":[]}}"#,
            "\n",
            r#"{"reason":"compiler-message","message":{"level":"error","message":"mismatched\nsecond","code":{"code":"E0308"},"spans":[{"file_name":"a/b.rs","byte_start":1,"byte_end":2,"is_primary":false,"expansion":null},{"file_name":"c.rs","byte_start":3,"byte_end":4,"is_primary":true,"expansion":{"span":{"file_name":"d.rs","byte_start":5,"byte_end":6,"is_primary":true,"expansion":null}}}]}}"#,
            "\n",
            r#"{"reason":"build-finished","success":false}"#,
            "\n",
        );
        let build = parse_build(stdout);
        assert!(!build.success);
        assert_eq!(build.errors.len(), 1);
        let error = &build.errors[0];
        assert_eq!(error.code.as_deref(), Some("E0308"));
        assert_eq!(error.message, "mismatched");
        assert_eq!(error.spans.len(), 1);
        assert_eq!(error.spans[0].len(), 2);
        assert_eq!(error.spans[0][1].file, "d.rs");
        assert!(parse_build(r#"{"reason":"build-finished","success":true}"#).success);
    }
}
