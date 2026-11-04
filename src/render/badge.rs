//! The `--format badge` output of `check`: a flat SVG badge with the grade.
//!
//! The badge has the shape and the colors of the fallow badge for
//! JavaScript, so both badges look the same in a README. A run with no
//! health score gives a grey `no score` badge.

use crate::health::Health;

/// The text on the left side of the badge.
const LABEL: &str = "fallow rust";

/// The message and the color of a run with no score.
const NO_SCORE: (&str, &str) = ("no score", "#9f9f9f");

/// The SVG badge of a health part of a `check` report.
pub fn health(health: Option<&Health>) -> String {
    let scored = match health {
        Some(Health::Score(score)) => score.score.zip(score.grade),
        _ => None,
    };
    match scored {
        Some((score, grade)) => {
            #[expect(
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                reason = "the score is in 0..=100"
            )]
            let whole = score.clamp(0.0, 100.0) as u32;
            render(LABEL, &format!("{grade} ({whole})"), grade_color(grade))
        }
        None => render(LABEL, NO_SCORE.0, NO_SCORE.1),
    }
}

/// The color of each grade.
fn grade_color(grade: &str) -> &'static str {
    match grade {
        "A" => "#4c1",
        "B" => "#97ca00",
        "C" => "#dfb317",
        "D" => "#fe7d37",
        _ => "#e05d44",
    }
}

/// Escapes a text for an XML attribute or element.
fn xml_escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('"', "&quot;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// The width of a character in Verdana at 11 px, the font of the badge.
#[expect(
    clippy::match_same_arms,
    reason = "a table with one width for each character"
)]
const fn char_width(c: char) -> f64 {
    match c {
        ' ' => 3.87,
        '!' => 4.33,
        '"' => 5.05,
        '#' => 9.0,
        '$' => 6.99,
        '%' => 11.84,
        '&' => 7.99,
        '\'' => 2.95,
        '(' | ')' => 5.0,
        '*' => 6.99,
        '+' => 9.0,
        ',' => 4.0,
        '-' => 5.0,
        '.' => 4.0,
        '/' => 5.0,
        '0' | '1' | '2' | '3' | '4' | '5' | '6' | '7' | '8' | '9' => 6.99,
        ':' | ';' => 5.0,
        '<' | '=' | '>' => 9.0,
        '?' => 6.0,
        '@' => 11.0,
        'A' => 7.52,
        'B' => 7.54,
        'C' => 7.68,
        'D' => 8.48,
        'E' => 6.96,
        'F' => 6.32,
        'G' => 8.53,
        'H' => 8.27,
        'I' => 4.63,
        'J' => 5.0,
        'K' => 7.62,
        'L' => 6.12,
        'M' => 9.27,
        'N' => 8.23,
        'O' => 8.66,
        'P' => 6.63,
        'Q' => 8.66,
        'R' => 7.65,
        'S' => 7.52,
        'T' => 6.78,
        'U' => 8.05,
        'V' => 7.52,
        'W' => 10.88,
        'X' => 7.54,
        'Y' => 6.77,
        'Z' => 7.54,
        '[' | '\\' | ']' => 5.0,
        '^' => 9.0,
        '_' | '`' => 6.99,
        'a' => 6.61,
        'b' => 6.85,
        'c' => 5.73,
        'd' => 6.85,
        'e' => 6.55,
        'f' => 3.87,
        'g' => 6.85,
        'h' => 6.96,
        'i' => 3.02,
        'j' => 3.79,
        'k' => 6.51,
        'l' => 3.02,
        'm' => 10.7,
        'n' => 6.96,
        'o' => 6.68,
        'p' | 'q' => 6.85,
        'r' => 4.69,
        's' => 5.73,
        't' => 4.33,
        'u' => 6.96,
        'v' => 6.51,
        'w' => 9.0,
        'x' | 'y' => 6.51,
        'z' => 5.78,
        '{' => 6.98,
        '|' => 5.0,
        '}' => 6.98,
        '~' => 9.0,
        _ => 10.7,
    }
}

/// The width of a text in whole pixels, rounded up to an odd number, the
/// same as the badges of shields.io.
fn text_width(text: &str) -> u32 {
    if text.is_empty() {
        return 0;
    }
    let raw: f64 = text.chars().map(char_width).sum();
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "a badge text is short and its width is positive"
    )]
    let whole = raw as u32;
    if whole.is_multiple_of(2) {
        whole + 1
    } else {
        whole
    }
}

/// A suffix for the element ids, so two badges on one page do not share ids.
fn id_suffix(label: &str, message: &str) -> String {
    let hash = label
        .bytes()
        .chain(message.bytes())
        .fold(0_u32, |hash, byte| {
            hash.wrapping_mul(31).wrapping_add(u32::from(byte))
        });
    format!("{hash:x}")
}

/// A flat badge with a label and a message.
fn render(label: &str, message: &str, color: &str) -> String {
    const PADDING: u32 = 5;
    const HEIGHT: u32 = 20;
    let label_width = text_width(label);
    let message_width = text_width(message);
    let left = label_width + 2 * PADDING;
    let right = message_width + 2 * PADDING;
    let total = left + right;
    let label_x = 10 * (1 + label_width / 2 + PADDING);
    let message_x = 10 * (left - 1 + message_width / 2 + PADDING);
    let label_length = 10 * label_width;
    let message_length = 10 * message_width;
    let suffix = id_suffix(label, message);
    let label = xml_escape(label);
    let message = xml_escape(message);
    let font = "Verdana,Geneva,DejaVu Sans,sans-serif";
    format!(
        r##"<svg xmlns="http://www.w3.org/2000/svg" width="{total}" height="{HEIGHT}" role="img" aria-label="{label}: {message}">
<title>{label}: {message}</title>
<linearGradient id="s-{suffix}" x2="0" y2="100%">
<stop offset="0" stop-color="#bbb" stop-opacity=".1"/>
<stop offset="1" stop-opacity=".1"/>
</linearGradient>
<clipPath id="r-{suffix}">
<rect width="{total}" height="{HEIGHT}" rx="3" fill="#fff"/>
</clipPath>
<g clip-path="url(#r-{suffix})">
<rect width="{left}" height="{HEIGHT}" fill="#555"/>
<rect x="{left}" width="{right}" height="{HEIGHT}" fill="{color}"/>
<rect width="{total}" height="{HEIGHT}" fill="url(#s-{suffix})"/>
</g>
<g fill="#fff" text-anchor="middle" font-family="{font}" text-rendering="geometricPrecision" font-size="110">
<text aria-hidden="true" x="{label_x}" y="150" fill="#010101" fill-opacity=".3" transform="scale(.1)" textLength="{label_length}">{label}</text>
<text x="{label_x}" y="140" transform="scale(.1)" fill="#fff" textLength="{label_length}">{label}</text>
<text aria-hidden="true" x="{message_x}" y="150" fill="#010101" fill-opacity=".3" transform="scale(.1)" textLength="{message_length}">{message}</text>
<text x="{message_x}" y="140" transform="scale(.1)" fill="#fff" textLength="{message_length}">{message}</text>
</g>
</svg>"##
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_grade_has_its_color_and_text() {
        for (grade, color) in [
            ("A", "#4c1"),
            ("B", "#97ca00"),
            ("C", "#dfb317"),
            ("D", "#fe7d37"),
            ("F", "#e05d44"),
        ] {
            let svg = render(LABEL, &format!("{grade} (80)"), grade_color(grade));
            assert!(svg.contains(&format!("fill=\"{color}\"")), "{grade}: {svg}");
            assert!(
                svg.contains(&format!("aria-label=\"fallow rust: {grade} (80)\"")),
                "{svg}"
            );
            assert!(svg.starts_with("<svg ") && svg.ends_with("</svg>"));
        }
    }

    #[test]
    fn a_run_with_no_score_gives_the_grey_badge() {
        let svg = health(None);
        assert!(svg.contains("fallow rust: no score"), "{svg}");
        assert!(svg.contains("fill=\"#9f9f9f\""), "{svg}");
    }

    #[test]
    fn widths_are_odd_and_the_text_is_escaped() {
        assert_eq!(text_width(""), 0);
        assert_eq!(text_width("A") % 2, 1);
        assert_eq!(xml_escape("<a&\"b>"), "&lt;a&amp;&quot;b&gt;");
    }

    #[test]
    fn the_badge_of_one_grade_is_stable() {
        let svg = render(LABEL, "A (92)", grade_color("A"));
        let expected_width = 2 * 5 + text_width(LABEL) + 2 * 5 + text_width("A (92)");
        assert!(
            svg.starts_with(&format!(
                "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"{expected_width}\" height=\"20\""
            )),
            "{svg}"
        );
        assert_eq!(svg.matches("<text").count(), 4);
    }
}
