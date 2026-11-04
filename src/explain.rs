//! The meaning of each rule: `cargo fallow explain <rule>` and `--explain`.
//!
//! One table holds each rule of each check, with its meaning, an example and
//! the fix. An agent reads a rule from the tool and needs no documentation
//! site. A test walks the rule lists of the checks, so a new rule needs an
//! entry here.

use std::fmt::Write as _;

use clap::Args;
use fallow_report_protocol::exit_code::EXIT_OK;
use serde::Serialize;
use serde_json::{Map, Value};

use crate::cli::Format;
use crate::error::{CliError, ErrorCode};
use crate::render;
use crate::section::{pretty, to_value};

/// The base address of the documentation pages. `README.md` there is the index.
const DOCS: &str = "https://github.com/fallow-rs/cargo-fallow/blob/main/docs/";

/// The flags of `explain`.
#[derive(Debug, Clone, Args)]
pub struct ExplainArgs {
    /// The rule, for example `unused-dependency`. A `_` and a `-` are the same.
    rule: String,
    /// The output format: `human` or `json`.
    #[arg(long, default_value = "human", value_parser = crate::cli::human_or_json())]
    format: Format,
}

impl ExplainArgs {
    /// The output format.
    pub const fn format(&self) -> Format {
        self.format
    }
}

/// The explanation of one rule. The field order is the wire order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, schemars::JsonSchema)]
#[schemars(rename = "ExplainRule")]
pub struct Explanation {
    /// The rule name, as it appears in a finding.
    pub rule: &'static str,
    /// The check that reports it.
    pub check: &'static str,
    /// The severity of a finding: `error` or `warn`.
    pub severity: &'static str,
    /// What the rule finds.
    pub meaning: &'static str,
    /// A short example of a finding.
    pub example: &'static str,
    /// What to do.
    pub fix: &'static str,
    /// The documentation address.
    pub docs: &'static str,
}

/// Each rule of each check.
pub const RULES: &[Explanation] = &[
    Explanation {
        rule: "parse-error",
        check: "parse",
        severity: "error",
        meaning: "A target root file does not parse, or it nests too deep to parse with a safe stack.",
        example: "src/lib.rs has `fn f( {`: the parser stops at the missing `)`.",
        fix: "Fix the syntax error. The other checks cannot judge a file that does not parse.",
        docs: "https://github.com/fallow-rs/cargo-fallow/blob/main/docs/check.md",
    },
    Explanation {
        rule: "unused-dependency",
        check: "deps",
        severity: "error",
        meaning: "A [dependencies] entry that no code of the package uses.",
        example: "`serde = \"1\"` in [dependencies], with no `serde` name in the code.",
        fix: "Remove the entry, or run `cargo fallow fix`. Add the key to `ignored` when a tool needs it with no name in the code.",
        docs: "https://github.com/fallow-rs/cargo-fallow/blob/main/docs/dependencies.md",
    },
    Explanation {
        rule: "unused-dev-dependency",
        check: "deps",
        severity: "error",
        meaning: "A [dev-dependencies] entry that no test, bench or example uses.",
        example: "`proptest = \"1\"` in [dev-dependencies], with no test that names it.",
        fix: "Remove the entry, or run `cargo fallow fix`.",
        docs: "https://github.com/fallow-rs/cargo-fallow/blob/main/docs/dependencies.md",
    },
    Explanation {
        rule: "unused-build-dependency",
        check: "deps",
        severity: "error",
        meaning: "A [build-dependencies] entry that the build script does not use.",
        example: "`cc = \"1\"` in [build-dependencies], with a build.rs that does not name `cc`.",
        fix: "Remove the entry, or run `cargo fallow fix`.",
        docs: "https://github.com/fallow-rs/cargo-fallow/blob/main/docs/dependencies.md",
    },
    Explanation {
        rule: "unused-workspace-dependency",
        check: "deps",
        severity: "error",
        meaning: "A [workspace.dependencies] entry that no member inherits with `workspace = true`.",
        example: "`regex = \"1\"` in the root [workspace.dependencies], with no member entry `regex = { workspace = true }`.",
        fix: "Remove the entry from the root Cargo.toml, or run `cargo fallow fix`.",
        docs: "https://github.com/fallow-rs/cargo-fallow/blob/main/docs/dependencies.md",
    },
    Explanation {
        rule: "misplaced-dependency",
        check: "deps",
        severity: "error",
        meaning: "A [dependencies] entry that only tests, benches or examples use.",
        example: "`tempfile` in [dependencies], with a use only in tests/it.rs.",
        fix: "Move the entry to [dev-dependencies], or run `cargo fallow fix`.",
        docs: "https://github.com/fallow-rs/cargo-fallow/blob/main/docs/dependencies.md",
    },
    Explanation {
        rule: "unused-optional-dependency",
        check: "deps",
        severity: "warn",
        meaning: "An optional dependency that no code uses. A feature enables it.",
        example: "`json = [\"dep:serde_json\"]`, with no `serde_json` name in the code.",
        fix: "Remove the entry and its feature values, or use the dependency under its feature.",
        docs: "https://github.com/fallow-rs/cargo-fallow/blob/main/docs/dependencies.md",
    },
    Explanation {
        rule: "feature-only-dependency",
        check: "deps",
        severity: "warn",
        meaning: "A dependency that no code uses, and that a feature of the package configures.",
        example: "`std = [\"log/std\"]`, with no `log` name in the code.",
        fix: "Remove the entry and its feature values. Add the key to `ignored` when the feature must set the features of the dependency.",
        docs: "https://github.com/fallow-rs/cargo-fallow/blob/main/docs/dependencies.md",
    },
    Explanation {
        rule: "redundant-ignore",
        check: "deps",
        severity: "warn",
        meaning: "An entry of an `ignored` list that hides no finding.",
        example: "`ignored = [\"serde\"]`, while the code uses `serde`.",
        fix: "Remove the entry from the `ignored` list.",
        docs: "https://github.com/fallow-rs/cargo-fallow/blob/main/docs/dependencies.md",
    },
    Explanation {
        rule: "unused-file",
        check: "files",
        severity: "warn",
        meaning: "A `.rs` file in a target folder that no target compiles. No `mod` item, `#[path]` attribute or `include!` call reaches it.",
        example: "src/old.rs, with no `mod old;` in src/lib.rs.",
        fix: "Delete the file, or declare it with a `mod` item.",
        docs: "https://github.com/fallow-rs/cargo-fallow/blob/main/docs/unused-files.md",
    },
    Explanation {
        rule: "dead_public",
        check: "visibility",
        severity: "warn",
        meaning: "A `pub` item that no code of the workspace reaches.",
        example: "`pub fn helper()` in a library that no crate calls.",
        fix: "Make the item `pub(crate)`, or run `cargo fallow fix`. When no code of the crate uses it, the compiler then reports dead code.",
        docs: "https://github.com/fallow-rs/cargo-fallow/blob/main/docs/visibility.md",
    },
    Explanation {
        rule: "unnecessary_public",
        check: "visibility",
        severity: "warn",
        meaning: "A `pub` item that only its own crate uses.",
        example: "`pub fn parse()` in a library, called only from the same library.",
        fix: "Make the item `pub(crate)`, or run `cargo fallow fix`.",
        docs: "https://github.com/fallow-rs/cargo-fallow/blob/main/docs/visibility.md",
    },
    Explanation {
        rule: "unnecessary_restricted_visibility",
        check: "visibility",
        severity: "warn",
        meaning: "A `pub(crate)`, `pub(super)` or `pub(in path)` item that only its own module uses.",
        example: "`pub(crate) fn step()` that only its own module calls.",
        fix: "Remove the visibility keyword, or run `cargo fallow fix`.",
        docs: "https://github.com/fallow-rs/cargo-fallow/blob/main/docs/visibility.md",
    },
    Explanation {
        rule: "boundaries",
        check: "boundaries",
        severity: "warn",
        meaning: "A rule of [workspace.metadata.fallow.boundaries] found a violation. The finding has the name of the rule. The kind is crate-dependency, source-pattern or module-path.",
        example: "A source rule `no-exit` that denies `std::process::exit` in crates/core, and a call in crates/core/src/run.rs.",
        fix: "Move the code or the dependency, or change the rule.",
        docs: "https://github.com/fallow-rs/cargo-fallow/blob/main/docs/structure.md",
    },
    Explanation {
        rule: "crate-dependency",
        check: "boundaries",
        severity: "warn",
        meaning: "A workspace package depends on a workspace package that a crate rule denies. The finding has the name of the rule.",
        example: "A rule `from = [\"core\"]`, `deny = [\"cli\"]`, and `cli` in the [dependencies] of `core`.",
        fix: "Remove the dependency, or change the rule in [workspace.metadata.fallow.boundaries].",
        docs: "https://github.com/fallow-rs/cargo-fallow/blob/main/docs/structure.md",
    },
    Explanation {
        rule: "source-pattern",
        check: "boundaries",
        severity: "warn",
        meaning: "A file under the paths of a source rule contains a pattern that the rule denies. The finding has the name of the rule.",
        example: "A rule that denies `std::process::exit` in crates/core, and a call in crates/core/src/run.rs.",
        fix: "Move the code to the package that owns the pattern, or change the rule.",
        docs: "https://github.com/fallow-rs/cargo-fallow/blob/main/docs/structure.md",
    },
    Explanation {
        rule: "module-path",
        check: "boundaries",
        severity: "warn",
        meaning: "A module uses a module of the same package that a module rule denies. The finding has the name of the rule.",
        example: "A rule `from = [\"crate::parse\"]`, `deny = [\"crate::render\"]`, and `use crate::render::x` in src/parse.rs.",
        fix: "Move the code, or change the rule.",
        docs: "https://github.com/fallow-rs/cargo-fallow/blob/main/docs/structure.md",
    },
    Explanation {
        rule: "grown-file",
        check: "size",
        severity: "warn",
        meaning: "A file over the line limit has more lines than its version at the base.",
        example: "src/big.rs has 2400 lines at the base and 2450 lines now, with a limit of 2000.",
        fix: "Split the file, or move the new code to another file.",
        docs: "https://github.com/fallow-rs/cargo-fallow/blob/main/docs/structure.md",
    },
    Explanation {
        rule: "new-large-file",
        check: "size",
        severity: "warn",
        meaning: "A new file is over the line limit.",
        example: "A new file src/gen.rs with 2100 lines, with a limit of 2000.",
        fix: "Split the file. A file with a generator header or an `@generated` comment does not count.",
        docs: "https://github.com/fallow-rs/cargo-fallow/blob/main/docs/structure.md",
    },
    Explanation {
        rule: "complexity",
        check: "complexity",
        severity: "warn",
        meaning: "A function over the cyclomatic or the cognitive complexity limit.",
        example: "A `fn` with a cyclomatic complexity of 25, with a limit of 20.",
        fix: "Split the function, or put `// fallow-ignore-next-line complexity` above it.",
        docs: "https://github.com/fallow-rs/cargo-fallow/blob/main/docs/structure.md",
    },
    Explanation {
        rule: "code-duplication",
        check: "dupes",
        severity: "warn",
        meaning: "A group of two or more copies of the same normalized token sequence.",
        example: "The same 60-token loop in src/a.rs and src/b.rs.",
        fix: "Move the code to one function, or put `// fallow-ignore-next-line code-duplication` above a copy.",
        docs: "https://github.com/fallow-rs/cargo-fallow/blob/main/docs/structure.md",
    },
    Explanation {
        rule: "stale-suppression",
        check: "any",
        severity: "warn",
        meaning: "A suppression comment with a kind of a check that ran hides no finding.",
        example: "`// fallow-ignore-next-line visibility` above a private function.",
        fix: "Remove the comment. The finding that it hid is gone.",
        docs: "https://github.com/fallow-rs/cargo-fallow/blob/main/docs/adoption.md#suppressions",
    },
    Explanation {
        rule: "surviving-mutant",
        check: "mutants",
        severity: "warn",
        meaning: "A small change in the source that each test passes with: no test detects it.",
        example: "`a > b` becomes `a >= b`, and each test still passes.",
        fix: "Add a test that fails with the change, or remove the code when no test can need it.",
        docs: "https://github.com/fallow-rs/cargo-fallow/blob/main/docs/mutation-testing.md",
    },
];

/// The rule with this name. A `_` and a `-` are the same.
pub fn find(name: &str) -> Option<&'static Explanation> {
    let wanted = name.replace('_', "-");
    RULES
        .iter()
        .find(|rule| rule.rule.replace('_', "-") == wanted)
}

/// Runs `explain` and returns the process exit code.
pub fn run(args: &ExplainArgs) -> Result<u8, CliError> {
    let Some(rule) = find(&args.rule) else {
        let known: Vec<&str> = RULES.iter().map(|rule| rule.rule).collect();
        return Err(CliError::new(
            ErrorCode::InvalidArgument,
            format!("`{}` is not a rule", args.rule),
        )
        .with_help(format!("The rules are: {}.", known.join(", "))));
    };
    let text = match args.format {
        Format::Json => pretty(rule)?,
        _ => human(rule),
    };
    render::write_stdout(text.trim_end()).map_err(|err| CliError::output(&err))?;
    Ok(EXIT_OK)
}

fn human(rule: &Explanation) -> String {
    let mut text = format!("{} ({}, {})\n", rule.rule, rule.check, rule.severity);
    let _ = writeln!(text, "\n{}", rule.meaning);
    let _ = writeln!(text, "\nExample: {}", rule.example);
    let _ = writeln!(text, "Fix: {}", rule.fix);
    let _ = writeln!(text, "Docs: {}", rule.docs);
    text
}

/// The `_meta` member of a JSON report: the explanation of each rule with a
/// finding, keyed by the rule of the finding. A boundaries finding has the
/// name of its rule in the configuration, so it gets the `boundaries` entry.
pub fn meta<'a>(findings: impl IntoIterator<Item = (&'a str, &'a str)>) -> Result<Value, CliError> {
    let mut found = Map::new();
    for (check, name) in findings {
        let entry = if check == "boundaries" {
            find("boundaries")
        } else {
            find(name)
        };
        if let Some(entry) = entry {
            found.insert(name.to_owned(), to_value(entry)?);
        }
    }
    let mut meta = Map::new();
    meta.insert("rules".to_owned(), Value::Object(found));
    meta.insert("docs".to_owned(), Value::from(format!("{DOCS}README.md")));
    Ok(Value::Object(meta))
}

#[cfg(test)]
#[expect(clippy::expect_used, reason = "a test fails loudly")]
mod tests {
    use super::*;

    /// Each `docs` address names a page and an anchor that the repository has.
    #[test]
    fn each_docs_address_names_a_page_of_the_repository() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("docs");
        assert!(
            root.join("README.md").is_file(),
            "docs/README.md is missing"
        );
        for rule in RULES {
            let page = rule.docs.strip_prefix(DOCS).expect("a docs address");
            let (file, anchor) = page.split_once('#').unwrap_or((page, ""));
            let text = std::fs::read_to_string(root.join(file))
                .unwrap_or_else(|error| panic!("{}: docs/{file}: {error}", rule.rule));
            if !anchor.is_empty() {
                let heading = anchor.replace('-', " ");
                assert!(
                    text.lines().any(|line| line.starts_with('#')
                        && line.trim_start_matches('#').trim().to_lowercase() == heading),
                    "{}: docs/{file} has no heading for #{anchor}",
                    rule.rule
                );
            }
        }
    }

    #[test]
    fn each_rule_of_each_check_has_an_entry() {
        let mut names: Vec<String> = crate::deps::report::Rule::ALL
            .iter()
            .map(|rule| rule.as_str().to_owned())
            .collect();
        names.extend(
            crate::visibility::FindingKind::ALL
                .iter()
                .map(|kind| kind.label().to_owned()),
        );
        names.extend(
            crate::structure::size::Kind::ALL
                .iter()
                .map(|kind| crate::structure::size::rule(*kind).to_owned()),
        );
        names.extend(
            crate::structure::boundaries::Kind::ALL
                .iter()
                .map(|kind| kind.as_str().to_owned()),
        );
        names.extend(
            [
                crate::files::RULE,
                crate::gates::RustGateName::ParseError.as_str(),
                crate::structure::complexity::RULE,
                crate::structure::dupes::DUPES_RULE,
                crate::render::mutants::SURVIVING_RULE,
                crate::suppress::STALE_RULE,
                "boundaries",
            ]
            .map(str::to_owned),
        );
        for name in &names {
            assert!(find(name).is_some(), "`{name}` has no entry");
        }
        for rule in RULES {
            assert!(
                names.iter().any(|name| name == rule.rule),
                "`{}` names no rule",
                rule.rule
            );
            assert!(matches!(rule.severity, "error" | "warn"), "{}", rule.rule);
            assert!(rule.docs.starts_with(DOCS), "{}", rule.rule);
        }
    }

    /// A new variant of a rule enum breaks this match, so the `ALL` lists
    /// above get the variant.
    #[expect(
        dead_code,
        reason = "the match only checks that each variant is listed"
    )]
    const fn exhaustive(
        deps: crate::deps::report::Rule,
        kind: crate::visibility::FindingKind,
        size: crate::structure::size::Kind,
        boundary: crate::structure::boundaries::Kind,
    ) -> usize {
        use crate::deps::report::Rule as D;
        use crate::structure::boundaries::Kind as B;
        use crate::structure::size::Kind as S;
        use crate::visibility::FindingKind as V;
        let deps = match deps {
            D::UnusedDependency
            | D::UnusedDevDependency
            | D::UnusedBuildDependency
            | D::UnusedWorkspaceDependency
            | D::MisplacedDependency
            | D::UnusedOptionalDependency
            | D::FeatureOnlyDependency
            | D::RedundantIgnore => crate::deps::report::Rule::ALL.len(),
        };
        let kind = match kind {
            V::DeadPublic | V::UnnecessaryPublic | V::UnnecessaryRestricted => V::ALL.len(),
        };
        let size = match size {
            S::GrownFile | S::NewLargeFile => S::ALL.len(),
        };
        let boundary = match boundary {
            B::CrateDependency | B::SourcePattern | B::ModulePath => B::ALL.len(),
        };
        deps + kind + size + boundary
    }

    #[test]
    fn lookup_takes_both_separators() {
        assert_eq!(
            find("dead-public").map(|rule| rule.rule),
            Some("dead_public")
        );
        assert_eq!(
            find("unused_dependency").map(|rule| rule.check),
            Some("deps")
        );
        assert!(find("nothing").is_none());
    }

    #[test]
    fn meta_lists_the_rules_with_an_entry() {
        let meta = meta([
            ("files", "unused-file"),
            ("boundaries", "my-boundary-rule"),
            ("other", "nothing"),
        ])
        .expect("the meta member builds");
        assert_eq!(meta["rules"]["unused-file"]["check"], "files");
        assert_eq!(meta["rules"]["my-boundary-rule"]["rule"], "boundaries");
        assert!(meta["rules"].get("nothing").is_none());
    }
}
