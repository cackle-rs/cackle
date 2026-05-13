//! This module tokenises Rust code and looks for the extern blocks. This is done as an additional
//! layer of defence in addition to use of the -Fmissing-unsafe-on-extern flag when compiling crates,
//! since that flag only works on code before Rust edition 2024.

use crate::location::SourceLocation;
use anyhow::Context;
use anyhow::Result;
use ra_ap_rustc_lexer::Token;
use ra_ap_rustc_lexer::TokenKind;
use std::path::Path;

/// Returns the locations of all extern block usages found in `path`
pub(crate) fn scan_path(path: &Path) -> Result<Vec<SourceLocation>> {
    let bytes =
        std::fs::read(path).with_context(|| format!("Failed to read `{}`", path.display()))?;
    let Ok(source) = std::str::from_utf8(&bytes) else {
        // If the file isn't valid UTF-8 then we don't need to check it for the extern blocks,
        // since it can't be a source file that the rust compiler would accept.
        return Ok(Vec::new());
    };
    Ok(scan_string(source, path))
}

fn scan_string(source: &str, path: &Path) -> Vec<SourceLocation> {
    let mut token_start_offset = 0;
    let mut locations = Vec::new();

    let skip_condition = |x| {
        matches!(
            x,
            TokenKind::BlockComment {
                doc_style: _,
                terminated: _
            } | TokenKind::LineComment { doc_style: _ }
                | TokenKind::Whitespace
        )
    };

    let mut iter = ra_ap_rustc_lexer::tokenize(source, ra_ap_rustc_lexer::FrontmatterAllowed::No);
    let mut previous_token_text = "";
    let mut previous_token_end_offset = 0;
    let mut previous_token_length = 0;

    while let (Some(token), skipped_offset) = next_token(&mut iter, skip_condition) {
        let token_length = usize::try_from(token.len).unwrap();

        token_start_offset += skipped_offset;

        let token_end_offset = token_start_offset + token_length;
        let token_text = &source[token_start_offset..token_end_offset];
        if !previous_token_text.is_empty() {
            // check against previous token
            if check_tokens(previous_token_text, token_text) {
                add_location(
                    source,
                    path,
                    &mut locations,
                    previous_token_end_offset,
                    previous_token_length,
                );
            }
        }
        previous_token_text = token_text;
        previous_token_end_offset = token_end_offset;
        previous_token_length = token_length;
        // always check against potential future token
        if let (Some(next_token), skipped_offset) = next_token(&mut iter, skip_condition) {
            // the next token starts after the end of the current token
            token_start_offset += token_length;
            token_start_offset += skipped_offset;

            let next_token_length = usize::try_from(next_token.len).unwrap();

            let next_token_end_offset = token_start_offset + next_token_length;
            let next_token_text = &source[token_start_offset..next_token_end_offset];
            if check_tokens(token_text, next_token_text) {
                add_location(source, path, &mut locations, token_end_offset, token_length);
            }
            token_start_offset = next_token_end_offset;
            // as we consumed the next token already, this will be our previous token in the next loop iteration
            previous_token_text = next_token_text;
            previous_token_end_offset = next_token_end_offset;
            previous_token_length = next_token_length;
        } else {
            // current token is last token in file
            // this should never be valid code, but we flag it nevertheless to be safe
            if token_text == "extern" {
                add_location(source, path, &mut locations, token_end_offset, token_length);
            }
            // as there should not be another loop iteration this should be useless
            // we do it just for completeness
            token_start_offset = token_end_offset;
        }
    }
    locations
}

fn check_tokens(first_token_text: &str, second_token_text: &str) -> bool {
    first_token_text == "extern" && !matches!(second_token_text, "crate" | "]" | ")")
}

/// Returns the next relevant token according to the condition given and the offset to it
fn next_token<F>(
    iter: &mut impl Iterator<Item = Token>,
    skip_condition: F,
) -> (Option<Token>, usize)
where
    F: Fn(TokenKind) -> bool,
{
    let mut skipped_offset = 0;

    for next in iter.by_ref() {
        if skip_condition(next.kind) {
            skipped_offset += usize::try_from(next.len).unwrap();
        } else {
            return (Some(next), skipped_offset);
        }
    }
    (None, skipped_offset)
}

fn add_location(
    source: &str,
    path: &Path,
    locations: &mut Vec<SourceLocation>,
    token_end_offset: usize,
    token_length: usize,
) {
    let column = source[..token_end_offset]
        .lines()
        .last()
        .map(|line| (line.len() - token_length + 1) as u32)
        .unwrap_or(1);
    let line = 1.max(source[..token_end_offset].lines().count() as u32);
    locations.push(SourceLocation::new(path, line, Some(column)));
}
