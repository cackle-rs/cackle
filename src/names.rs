use crate::utf8::Utf8Bytes;
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
    let mut input = composite;
    let mut all_names: Vec<Name> = Vec::new();
    let mut parts = Vec::new();
    // True if we encountered " as ". When we subsequently encounter '>', we'll ignore it so
    // that the subsequent name part gets added to whatever part came after the " as ".
    let mut as_active = false;
    loop {
        let end_word = input
            .char_indices()
            .find_map(|(pos, ch)| "():&<>, ".contains(ch).then_some(pos))
            .unwrap_or(input.len());
        let part = &input[..end_word];
        if !part.is_empty() && part != "mut" {
            parts.push(Utf8Bytes::borrowed(part));
        }
        input = &input[end_word..];
        if let Some(rest) = input.strip_prefix(" as ") {
            all_names.push(Name {
                parts: std::mem::take(&mut parts),
            });
            input = rest;
            as_active = true;
        } else if let Some(ch) = input.chars().next() {
            if "()<>,".contains(ch) {
                if as_active {
                    as_active = false;
                } else if !parts.is_empty() {
                    all_names.push(Name {
                        parts: std::mem::take(&mut parts),
                    });
                }
            }
            input = &input[1..];
        } else {
            break;
        }
    }
    if !parts.is_empty() {
        all_names.push(Name { parts });
    }
    all_names
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
                vec!["{{closure}}"],
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
