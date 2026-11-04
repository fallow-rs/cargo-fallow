//! The intra-doc links of a documentation text.
//!
//! rustdoc resolves a link path from the module of the doc comment. The forms
//! are `[path]`, `` [`path`] ``, `[text](path)`, `[text][path]` and a
//! reference definition `[label]: path`. A link can have a disambiguator such
//! as `fn@`, and a suffix such as `()` or `!`. A code block holds no links.
//!
//! The scan gives credit when it cannot decide: each bracket text that is a
//! valid path counts as a link, also when rustdoc does not resolve it.

/// One link path: the segments, and true when the path starts with `::`.
pub type LinkPath = (Vec<String>, bool);

/// The link paths of a documentation text, each with the zero-based index of
/// its line. The text can still have the comment markers `///`, `//!` and `*`
/// at the start of each line.
pub fn link_paths(text: &str) -> Vec<(usize, LinkPath)> {
    let mut paths = Vec::new();
    let mut fence: Option<&str> = None;
    for (index, raw) in text.lines().enumerate() {
        let line = strip_marker(raw);
        let trimmed = line.trim_start();
        if let Some(open) = fence {
            if trimmed.starts_with(open) {
                fence = None;
            }
            continue;
        }
        if let Some(open) = ["```", "~~~"]
            .into_iter()
            .find(|open| trimmed.starts_with(open))
        {
            fence = Some(open);
            continue;
        }
        if let Some(target) = definition(trimmed) {
            paths.extend(path_of(target).map(|path| (index, path)));
            continue;
        }
        let mut found = Vec::new();
        scan_line(line, &mut found);
        paths.extend(found.into_iter().map(|path| (index, path)));
    }
    paths
}

/// The line without its comment marker.
fn strip_marker(line: &str) -> &str {
    let trimmed = line.trim_start();
    for marker in ["///", "//!", "/**", "/*!", "*/", "*"] {
        if let Some(rest) = trimmed.strip_prefix(marker) {
            return rest;
        }
    }
    line
}

/// The target of a reference definition `[label]: target`.
fn definition(line: &str) -> Option<&str> {
    let rest = line.strip_prefix('[')?;
    let (_, after) = rest.split_once("]:")?;
    after.split_whitespace().next()
}

/// Adds the paths of the links in one line.
fn scan_line(line: &str, paths: &mut Vec<LinkPath>) {
    let mut rest = line;
    while let Some(open) = rest.find(['[', '`']) {
        if rest[open..].starts_with('`') {
            // A code span outside brackets holds no link.
            let run = rest[open..].len() - rest[open..].trim_start_matches('`').len();
            let ticks = &rest[open..open + run];
            let after = &rest[open + run..];
            rest = after.find(ticks).map_or(after, |end| &after[end + run..]);
            continue;
        }
        let after_open = &rest[open + 1..];
        let Some(close) = closing_bracket(after_open) else {
            return;
        };
        let content = &after_open[..close];
        let after = &after_open[close + 1..];
        if let Some(inner) = after.strip_prefix('(') {
            let end = inner.find(')').unwrap_or(inner.len());
            paths.extend(path_of(&inner[..end]));
            rest = &inner[end..];
        } else if let Some(inner) = after.strip_prefix('[') {
            let end = inner.find(']').unwrap_or(inner.len());
            let label = &inner[..end];
            paths.extend(path_of(if label.is_empty() { content } else { label }));
            rest = &inner[end..];
        } else {
            paths.extend(path_of(content));
            rest = after;
        }
    }
}

/// The offset of the `]` that closes a bracket. A `[` inside the text starts
/// a new bracket, so the text of the outer bracket is not a link.
fn closing_bracket(text: &str) -> Option<usize> {
    let close = text.find(']')?;
    match text.find('[') {
        Some(open) if open < close => None,
        _ => Some(close),
    }
}

/// The path of a link target, or `None` when the target is not a path, for
/// example a URL.
fn path_of(target: &str) -> Option<LinkPath> {
    let mut text = target.trim().trim_matches('`').trim();
    if let Some(at) = text.find('@') {
        let kind = &text[..at];
        if kind == "prim" || kind == "primitive" {
            return None;
        }
        text = &text[at + 1..];
    }
    if let Some(hash) = text.find('#') {
        text = &text[..hash];
    }
    for suffix in ["!()", "![]", "!{}", "()", "!"] {
        if let Some(stripped) = text.strip_suffix(suffix) {
            text = stripped;
            break;
        }
    }
    let text = without_generics(text)?;
    let (body, global) = match text.strip_prefix("::") {
        Some(rest) => (rest, true),
        None => (text.as_str(), false),
    };
    let segments: Vec<String> = body.split("::").map(str::to_owned).collect();
    segments
        .iter()
        .all(|segment| is_identifier(segment))
        .then_some((segments, global))
}

/// The text without generic arguments, as in `Vec<T>::new`. `None` when the
/// angle brackets do not balance.
fn without_generics(text: &str) -> Option<String> {
    let mut depth = 0_u32;
    let mut out = String::with_capacity(text.len());
    for character in text.chars() {
        match character {
            '<' => depth += 1,
            '>' => depth = depth.checked_sub(1)?,
            _ if depth == 0 => out.push(character),
            _ => {}
        }
    }
    (depth == 0).then_some(out)
}

fn is_identifier(segment: &str) -> bool {
    let name = segment.strip_prefix("r#").unwrap_or(segment);
    let mut chars = name.chars();
    chars
        .next()
        .is_some_and(|first| first == '_' || first.is_alphabetic())
        && chars.all(|character| character == '_' || character.is_alphanumeric())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paths(text: &str) -> Vec<String> {
        link_paths(text)
            .into_iter()
            .map(|(_, (segments, global))| {
                let joined = segments.join("::");
                if global {
                    format!("::{joined}")
                } else {
                    joined
                }
            })
            .collect()
    }

    #[test]
    fn each_link_form_gives_its_path() {
        let text = "/// [`a::b`], [c], [text](d::e), [text][f], [g][], [fn@h], \
                    [`i()`], [j!], [`Vec<T>::new`], [::k].\n///\n/// [label]: l::m\n";
        assert_eq!(
            paths(text),
            [
                "a::b", "c", "d::e", "f", "g", "h", "i", "j", "Vec::new", "::k", "l::m"
            ]
        );
    }

    #[test]
    fn each_path_has_the_index_of_its_line() {
        let lines: Vec<usize> = link_paths("/// [a]\n/// text\n/// [b] and [c]\n")
            .into_iter()
            .map(|(line, _)| line)
            .collect();
        assert_eq!(lines, [0, 2, 2]);
    }

    #[test]
    fn a_url_a_primitive_and_a_code_block_give_no_path() {
        let text = "//! [site](https://example.com), [prim@u8], [1], [a b], `[code]`\n\
                    //! ```\n//! let x = [linked];\n//! ```\n//! [after]\n";
        assert_eq!(paths(text), ["after"]);
    }
}
