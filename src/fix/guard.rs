//! The `cargo check` guard of the dependency edits of `fix`.
//!
//! A macro of a crate outside the workspace can write code that names an
//! entry, and the analysis cannot read that code. When such a macro and the
//! entry do not depend on each other, the edit of the entry is guarded: `fix`
//! runs `cargo check` before and after the edits. A compiler error after the
//! edits that names the crate of a guarded edit leaves out that edit. An edit
//! without the guard never goes through this module.

use std::collections::BTreeSet;

use super::check::Diagnostic;
use super::deps::{Edit, WORKSPACE_SECTION};
use crate::workspace::Member;

/// The `skip_reason` of a guarded edit when the workspace does not build
/// before the edits, so the guard cannot judge the edit.
pub const BUILD_FAILS_BEFORE: &str = "build_fails_before";

/// The `skip_reason` of a guarded edit when the workspace builds before the
/// edits only without `--all-features`. That build does not compile the code
/// under a feature, so the guard cannot judge the edit.
pub const ALL_FEATURES_FAIL_BEFORE: &str = "all_features_fail_before";

/// The `skip_reason` of a guarded edit when `cargo metadata` fails after the
/// edits.
pub const LOAD_ERROR: &str = "load_error";

/// The crate names in a compiler message: the first segment of each path in
/// backticks. For example `unresolved import `gamma::f`` names `gamma`.
fn named_crates(message: &str) -> BTreeSet<&str> {
    message
        .split('`')
        .skip(1)
        .step_by(2)
        .filter_map(|name| name.trim_start_matches("::").split("::").next())
        .filter(|name| !name.is_empty())
        .collect()
}

/// The member that holds a file, by the longest folder of a member manifest.
/// A file outside the workspace has no member.
fn package_of<'a>(file: &str, members: &'a [Member]) -> Option<&'a str> {
    if file.is_empty() || file.starts_with('/') {
        return None;
    }
    members
        .iter()
        .filter(|member| {
            let folder = member
                .manifest_path
                .strip_suffix("Cargo.toml")
                .unwrap_or(&member.manifest_path);
            file.starts_with(folder)
        })
        .max_by_key(|member| member.manifest_path.len())
        .map(|member| member.name.as_str())
}

/// True when the edit belongs to `package`. A workspace edit belongs to each
/// member, and a message without a member matches each edit.
fn in_package(edit: &Edit, package: Option<&str>) -> bool {
    edit.package.is_none() || package.is_none() || edit.package.as_deref() == package
}

/// Moves each guarded edit that `hit` matches from `kept` to `left`, with the
/// reason and the compiler message.
pub fn take(
    kept: &mut Vec<Edit>,
    left: &mut Vec<Edit>,
    reason: &'static str,
    diagnostic: &str,
    hit: impl Fn(&Edit) -> bool,
) {
    let (out, stay): (Vec<_>, Vec<_>) = kept.drain(..).partition(|edit| edit.guarded && hit(edit));
    *kept = stay;
    left.extend(out.into_iter().map(|mut edit| {
        edit.skip_reason = Some(reason);
        edit.diagnostic = Some(diagnostic.to_owned());
        edit
    }));
}

/// Leaves out the guarded edits whose crate a compiler error names. The
/// error must come from a file of the package of the edit, when the file has
/// a member.
pub fn named(kept: &mut Vec<Edit>, errors: &[&str], members: &[Member]) -> Vec<Edit> {
    let mut left = Vec::new();
    for line in errors {
        let crates = named_crates(line);
        let package = super::error_place(line).and_then(|(file, _)| package_of(file, members));
        take(kept, &mut left, super::COMPILER_ERROR, line, |edit| {
            edit.code_name
                .as_deref()
                .is_some_and(|name| crates.contains(name))
                && in_package(edit, package)
        });
    }
    link(kept, &mut left);
    left
}

/// Leaves out the guarded edits of each package with a file in a compiler
/// error or a new warning. It runs when no message names an edit.
pub fn by_package(
    kept: &mut Vec<Edit>,
    errors: &[&str],
    warned: &[Diagnostic],
    members: &[Member],
) -> Vec<Edit> {
    let mut left = Vec::new();
    for line in errors {
        if let Some(package) =
            super::error_place(line).and_then(|(file, _)| package_of(file, members))
        {
            take(kept, &mut left, super::COMPILER_ERROR, line, |edit| {
                edit.package.as_deref() == Some(package)
            });
        }
    }
    for warning in warned {
        if let Some(package) = package_of(&warning.file, members) {
            take(
                kept,
                &mut left,
                super::COMPILER_WARNING,
                &warning.line(),
                |edit| edit.package.as_deref() == Some(package),
            );
        }
    }
    link(kept, &mut left);
    left
}

/// Leaves out each guarded edit. It runs when no message names an edit or a
/// file of a package, so the edits without the guard can still apply.
pub fn all(kept: &mut Vec<Edit>, reason: &'static str, diagnostic: &str) -> Vec<Edit> {
    let mut left = Vec::new();
    take(kept, &mut left, reason, diagnostic, |_| true);
    left
}

/// Leaves out the workspace edit of each key that a left-out member edit
/// inherits. Without the workspace entry, the member entry does not load.
fn link(kept: &mut Vec<Edit>, left: &mut Vec<Edit>) {
    let members: Vec<(String, &'static str, String)> = left
        .iter()
        .filter(|edit| edit.section != WORKSPACE_SECTION)
        .map(|edit| {
            (
                edit.dependency.clone(),
                edit.skip_reason.unwrap_or(super::COMPILER_ERROR),
                edit.diagnostic.clone().unwrap_or_default(),
            )
        })
        .collect();
    for (key, reason, diagnostic) in members {
        take(kept, left, reason, &diagnostic, |edit| {
            edit.section == WORKSPACE_SECTION && edit.dependency == key
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::deps::report::Rule;

    fn edit(package: Option<&str>, section: &str, key: &str, guarded: bool) -> Edit {
        Edit {
            kind: "remove-dependency",
            rule: Rule::UnusedDependency,
            package: package.map(str::to_owned),
            manifest_path: "app/Cargo.toml".to_owned(),
            section: section.to_owned(),
            platform: None,
            dependency: key.to_owned(),
            line: None,
            applied: false,
            guarded,
            skip_reason: None,
            diagnostic: None,
            code_name: guarded.then(|| key.replace('-', "_")),
        }
    }

    fn member(name: &str, manifest_path: &str) -> Member {
        Member {
            name: name.to_owned(),
            version: "0.1.0".to_owned(),
            manifest_path: manifest_path.to_owned(),
            targets: Vec::new(),
            dependencies: Vec::new(),
            links: Vec::new(),
            features: Vec::new(),
            published: false,
            consumer: false,
        }
    }

    fn keys(edits: &[Edit]) -> Vec<(Option<&str>, &str)> {
        edits
            .iter()
            .map(|edit| (edit.package.as_deref(), edit.section.as_str()))
            .collect()
    }

    #[test]
    fn an_error_names_the_crate_by_the_first_segment_of_a_path() {
        let crates = named_crates(
            "error[E0433]: failed to resolve: use of unresolved module or unlinked crate `my_dep`",
        );
        assert!(crates.contains("my_dep"));
        assert!(named_crates("unresolved import `::my_dep::f`").contains("my_dep"));
    }

    #[test]
    fn an_error_leaves_out_the_guarded_edit_of_its_package_and_the_inherited_entry() {
        let members = [
            member("app", "app/Cargo.toml"),
            member("lib", "lib/Cargo.toml"),
        ];
        let mut kept = vec![
            edit(Some("app"), "dependencies", "my-dep", true),
            edit(Some("lib"), "dependencies", "my-dep", true),
            edit(None, WORKSPACE_SECTION, "my-dep", true),
            edit(Some("app"), "dependencies", "other", false),
        ];
        let line = "app/src/lib.rs:3:5: error[E0432]: unresolved import `my_dep`";
        let left = named(&mut kept, &[line], &members);
        assert_eq!(
            keys(&left),
            [(Some("app"), "dependencies"), (None, WORKSPACE_SECTION)]
        );
        assert!(
            left.iter()
                .all(|edit| edit.diagnostic.as_deref() == Some(line))
        );
        assert_eq!(
            keys(&kept),
            [(Some("lib"), "dependencies"), (Some("app"), "dependencies")]
        );
    }

    #[test]
    fn an_edit_without_the_guard_stays_in_each_fallback() {
        let members = [member("app", "app/Cargo.toml")];
        let mut kept = vec![
            edit(Some("app"), "dependencies", "x", false),
            edit(Some("app"), "dependencies", "y", true),
        ];
        let line = "app/src/lib.rs:1:1: error[E0425]: cannot find value `z`";
        assert!(named(&mut kept, &[line], &members).is_empty());
        let left = by_package(&mut kept, &[line], &[], &members);
        assert_eq!(left.len(), 1);
        assert_eq!(left[0].dependency, "y");
        assert!(all(&mut kept, super::super::COMPILER_ERROR, line).is_empty());
        assert_eq!(kept.len(), 1);
    }
}
