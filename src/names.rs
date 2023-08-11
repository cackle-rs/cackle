use crate::cowarc::Utf8Bytes;
use crate::demangle::DemangleToken;
use crate::demangle::NonMangledIterator;
use anyhow::bail;
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

pub(crate) fn collect_names<'data, I: Iterator<Item = DemangleToken<'data>>>(
    it: I,
    out: &mut Vec<Name<'data>>,
) -> Result<()> {
    let mut parts = Vec::new();
    let mut as_state = None;
    for token in it {
        match token {
            DemangleToken::Text(text) => {
                if text != "mut" {
                    parts.push(Utf8Bytes::Borrowed(text))
                }
            }
            DemangleToken::Char(ch) => {
                match ch {
                    '}' if parts == [Utf8Bytes::Borrowed("closure")] => {
                        parts.clear();
                    }
                    ' ' if parts == [Utf8Bytes::Borrowed("as")] => {
                        parts.clear();
                        as_state = Some(AsState {
                            parts: None,
                            gt_depth: 1,
                        });
                    }
                    '<' => {
                        if let Some(s) = as_state.as_mut() {
                            s.gt_depth += 1;
                        }
                    }
                    '>' => {
                        if let Some(s) = as_state.as_mut() {
                            s.gt_depth -= 1;
                        }
                    }
                    _ => {}
                }
                if let Some(s) = as_state.as_mut() {
                    if !parts.is_empty() && s.parts.is_none() {
                        s.parts = Some(std::mem::take(&mut parts));
                    }
                    if s.gt_depth == 0 {
                        if let Some(p) = s.parts.take() {
                            parts = p;
                            continue;
                        }
                        as_state = None;
                    }
                }
                if !parts.is_empty() {
                    let name = Name {
                        parts: std::mem::take(&mut parts),
                    };
                    // Ignore names where all parts are just integers.
                    if !name.parts.iter().all(|p| p.parse::<i64>().is_ok()) {
                        out.push(name)
                    }
                }
            }
            DemangleToken::UnsupportedEscape(esc) => bail!("Unsupported escape `{esc}`"),
        }
    }
    if !parts.is_empty() {
        out.push(Name { parts })
    }
    Ok(())
}

#[derive(Debug)]
struct AsState<'a> {
    parts: Option<Vec<Utf8Bytes<'a>>>,
    gt_depth: i32,
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
