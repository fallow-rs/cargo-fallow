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

/// The base address of the documentation.
const DOCS: &str = "https://github.com/fallow-rs/cargo-fallow#";

/// The flags of `explain`.
#[derive(Debug, Clone, Args)]
pub struct ExplainArgs {
    /// The rule, for example `unused-dependency`. A `_` and a `-` are the same.
    rule: String,
    /// The output format: `human` or `json`.
    #[arg(long, value_enum, default_value_t = Format::Human)]
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
        docs: "https://github.com/fallow-rs/cargo-fallow#usage",
    },
    Explanation {
        rule: "unused-dependency",
        check: "deps",
        severity: "error",
        meaning: "A [dependencies] entry that no code of the package uses.",
        example: "`serde = \"1\"` in [dependencies], with no `serde` name in the code.",
        fix: "Remove the entry, or run `cargo fallow fix`. Add the key to `ignored` when a tool needs it with no name in the code.",
        docs: "https://github.com/fallow-rs/cargo-fallow#unused-dependencies",
    },
    Explanation {
        rule: "unused-dev-dependency",
        check: "deps",
        severity: "error",
        meaning: "A [dev-dependencies] entry that no test, bench or example uses.",
        example: "`proptest = \"1\"` in [dev-dependencies], with no test that names it.",
        fix: "Remove the entry, or run `cargo fallow fix`.",
        docs: "https://github.com/fallow-rs/cargo-fallow#unused-dependencies",
    },
    Explanation {
        rule: "unused-build-dependency",
        check: "deps",
        severity: "error",
        meaning: "A [build-dependencies] entry that the build script does not use.",
        example: "`cc = \"1\"` in [build-dependencies], with a build.rs that does not name `cc`.",
        fix: "Remove the entry, or run `cargo fallow fix`.",
        docs: "https://github.com/fallow-rs/cargo-fallow#unused-dependencies",
    },
    Explanation {
        rule: "unused-workspace-dependency",
        check: "deps",
        severity: "error",
        meaning: "A [workspace.dependencies] entry that no member inherits with `workspace = true`.",
        example: "`regex = \"1\"` in the root [workspace.dependencies], with no member entry `regex = { workspace = true }`.",
        fix: "Remove the entry from the root Cargo.toml, or run `cargo fallow fix`.",
        docs: "https://github.com/fallow-rs/cargo-fallow#unused-dependencies",
    },
    Explanation {
        rule: "misplaced-dependency",
        check: "deps",
        severity: "error",
        meaning: "A [dependencies] entry that only tests, benches or examples use.",
        example: "`tempfile` in [dependencies], with a use only in tests/it.rs.",
        fix: "Move the entry to [dev-dependencies], or run `cargo fallow fix`.",
        docs: "https://github.com/fallow-rs/cargo-fallow#unused-dependencies",
    },
    Explanation {
        rule: "unused-optional-dependency",
        check: "deps",
        severity: "warn",
        meaning: "An optional dependency that no code uses. A feature enables it.",
        example: "`json = [\"dep:serde_json\"]`, with no `serde_json` name in the code.",
        fix: "Remove the entry and its feature values, or use the dependency under its feature.",
        docs: "https://github.com/fallow-rs/cargo-fallow#unused-dependencies",
    },
    Explanation {
        rule: "feature-only-dependency",
        check: "deps",
        severity: "warn",
        meaning: "A dependency that no code uses, and that a feature of the package configures.",
        example: "`std = [\"log/std\"]`, with no `log` name in the code.",
        fix: "Remove the entry and its feature values. When the feature must set the features of the dependency, add it to `ignored`.",
        docs: "https://github.com/fallow-rs/cargo-fallow#unused-dependencies",
    },
    Explanation {
        rule: "redundant-ignore",
        check: "deps",
        severity: "warn",
        meaning: "An entry of an `ignored` list that hides no finding.",
        example: "`ignored = [\"serde\"]`, while the code uses `serde`.",
        fix: "Remove the entry from the `ignored` list.",
        docs: "https://github.com/fallow-rs/cargo-fallow#unused-dependencies",
    },
    Explanation {
        rule: "unused-file",
        check: "files",
        severity: "warn",
        meaning: "A `.rs` file in a target folder that no target compiles: no `mod` item, `#[path]` attribute or `include!` call reaches it.",
        example: "src/old.rs, with no `mod old;` in src/lib.rs.",
        fix: "Delete the file, or declare it with a `mod` item.",
        docs: "https://github.com/fallow-rs/cargo-fallow#unused-files",
    },
    Explanation {
        rule: "dead_public",
        check: "visibility",
        severity: "warn",
        meaning: "A `pub` item that no code of the workspace reaches.",
        example: "`pub fn helper()` in a library that no crate calls.",
        fix: "Make the item `pub(crate)`, or run `cargo fallow fix`. The compiler then reports it as dead code when no code of the crate uses it.",
        docs: "https://github.com/fallow-rs/cargo-fallow#visibility",
    },
    Explanation {
        rule: "unnecessary_public",
        check: "visibility",
        severity: "warn",
        meaning: "A `pub` item that only its own crate uses.",
        example: "`pub fn parse()` in a library, called only from the same library.",
        fix: "Make the item `pub(crate)`, or run `cargo fallow fix`.",
        docs: "https://github.com/fallow-rs/cargo-fallow#visibility",
    },
    Explanation {
        rule: "unnecessary_restricted_visibility",
        check: "visibility",
        severity: "warn",
        meaning: "A `pub(crate)`, `pub(super)` or `pub(in path)` item that only its own module uses.",
        example: "`pub(crate) fn step()` that only its own module calls.",
        fix: "Remove the visibility keyword, or run `cargo fallow fix`.",
        docs: "https://github.com/fallow-rs/cargo-fallow#visibility",
    },
    Explanation {
        rule: "crate-dependency",
        check: "boundaries",
        severity: "warn",
        meaning: "A workspace package depends on a workspace package that a crate rule denies. The finding has the name of the rule.",
        example: "A rule `from = [\"core\"]`, `deny = [\"cli\"]`, and `cli` in the [dependencies] of `core`.",
        fix: "Remove the dependency, or change the rule in [workspace.metadata.fallow.boundaries].",
        docs: "https://github.com/fallow-rs/cargo-fallow#structure-checks",
    },
    Explanation {
        rule: "source-pattern",
        check: "boundaries",
        severity: "warn",
        meaning: "A file under the paths of a source rule contains a pattern that the rule denies. The finding has the name of the rule.",
        example: "A rule that denies `std::process::exit` in crates/core, and a call in crates/core/src/run.rs.",
        fix: "Move the code to the package that owns the pattern, or change the rule.",
        docs: "https://github.com/fallow-rs/cargo-fallow#structure-checks",
    },
    Explanation {
        rule: "module-path",
        check: "boundaries",
        severity: "warn",
        meaning: "A module uses a module of the same package that a module rule denies. The finding has the name of the rule.",
        example: "A rule `from = [\"crate::parse\"]`, `deny = [\"crate::render\"]`, and `use crate::render::x` in src/parse.rs.",
        fix: "Move the code, or change the rule.",
        docs: "https://github.com/fallow-rs/cargo-fallow#structure-checks",
    },
    Explanation {
        rule: "grown-file",
        check: "size",
        severity: "warn",
        meaning: "A file over the line limit has more lines than its version at the base.",
        example: "src/big.rs has 2400 lines at the base and 2450 lines now, with a limit of 2000.",
        fix: "Split the file, or move the new code to another file.",
        docs: "https://github.com/fallow-rs/cargo-fallow#structure-checks",
    },
    Explanation {
        rule: "new-large-file",
        check: "size",
        severity: "warn",
        meaning: "A new file is over the line limit.",
        example: "A new file src/gen.rs with 2100 lines, with a limit of 2000.",
        fix: "Split the file. A generated file with a generator header does not count.",
        docs: "https://github.com/fallow-rs/cargo-fallow#structure-checks",
    },
    Explanation {
        rule: "complexity",
        check: "complexity",
        severity: "warn",
        meaning: "A function over the cyclomatic or the cognitive complexity limit.",
        example: "A `fn` with a cyclomatic complexity of 25, with a limit of 20.",
        fix: "Split the function, or put `// fallow-ignore-next-line complexity` above it.",
        docs: "https://github.com/fallow-rs/cargo-fallow#structure-checks",
    },
    Explanation {
        rule: "code-duplication",
        check: "dupes",
        severity: "warn",
        meaning: "A group of two or more copies of the same normalized token sequence.",
        example: "The same 60-token loop in src/a.rs and src/b.rs.",
        fix: "Move the code to one function, or put `// fallow-ignore-next-line code-duplication` above a copy.",
        docs: "https://github.com/fallow-rs/cargo-fallow#structure-checks",
    },
    Explanation {
        rule: "surviving-mutant",
        check: "mutants",
        severity: "error",
        meaning: "A small change in the source that each test passes with: no test detects it.",
        example: "`a > b` becomes `a >= b`, and each test still passes.",
        fix: "Add a test that fails with the change, or remove the code when no test can need it.",
        docs: "https://github.com/fallow-rs/cargo-fallow#mutation-testing",
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
    if matches!(args.format, Format::Sarif | Format::Codeclimate) {
        return Err(CliError::new(
            ErrorCode::InvalidArgument,
            "`explain` has no SARIF or CodeClimate output",
        )
        .with_help("Use `--format human` or `--format json`."));
    }
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
/// finding in `findings`. A rule with no entry, for example the name of a
/// boundary rule, uses the entry of its check kind when one exists.
pub fn meta<'a>(rules: impl IntoIterator<Item = &'a str>) -> Result<Value, CliError> {
    let mut found = Map::new();
    for name in rules {
        if let Some(rule) = find(name) {
            found.insert(name.to_owned(), to_value(rule)?);
        }
    }
    let mut meta = Map::new();
    meta.insert("rules".to_owned(), Value::Object(found));
    meta.insert("docs".to_owned(), Value::from(DOCS));
    Ok(Value::Object(meta))
}

#[cfg(test)]
mod tests {
    use super::*;

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
                "complexity",
                "code-duplication",
                "surviving-mutant",
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
        let meta = meta(["unused-file", "my-boundary-rule"]).unwrap_or_default();
        assert_eq!(meta["rules"]["unused-file"]["check"], "files");
        assert!(meta["rules"].get("my-boundary-rule").is_none());
    }
}
