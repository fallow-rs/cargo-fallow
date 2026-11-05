//! The command classifier of the agent gate: is a shell command a `git commit`
//! or a `git push`?
//!
//! The gate runs on each shell command of an agent. Only a git write starts
//! a check. The classifier has two passes:
//!
//! 1. A word scan. Control operators split the text into simple commands.
//!    After a `git` word, the scan skips the global options of git and reads
//!    the subcommand. `git log commit.txt` is not a write.
//! 2. A loose pass for text with quotes, escapes or expansions, which the scan
//!    can split wrong (`git -C "a b" commit`, `sh -c 'git push'`). It drops
//!    the quote and escape characters and finds a `git` word with a later
//!    `commit` or `push` word, or with an expansion such as `git $cmd`.
//!
//! A wrong match only adds a check, and a check of clean code passes. So each
//! pass errs on the side of a match.
//!
//! The scan also collects the folders that the write can target: a `cd <dir>`
//! before it, `-C <dir>`, `--work-tree <dir>`, the parent of a `--git-dir`
//! that ends in `.git`, and the `GIT_WORK_TREE` and `GIT_DIR` prefixes. The
//! gate checks each of these folders in addition to the session folder.

/// The kind of a git write.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum WriteKind {
    /// `git commit`.
    Commit,
    /// `git push`.
    Push,
}

/// A git write in a shell command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitWrite {
    /// Each kind of write in the command, sorted, without repeats.
    pub kinds: Vec<WriteKind>,
    /// The folders that a write can target, as the command spells them. A
    /// relative folder is relative to the session folder.
    pub targets: Vec<String>,
}

/// The global options of git whose value is the next word.
const VALUE_OPTIONS: [&str; 7] = [
    "-c",
    "--namespace",
    "--config-env",
    "--super-prefix",
    "--exec-path",
    "--list-cmds",
    "--attr-source",
];

/// The characters that separate two simple commands.
const OPERATORS: [char; 6] = [';', '|', '&', '(', ')', '\n'];

/// Returns the git write in `command`, or `None` when the command does not
/// write with git.
pub fn git_write(command: &str) -> Option<GitWrite> {
    let joined = command.replace("\\\n", " ");
    let mut scan = Scan::default();
    for segment in joined.split(OPERATORS) {
        scan.segment(segment);
    }
    if scan.kinds.is_empty() {
        scan.kinds.extend(loose_kinds(&joined));
    }
    if scan.kinds.is_empty() {
        return None;
    }
    scan.kinds.sort();
    scan.kinds.dedup();
    scan.targets.dedup();
    Some(GitWrite {
        kinds: scan.kinds,
        targets: scan.targets,
    })
}

/// The state of the word scan over the simple commands of one command text.
#[derive(Default)]
struct Scan {
    kinds: Vec<WriteKind>,
    targets: Vec<String>,
    /// The folder of the last `cd`, relative to the session folder.
    cd_dir: String,
}

impl Scan {
    /// Scans one simple command.
    fn segment(&mut self, segment: &str) {
        let words: Vec<&str> = segment.split_whitespace().collect();
        if let ["cd", dir, ..] = words.as_slice() {
            self.cd_dir = join(&self.cd_dir, &unquote(dir));
        }
        let mut index = 0;
        while index < words.len() {
            let word = words[index];
            index += 1;
            if word != "git" {
                self.location_prefix(word);
                continue;
            }
            index = self.git_words(&words, index);
        }
    }

    /// Reads a `GIT_WORK_TREE=` or a `GIT_DIR=` prefix.
    fn location_prefix(&mut self, word: &str) {
        if let Some(value) = word.strip_prefix("GIT_WORK_TREE=")
            && !value.is_empty()
        {
            self.targets.push(join(&self.cd_dir, &unquote(value)));
        } else if let Some(value) = word.strip_prefix("GIT_DIR=")
            && let Some(parent) = git_dir_parent(&unquote(value))
        {
            self.targets.push(join(&self.cd_dir, &parent));
        }
    }

    /// Reads the words after one `git` word, from `index`. Returns the index of
    /// the next word to scan.
    fn git_words(&mut self, words: &[&str], mut index: usize) -> usize {
        let mut dir = self.cd_dir.clone();
        while let Some(&word) = words.get(index) {
            let value = words.get(index + 1).map(|value| unquote(value));
            match word {
                "commit" | "push" => {
                    self.kinds.push(if word == "commit" {
                        WriteKind::Commit
                    } else {
                        WriteKind::Push
                    });
                    if !dir.is_empty() {
                        self.targets.push(dir);
                    }
                    return index + 1;
                }
                "-C" => {
                    dir = join(&dir, &value.unwrap_or_default());
                    index += 2;
                }
                "--work-tree" | "--git-dir" => {
                    self.git_location(&dir, word, &value.unwrap_or_default());
                    index += 2;
                }
                _ if VALUE_OPTIONS.contains(&word) => index += 2,
                _ if word.starts_with("--work-tree=") || word.starts_with("--git-dir=") => {
                    let (name, value) = word.split_once('=').unwrap_or_default();
                    self.git_location(&dir, name, &unquote(value));
                    index += 1;
                }
                _ if word.starts_with('-') => index += 1,
                // Another subcommand: the scan goes on after it.
                _ => return index + 1,
            }
        }
        index
    }

    /// Reads the value of `--work-tree` or `--git-dir`.
    fn git_location(&mut self, dir: &str, name: &str, value: &str) {
        if name == "--work-tree" {
            self.targets.push(join(dir, value));
        } else if let Some(parent) = git_dir_parent(value) {
            self.targets.push(join(dir, &parent));
        }
    }
}

/// The parent of a git folder that ends in `.git`, or `None` for another
/// value.
fn git_dir_parent(value: &str) -> Option<String> {
    let trimmed = value.trim_end_matches('/');
    let parent = trimmed.strip_suffix(".git")?;
    if !(parent.is_empty() || parent.ends_with('/')) {
        return None;
    }
    let parent = parent.trim_end_matches('/');
    Some(if parent.is_empty() {
        if trimmed.starts_with('/') { "/" } else { "." }.to_owned()
    } else {
        parent.to_owned()
    })
}

/// Removes one pair of outer quotes.
fn unquote(word: &str) -> String {
    for quote in ['\'', '"'] {
        if word.len() >= 2 && word.starts_with(quote) && word.ends_with(quote) {
            return word[1..word.len() - 1].to_owned();
        }
    }
    word.to_owned()
}

/// Joins a folder to a base folder. An absolute folder replaces the base.
fn join(base: &str, dir: &str) -> String {
    if dir.starts_with('/') || base.is_empty() {
        dir.to_owned()
    } else {
        format!("{base}/{dir}")
    }
}

/// The loose pass: the kinds of write in a command with quotes, escapes or
/// expansions. An expansion after `git` counts as both kinds, because it can
/// be either.
fn loose_kinds(command: &str) -> Vec<WriteKind> {
    if !command.contains(['\'', '"', '\\', '$', '`']) {
        return Vec::new();
    }
    let text: String = command
        .chars()
        .filter(|c| !matches!(c, '\'' | '"' | '\\'))
        .map(|c| if matches!(c, '`' | '(' | ')') { ' ' } else { c })
        .collect();
    let mut kinds = Vec::new();
    for segment in text.split([';', '|', '&', '\n']) {
        let mut seen_git = false;
        for word in segment.split_whitespace() {
            if word == "git" {
                seen_git = true;
            } else if seen_git && word.starts_with('$') {
                kinds.extend([WriteKind::Commit, WriteKind::Push]);
            } else if seen_git && word == "commit" {
                kinds.push(WriteKind::Commit);
            } else if seen_git && word == "push" {
                kinds.push(WriteKind::Push);
            }
        }
    }
    kinds
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(command: &str) -> Vec<WriteKind> {
        git_write(command)
            .map(|write| write.kinds)
            .unwrap_or_default()
    }

    fn targets(command: &str) -> Vec<String> {
        git_write(command)
            .map(|write| write.targets)
            .unwrap_or_default()
    }

    #[test]
    fn plain_writes_are_found() {
        assert_eq!(kinds("git commit -m x"), [WriteKind::Commit]);
        assert_eq!(kinds("git push origin main"), [WriteKind::Push]);
        assert_eq!(
            kinds("git add . && git commit -m x && git push"),
            [WriteKind::Commit, WriteKind::Push]
        );
    }

    #[test]
    fn global_options_before_the_subcommand_are_skipped() {
        for command in [
            "git -c user.name=x commit",
            "git -C dir commit",
            "git --no-pager commit",
            "git --git-dir=/x/.git commit",
            "git -c 'user.name=Jane Doe' push",
            "git \\\n  -C dir \\\n  commit -m x",
        ] {
            assert!(!kinds(command).is_empty(), "{command}");
        }
    }

    #[test]
    fn other_commands_are_not_writes() {
        for command in [
            "git status",
            "git log commit.txt",
            "git diff -- push.rs",
            "cargo test",
            "echo commit",
            "",
        ] {
            assert_eq!(git_write(command), None, "{command}");
        }
    }

    #[test]
    fn shell_forms_go_to_the_loose_pass() {
        for command in [
            "sh -c 'git commit -m x'",
            "eval \"git push\"",
            "g''it commit",
            "git $SUB",
            "git -C \"$(pwd)\" commit",
        ] {
            assert!(!kinds(command).is_empty(), "{command}");
        }
    }

    #[test]
    fn targets_come_from_cd_and_the_location_options() {
        assert_eq!(targets("git -C a/b commit"), ["a/b"]);
        assert_eq!(targets("cd sub && git -C x commit"), ["sub/x"]);
        assert_eq!(targets("git -C /abs push"), ["/abs"]);
        assert_eq!(targets("git --work-tree=../w commit"), ["../w"]);
        assert_eq!(targets("git --git-dir ../w/.git commit"), ["../w"]);
        assert_eq!(targets("GIT_DIR=../w/.git git commit"), ["../w"]);
        assert_eq!(targets("GIT_WORK_TREE=../w git commit"), ["../w"]);
        // The word scan cannot read a quoted folder with a space. The loose
        // pass finds the write, and the session folder is the one check.
        assert!(targets("git -C 'a b' commit").is_empty());
        assert!(targets("git commit").is_empty());
    }

    #[test]
    fn a_git_dir_without_the_git_name_names_no_target() {
        assert_eq!(git_dir_parent("/x/repo.git"), None);
        assert_eq!(git_dir_parent(".git"), Some(".".to_owned()));
        assert_eq!(git_dir_parent("/.git/"), Some("/".to_owned()));
    }
}
