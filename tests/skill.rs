//! The user skill in `skills/cargo-fallow/SKILL.md` names only commands and
//! flags that exist. Each `cargo fallow ...` text in the skill goes through
//! the `--help` of the command that it names.
#![expect(
    clippy::expect_used,
    reason = "a test fails loudly when its setup does not work"
)]

use std::collections::BTreeMap;

use assert_cmd::Command;

const SKILL: &str = include_str!("../skills/cargo-fallow/SKILL.md");

/// The help text of a command path, for example `["hooks", "install"]`.
fn help(path: &[String]) -> String {
    let output = Command::cargo_bin("cargo-fallow")
        .expect("the binary exists")
        .args(path)
        .arg("--help")
        .output()
        .expect("run the binary");
    assert!(output.status.success(), "{path:?}: {output:?}");
    String::from_utf8(output.stdout).expect("UTF-8")
}

/// The subcommands that a help text lists.
fn subcommands(help: &str) -> Vec<String> {
    help.lines()
        .skip_while(|line| line.trim() != "Commands:")
        .skip(1)
        .take_while(|line| line.starts_with("  "))
        .filter_map(|line| line.split_whitespace().next())
        .map(str::to_owned)
        .collect()
}

/// Each `cargo fallow ...` command text in a Markdown text: each line of a
/// fenced code block, and each code span. A span can go over a line break.
fn commands(text: &str) -> Vec<String> {
    let mut prose = String::new();
    let mut found = Vec::new();
    let mut fenced = false;
    for line in text.lines() {
        if line.trim_start().starts_with("```") {
            fenced = !fenced;
            prose.push('\n');
        } else if fenced {
            let code = line.split(" #").next().unwrap_or(line);
            found.extend(command_in(code));
        } else {
            prose.push_str(line);
            prose.push('\n');
        }
    }
    for span in prose.split('`').skip(1).step_by(2) {
        found.extend(command_in(&span.replace('\n', " ")));
    }
    found
}

/// The `cargo fallow ...` command in a piece of code, if any.
fn command_in(code: &str) -> Option<String> {
    let start = code.find("cargo fallow")?;
    Some(
        code[start..]
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" "),
    )
}

/// The problems of one command: an unknown subcommand or an unknown flag.
fn problems(command: &str, cache: &mut BTreeMap<Vec<String>, String>) -> Vec<String> {
    let mut problems = Vec::new();
    let mut path: Vec<String> = Vec::new();
    let mut seen_flag = false;
    for word in command.split_whitespace().skip(2) {
        let text = cache
            .entry(path.clone())
            .or_insert_with(|| help(&path))
            .clone();
        if word.starts_with('-') {
            seen_flag = true;
            let flag = word.split('=').next().unwrap_or(word);
            let named = text
                .split(|c: char| c.is_whitespace() || c == ',')
                .any(|token| token == flag);
            if !named {
                problems.push(format!(
                    "`{command}`: `{flag}` is not a flag of `{}`",
                    path.join(" ")
                ));
            }
        } else if !seen_flag && !word.starts_with('<') {
            let names = subcommands(&text);
            if names.iter().any(|name| name == word) {
                path.push(word.to_owned());
            } else if !names.is_empty() {
                problems.push(format!(
                    "`{command}`: `{word}` is not a subcommand of `{}`",
                    path.join(" ")
                ));
            }
        }
    }
    problems
}

#[test]
fn each_command_and_flag_of_the_skill_exists() {
    let mut cache = BTreeMap::new();
    let found = commands(SKILL);
    assert!(found.len() > 20, "the skill names few commands: {found:?}");
    let problems: Vec<String> = found
        .iter()
        .flat_map(|command| problems(command, &mut cache))
        .collect();
    assert!(problems.is_empty(), "{problems:#?}");
}

#[test]
fn a_wrong_subcommand_or_flag_is_a_problem() {
    let mut cache = BTreeMap::new();
    for bad in [
        "cargo fallow chekk --format json",
        "cargo fallow deps --no-such-flag",
        "cargo fallow hooks instal --target agent",
        "cargo fallow hooks install --no-such-flag",
    ] {
        assert!(!problems(bad, &mut cache).is_empty(), "{bad}");
    }
    for good in [
        "cargo fallow --version",
        "cargo fallow explain <rule> --format json",
        "cargo fallow check -p app --base main",
        "cargo fallow hooks install --target=agent",
    ] {
        assert_eq!(problems(good, &mut cache), Vec::<String>::new(), "{good}");
    }
}

#[test]
fn commands_come_from_code_blocks_and_wrapped_spans() {
    let text = "Run `cargo fallow fix --dry-run\n  --quiet`, then `cargo fallow fix --yes`.\n\n```bash\ncargo fallow deps --trace x   # a note\n```\n";
    assert_eq!(
        commands(text),
        [
            "cargo fallow deps --trace x",
            "cargo fallow fix --dry-run --quiet",
            "cargo fallow fix --yes",
        ]
    );
}
