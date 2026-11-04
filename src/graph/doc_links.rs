//! The intra-doc links of a documentation text.
//!
//! rustdoc resolves a link path from the module of the doc comment. The forms
//! are `[path]`, `` [`path`] ``, `[text](path)`, `[text][path]` and a
//! reference definition `[label]: path`. A link can have a disambiguator such
//! as `fn@`, and a suffix such as `()` or `!`. A code block holds no links.
//!
//! The scan gives credit when it cannot decide: each bracket text that is a
//! valid path counts as a link, also when rustdoc does not resolve it. A link
//! can continue on the next line of its paragraph.

/// One link path: the segments, and true when the path starts with `::`.
pub type LinkPath = (Vec<String>, bool);

/// The link paths of a documentation text, each with the zero-based index of
/// its line. The text can still have the comment markers `///`, `//!` and `*`
/// at the start of each line.
pub fn link_paths(text: &str) -> Vec<(usize, LinkPath)> {
    let mut paths = Vec::new();
    let mut paragraph = Paragraph::default();
    let mut fence: Option<(char, usize)> = None;
    for (index, raw) in text.lines().enumerate() {
        let line = strip_marker(raw);
        let trimmed = line.trim_start();
        if let Some((mark, length)) = fence {
            if fence_of(trimmed).is_some_and(|(close, run)| close == mark && run >= length) {
                fence = None;
            }
            continue;
        }
        if let Some(open) = fence_of(trimmed) {
            paragraph.flush(&mut paths);
            fence = Some(open);
            continue;
        }
        if trimmed.is_empty() {
            paragraph.flush(&mut paths);
            continue;
        }
        // An indented code block starts after a blank line.
        if paragraph.text.is_empty() && line.starts_with("    ") {
            continue;
        }
        if let Some(target) = definition(trimmed) {
            paragraph.flush(&mut paths);
            paths.extend(path_of(target).map(|path| (index, path)));
            continue;
        }
        paragraph.push(index, line);
    }
    paragraph.flush(&mut paths);
    paths
}

/// The lines of one paragraph, joined with a space.
#[derive(Default)]
struct Paragraph {
    text: String,
    /// The offset in `text` where each line starts, and its line index.
    starts: Vec<(usize, usize)>,
}

impl Paragraph {
    fn push(&mut self, index: usize, line: &str) {
        if !self.text.is_empty() {
            self.text.push(' ');
        }
        self.starts.push((self.text.len(), index));
        self.text.push_str(line);
    }

    fn flush(&mut self, paths: &mut Vec<(usize, LinkPath)>) {
        for (offset, path) in scan(&self.text) {
            let at = self.starts.partition_point(|(start, _)| *start <= offset);
            let line = self
                .starts
                .get(at.saturating_sub(1))
                .map_or(0, |(_, line)| *line);
            paths.push((line, path));
        }
        self.text.clear();
        self.starts.clear();
    }
}

/// The line without its comment marker and the one space after it.
fn strip_marker(line: &str) -> &str {
    let trimmed = line.trim_start();
    for marker in ["///", "//!", "/**", "/*!", "*/", "*"] {
        if let Some(rest) = trimmed.strip_prefix(marker) {
            return rest.strip_prefix(' ').unwrap_or(rest);
        }
    }
    line
}

/// The mark and the length of a code fence that starts the line.
fn fence_of(line: &str) -> Option<(char, usize)> {
    let mark = line
        .chars()
        .next()
        .filter(|mark| matches!(mark, '`' | '~'))?;
    let run = line.len() - line.trim_start_matches(mark).len();
    (run >= 3).then_some((mark, run))
}

/// The target of a reference definition `[label]: target "title"`. A line
/// with other text after the target is no definition.
fn definition(line: &str) -> Option<&str> {
    let rest = line.strip_prefix('[')?;
    let (label, after) = rest.split_once("]:")?;
    if label.contains('[') {
        return None;
    }
    let after = after.trim();
    let (target, title) = after
        .split_once(char::is_whitespace)
        .map_or((after, ""), |(target, title)| (target, title.trim()));
    let titled = title.is_empty()
        || [('"', '"'), ('\'', '\''), ('(', ')')]
            .iter()
            .any(|(open, close)| title.starts_with(*open) && title.ends_with(*close));
    (!target.is_empty() && titled).then_some(target)
}

/// The paths of the links in one paragraph, with the offset of each link.
fn scan(text: &str) -> Vec<(usize, LinkPath)> {
    let mut paths = Vec::new();
    let mut at = 0;
    while let Some(found) = text[at..].find(['[', '`']) {
        let open = at + found;
        if text[open..].starts_with('`') {
            // A code span outside brackets holds no link.
            let run = text[open..].len() - text[open..].trim_start_matches('`').len();
            let ticks = &text[open..open + run];
            let after = open + run;
            at = text[after..]
                .find(ticks)
                .map_or(after, |end| after + end + run);
            continue;
        }
        let content_start = open + 1;
        let Some(close) = closing_bracket(&text[content_start..]) else {
            // An inner `[` starts a new bracket.
            at = content_start;
            continue;
        };
        let content = &text[content_start..content_start + close];
        let after = content_start + close + 1;
        let rest = &text[after..];
        if let Some(inner) = rest.strip_prefix('(') {
            let end = inner.find(')').unwrap_or(inner.len());
            paths.extend(path_of(inline_target(&inner[..end])).map(|path| (open, path)));
            at = after + 1 + end;
        } else if let Some(inner) = rest.strip_prefix('[') {
            let end = inner.find(']').unwrap_or(inner.len());
            let label = &inner[..end];
            let target = if label.is_empty() { content } else { label };
            paths.extend(path_of(target).map(|path| (open, path)));
            at = after + 1 + end;
        } else {
            paths.extend(path_of(content).map(|path| (open, path)));
            at = after;
        }
    }
    paths
}

/// The target of an inline link `(target "title")`, without the title and
/// without the angle brackets of `(<target>)`.
fn inline_target(text: &str) -> &str {
    let text = text.trim();
    if let Some(inner) = text.strip_prefix('<')
        && let Some(end) = inner.find('>')
    {
        return &inner[..end];
    }
    text.split_whitespace().next().unwrap_or("")
}

/// The offset of the `]` that closes a bracket, or `None` when a `[` comes
/// first.
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
    fn a_url_a_primitive_and_a_code_block_give_no_path() {
        let text = "//! [site](https://example.com), [prim@u8], [1], [a b], `[code]`\n\
                    //! ```\n//! let x = [linked];\n//! ```\n//! [after]\n";
        assert_eq!(paths(text), ["after"]);
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
    fn a_link_can_continue_on_the_next_line() {
        let found = link_paths("/// Text [over\n/// two lines](a::b) end.\n");
        assert_eq!(found, [(0, (vec!["a".to_owned(), "b".to_owned()], false))]);
    }

    #[test]
    fn titles_angle_brackets_and_open_brackets_keep_the_link() {
        let text = "/// [t](a \"title\") [u](<b>) [i and [`c`] and [d]: e\n";
        assert_eq!(paths(text), ["a", "b", "c", "d"]);
    }

    #[test]
    fn a_line_with_text_after_the_target_is_no_definition() {
        assert_eq!(
            paths("/// [`a`]: the item, see [`b`]: it is here.\n"),
            ["a", "b"]
        );
        assert_eq!(paths("/// [label]: c \"title\"\n"), ["c"]);
    }

    #[test]
    fn indented_code_and_long_fences_hold_no_link() {
        let text = "/// Text.\n///\n///     let x = [a];\n///\n/// ````\n/// ```\n/// [b]\n\
                    /// ````\n/// [c]\n";
        assert_eq!(paths(text), ["c"]);
    }

    #[test]
    fn odd_brackets_and_wide_characters_do_not_panic() {
        assert_eq!(paths("/// ]] [[[ [é] [`ü::ö`] [\n"), ["é", "ü::ö"]);
    }
}
