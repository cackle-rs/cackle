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

/// Attempts, where possible to trim the supplied diff to less than `max_lines`.
pub(super) fn remove_excess_context(lines: &mut Vec<Line>, max_lines: usize) {
    struct ContextBlock {
        start: usize,
        length: usize,
        to_take: usize,
    }

    let mut blocks = Vec::new();
    let mut current_block = None;
    for (offset, line) in lines.iter().enumerate() {
        let is_context = line
            .spans
            .first()
            .map(|span| span.content.starts_with(' '))
            .unwrap_or(false);
        if is_context {
            current_block
                .get_or_insert(ContextBlock {
                    start: offset,
                    length: 0,
                    to_take: 0,
                })
                .length += 1;
        } else if let Some(block) = current_block.take() {
            blocks.push(block);
        }
    }
    blocks.extend(current_block);

    let mut to_reclaim = (lines.len() as isize).saturating_sub(max_lines as isize);
    while to_reclaim > 0 {
        if let Some(block) = blocks.iter_mut().max_by_key(|b| b.length - b.to_take) {
            if block.length - block.to_take == 0 {
                // We've run out of context to remove.
                break;
            }
            if block.to_take > 0 {
                to_reclaim -= 1;
            }
            block.to_take += 1;
        } else {
            // We have no blocks at all.
            break;
        }
    }

    let old_lines = std::mem::take(lines);
    let mut block_index = 0;
    blocks.retain(|block| block.to_take > 0);
    for (offset, line) in old_lines.into_iter().enumerate() {
        let Some(block) = blocks.get(block_index) else {
            lines.push(line);
            continue;
        };
        let skip_start = block.start + (block.length - block.to_take) / 2;
        let skip_end = skip_start + block.to_take;
        if offset < skip_start {
            lines.push(line);
            continue;
        }
        if offset >= skip_end {
            lines.push(Line::from("..."));
            lines.push(line);
            block_index += 1;
        }
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use indoc::indoc;

    fn to_lines(text: &str) -> Vec<Line<'static>> {
        text.lines()
            .filter_map(|l| {
                // Ignore @ - it's just there to tell indoc where the left margin is.
                if !l.starts_with('@') {
                    Some(Line::from(l.to_owned()))
                } else {
                    None
                }
            })
            .collect()
    }

    fn to_text(lines: &[Line]) -> String {
        lines
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<Vec<_>>()
                    .join("")
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn test_remove_excess_context() {
        let mut lines = to_lines(indoc! {r#"
        @
         [common]
        -version = 1
        +version = 2
         a
         b
         c
         d
         e
         f
         g
         h
         i
         j
         k
        +foo = 1
         l
         m
         n
    "#});
        remove_excess_context(&mut lines, 11);
        assert_eq!(
            to_text(&lines),
            to_text(&to_lines(indoc! {r#"
            @
             [common]
            -version = 1
            +version = 2
             a
            ...
             j
             k
            +foo = 1
             l
             m
             n
        "#}))
        );
    }

    #[test]
    fn test_remove_excess_context_from_empty() {
        let mut lines = to_lines(indoc! {r#"
            @
            +[common]
            +version = 1
            +import_std = [
            +    "fs",
            +    "process",
            +    "net",
            +]
        "#});
        remove_excess_context(&mut lines, 6);
        assert_eq!(
            to_text(&lines),
            to_text(&to_lines(indoc! {r#"
            @
            +[common]
            +version = 1
            +import_std = [
            +    "fs",
            +    "process",
            +    "net",
            +]
        "#}))
        );
    }
}
