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
fn help(path: &[&str]) -> String {
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

/// Each `cargo fallow ...` command text in the skill: the text in backticks
/// and the lines of the code blocks.
fn commands(text: &str) -> Vec<String> {
    let mut found = Vec::new();
    for line in text.lines() {
        for (index, part) in line.split('`').enumerate() {
            let in_code = index % 2 == 1 || !line.contains('`');
            if in_code && let Some(start) = part.find("cargo fallow") {
                found.push(part[start..].trim().to_owned());
            }
        }
    }
    found
}

#[test]
fn each_command_and_flag_of_the_skill_exists() {
    let mut cache: BTreeMap<Vec<String>, String> = BTreeMap::new();
    let found = commands(SKILL);
    assert!(found.len() > 20, "the skill names few commands: {found:?}");
    for command in found {
        let words: Vec<&str> = command.split_whitespace().skip(2).collect();
        let mut path: Vec<String> = Vec::new();
        for word in &words {
            let text = cache
                .entry(path.clone())
                .or_insert_with(|| help(&path.iter().map(String::as_str).collect::<Vec<_>>()))
                .clone();
            if word.starts_with('-') {
                let flag = word.split('=').next().unwrap_or(word);
                let named = text
                    .split(|c: char| c.is_whitespace() || c == ',')
                    .any(|token| token == flag);
                assert!(named, "`{command}`: `{flag}` is not a flag of `{}`", path.join(" "));
            } else if subcommands(&text).iter().any(|name| name == word) && path.len() < 2 {
                path.push((*word).to_owned());
            }
        }
    }
}

#[test]
fn an_unknown_flag_is_found() {
    let text = help(&["deps"]);
    assert!(!text.split_whitespace().any(|token| token == "--no-such-flag"));
    assert_eq!(
        commands("Run `cargo fallow deps --trace x`."),
        ["cargo fallow deps --trace x"]
    );
    assert!(subcommands(&help(&["hooks"])).contains(&"install".to_owned()));
}
