use crate::cowarc::Utf8Bytes;
use crate::demangle::DemangleToken;
use crate::demangle::NonMangledIterator;
use anyhow::anyhow;
use anyhow::Result;
use std::fmt::Debug;
use std::fmt::Display;

/// A name of something. e.g. `std::path::Path`.
#[derive(Eq, PartialEq, Hash, Clone)]
pub(crate) struct Name<'data> {
    /// The components of this name. e.g. ["std", "path", "Path"]
    pub(crate) parts: Vec<Utf8Bytes<'data>>,
}

impl<'data> Name<'data> {
    pub(crate) fn to_heap(&self) -> Name<'static> {
        Name {
            parts: self.parts.iter().map(|p| p.to_heap()).collect(),
        }
    }
}

/// Splits a composite name into names. Each name is further split on "::". For example:
/// "core::ptr::drop_in_place<std::rt::lang_start<()>::{{closure}}>" would split into:
/// [
///   ["core", "ptr", "drop_in_place"],
///   ["std", "rt", "lang_start"],
///   ["{{closure}}"],
/// ]
/// "<alloc::string::String as std::fmt::Debug>::fmt" would split into:
/// [
///   ["alloc", "string", "String"],
///   ["std", "fmt", "Debug", "fmt"],
/// ]
pub(crate) fn split_names(composite: &str) -> Vec<Name> {
    let mut names: Vec<Name> = Vec::new();
    // The following unwrap should always succeed, since NonMangledIterator never produces
    // DemangleToken::UnsupportedEscape, which is the only failure mode for collect_names.
    collect_names(NonMangledIterator::new(composite), &mut names).unwrap();
    names
}

pub(crate) fn collect_names<'data, I: Clone + Iterator<Item = DemangleToken<'data>>>(
    it: I,
    out: &mut Vec<Name<'data>>,
) -> Result<()> {
    let mut name_parts = Vec::new();
    for token in NamesIterator::new(it) {
        match token {
            NameToken::Part(part) => name_parts.push(Utf8Bytes::Borrowed(part)),
            NameToken::EndName => out.push(Name {
                parts: std::mem::take(&mut name_parts),
            }),
            NameToken::Error(error) => return Err(error),
        }
    }
    Ok(())
}

struct NamesIterator<'data, I: Iterator<Item = DemangleToken<'data>>> {
    it: I,
    state: NamesIteratorState<I>,
    brace_depth: i32,
    as_final: Option<&'data str>,
}

impl<'data, I: Iterator<Item = DemangleToken<'data>>> NamesIterator<'data, I> {
    fn new(it: I) -> Self {
        Self {
            it,
            state: NamesIteratorState::Inactive,
            brace_depth: 0,
            as_final: None,
        }
    }
}

enum NameToken<'data> {
    Part(&'data str),
    EndName,
    Error(anyhow::Error),
}

impl<'data, I: Clone + Iterator<Item = DemangleToken<'data>>> Iterator for NamesIterator<'data, I> {
    type Item = NameToken<'data>;

    fn next(&mut self) -> Option<Self::Item> {
        while let Some(token) = self.it.next() {
            match token {
                DemangleToken::Text(text) => {
                    if text == "mut" {
                        continue;
                    }
                    if self.brace_depth > 0 && text == "closure" {
                        continue;
                    }
                    // Ignore numbers.
                    if text.parse::<i64>().is_ok() {
                        continue;
                    }
                    if text == "as" {
                        let mut look_ahead = self.it.clone();
                        if look_ahead.next() == Some(DemangleToken::Char(' ')) {
                            self.it = look_ahead;
                            self.state = NamesIteratorState::AsPrefix;
                            continue;
                        }
                    }
                    if self.as_final == Some(text)
                        && self
                            .as_final
                            .map(|t| t.as_ptr() as usize == text.as_ptr() as usize)
                            .unwrap_or(false)
                    {
                        // This text was already output as the final part of an as-name. Ignore it.
                        continue;
                    }
                    match &self.state {
                        NamesIteratorState::Inactive => {
                            self.state = NamesIteratorState::OutputtingName;
                        }
                        NamesIteratorState::AsSkip { .. } => {
                            continue;
                        }
                        _ => {}
                    }
                    return Some(NameToken::Part(text));
                }
                DemangleToken::Char(ch) => {
                    if let NamesIteratorState::AsPrefix = &self.state {
                        self.state = NamesIteratorState::AsSkip {
                            gt_depth: 1,
                            return_point: self.it.clone(),
                        };
                    }
                    match ch {
                        '{' => self.brace_depth += 1,
                        '}' => self.brace_depth -= 1,
                        '<' => {
                            if let NamesIteratorState::AsSkip { gt_depth, .. } = &mut self.state {
                                *gt_depth += 1;
                            }
                        }
                        '>' => {
                            if let NamesIteratorState::AsSkip { gt_depth, .. } = &mut self.state {
                                *gt_depth -= 1;
                            }
                        }
                        _ => {}
                    }
                    match &self.state {
                        NamesIteratorState::OutputtingName => {
                            self.state = NamesIteratorState::Inactive;
                            return Some(NameToken::EndName);
                        }
                        NamesIteratorState::AsSkip {
                            gt_depth,
                            return_point,
                        } => {
                            if *gt_depth == 0 {
                                match self.it.next() {
                                    Some(DemangleToken::Text(text)) => {
                                        self.it = return_point.clone();
                                        self.as_final = Some(text);
                                        self.state = NamesIteratorState::OutputtingName;
                                        return Some(NameToken::Part(text));
                                    }
                                    other => {
                                        return Some(NameToken::Error(anyhow!(
                                            "Expected text after '>', got {other:?}"
                                        )));
                                    }
                                }
                            }
                        }
                        _ => {}
                    }
                }
                DemangleToken::UnsupportedEscape(esc) => {
                    return Some(NameToken::Error(anyhow!("Unsupported escape `{esc}`")));
                }
            }
        }
        if matches!(&self.state, NamesIteratorState::OutputtingName) {
            self.state = NamesIteratorState::Inactive;
            return Some(NameToken::EndName);
        }
        None
    }
}

#[derive(Debug)]
enum NamesIteratorState<I> {
    /// We're not outputting a anme and no 'as' token has been encountered yet.
    Inactive,
    /// We've output at least one part of a name.
    OutputtingName,
    /// Reading prefix. We're reading up until a name-terminator. e.g. in `<Foo as bar::Baz>::baz`,
    /// we're somewhere in the `bar::Baz` part.
    AsPrefix,
    /// We've stopped reading the prefix and we're waiting until the '>' depth reaches zero.
    AsSkip {
        /// The number of '>' symbols we need before we read the final part.
        gt_depth: i32,
        /// An iterator pointing to where we'll come back to once we've finished with the as-name.
        return_point: I,
    },
}

impl<'data> Display for Name<'data> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let parts: Vec<String> = self.parts.iter().map(|p| p.to_string()).collect();
        write!(f, "{}", parts.join("::"))
    }
}

impl<'data> Debug for Name<'data> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Name({})", self)
    }
}

pub(crate) fn split_simple(value: &str) -> Name {
    Name {
        parts: value.split("::").map(Utf8Bytes::Borrowed).collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn borrow<'a>(input: &'a [Name]) -> Vec<Vec<&'a str>> {
        input
            .iter()
            .map(|name| name.parts.iter().map(|s| s.data()).collect())
            .collect()
    }

    #[test]
    fn test_split_with_closure() {
        assert_eq!(
            borrow(&split_names(
                "core::ptr::drop_in_place<std::rt::lang_start<()>::{{closure}}>"
            )),
            vec![
                vec!["core", "ptr", "drop_in_place"],
                vec!["std", "rt", "lang_start"],
            ]
        );
    }

    #[test]
    fn test_split_as() {
        assert_eq!(
            borrow(&split_names(
                "<alloc::string::String as core::fmt::Debug>::fmt"
            )),
            vec![
                vec!["alloc", "string", "String"],
                vec!["core", "fmt", "Debug", "fmt"],
            ]
        );
    }

    #[test]
    fn test_split_with_comma() {
        assert_eq!(
            borrow(&split_names(
                "HashMap<std::string::String, std::path::PathBuf>"
            )),
            vec![
                vec!["HashMap"],
                vec!["std", "string", "String"],
                vec!["std", "path", "PathBuf"],
            ]
        );
    }

    #[test]
    fn test_split_mut_ref() {
        assert_eq!(
            borrow(&split_names("Vec<&mut std::string::String>")),
            vec![vec!["Vec"], vec!["std", "string", "String"],]
        );
    }
}
