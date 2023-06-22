use ratatui::style::Color;
use ratatui::style::Style;
use ratatui::text::Line;
use ratatui::text::Span;
use std::collections::VecDeque;

/// Builds the styled lines of a diff from `original` to `updated`. Shows common context from the
/// start to the end of the current section.
pub(super) fn diff_lines(original: &str, updated: &str) -> Vec<Line<'static>> {
    fn is_section_start(line: &str) -> bool {
        line.starts_with('[')
    }

    let mut lines = Vec::new();

    let mut common = VecDeque::new();
    let mut after_context = false;
    for diff in diff::lines(original, updated) {
        match diff {
            diff::Result::Both(s, _) => {
                if after_context {
                    if is_section_start(s) {
                        after_context = false;
                    } else {
                        lines.push(Line::from(format!(" {s}")));
                    }
                } else {
                    if is_section_start(s) {
                        common.clear();
                    }
                    common.push_back(s);
                }
            }
            diff::Result::Left(s) => {
                for line in common.drain(..) {
                    lines.push(Line::from(format!(" {line}")));
                }
                lines.push(Line::from(vec![Span::styled(
                    format!("-{s}"),
                    Style::default().fg(Color::Red),
                )]));
                after_context = true;
            }
            diff::Result::Right(s) => {
                for line in common.drain(..) {
                    lines.push(Line::from(format!(" {line}")));
                }
                lines.push(Line::from(vec![Span::styled(
                    format!("+{s}"),
                    Style::default().fg(Color::Green),
                )]));
                after_context = true;
            }
        }
    }
    lines
}

#[test]
fn test_diff_lines() {
    fn line_to_string(line: &Line) -> String {
        line.spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect::<Vec<_>>()
            .join("")
    }
    let lines = diff_lines(
        indoc::indoc! { r#"
            a = 1
            [section1]
            b = 2
            x = [
                "x1",
                "x2",
                "x3",
            ]
            [section2]
            c = 3
            d = 4
            e = 5
            f = 6
            g = 7
            h = 8
        "# },
        indoc::indoc! { r#"
            a = 1
            [section1]
            b = 2
            x = [
                "x1",
                "x2",
                "x3",
            ]
            [section2]
            c = 3
            d = 4
            e = 5
            f = 6
            g2 = 7.5
            h = 8
        "# },
    );
    let lines: Vec<_> = lines.iter().map(line_to_string).collect();
    let expected = vec![
        " [section2]",
        " c = 3",
        " d = 4",
        " e = 5",
        " f = 6",
        "-g = 7",
        "+g2 = 7.5",
        " h = 8",
        " ",
    ];
    assert_eq!(lines, expected);
}
