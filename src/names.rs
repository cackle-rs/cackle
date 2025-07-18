use crate::cowarc::Utf8Bytes;
use crate::demangle::DemangleToken;
use crate::demangle::NonMangledIterator;
use crate::symbol::Symbol;
use anyhow::anyhow;
use anyhow::bail;
use anyhow::Result;
use std::fmt::Debug;
use std::fmt::Display;
use std::sync::Arc;

/// A name of something. e.g. `std::path::Path`.
#[derive(Eq, PartialEq, Hash, Clone)]
pub(crate) struct Name {
    /// The components of this name. e.g. ["std", "path", "Path"]
    pub(crate) parts: Vec<Arc<str>>,
}

/// A name obtained from debug info.
#[derive(Eq, PartialEq, Hash, Clone, Debug, PartialOrd, Ord)]
pub(crate) struct DebugName<'input> {
    pub(crate) namespace: Namespace,
    pub(crate) name: Utf8Bytes<'input>,
}

#[derive(Eq, PartialEq, Hash, Clone, Debug, PartialOrd, Ord)]
pub(crate) struct Namespace {
    pub(crate) parts: Arc<[Arc<str>]>,
}

#[derive(Default, Clone, Debug)]
pub(crate) struct SymbolAndName<'input> {
    pub(crate) symbol: Option<Symbol<'input>>,
    pub(crate) debug_name: Option<DebugName<'input>>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) enum SymbolOrDebugName {
    Symbol(Symbol<'static>),
    DebugName(DebugName<'static>),
}

impl Name {
    pub(crate) fn parts(&self) -> impl Iterator<Item = &str> {
        self.parts.iter().map(|p| p.as_ref())
    }

    pub(crate) fn starts_with(&self, prefix: &str) -> bool {
        self.parts
            .first()
            .is_some_and(|name_start| prefix == &**name_start)
    }
}

impl Namespace {
    pub(crate) fn empty() -> Self {
        Self {
            parts: Arc::new([]),
        }
    }

    pub(crate) fn top_level(name: &str) -> Self {
        Self {
            parts: Arc::new([Arc::from(name)]),
        }
    }

    pub(crate) fn plus(&self, name: &str) -> Self {
        Self {
            parts: self
                .parts
                .iter()
                .cloned()
                .chain(std::iter::once(Arc::from(name)))
                .collect(),
        }
    }

    fn is_empty(&self) -> bool {
        self.parts.is_empty()
    }
}

impl<'input> DebugName<'input> {
    pub(crate) fn names_iterator<'a>(&'a self) -> NamesIterator<'a, NonMangledIterator<'a>> {
        NamesIterator::new(NonMangledIterator::new(
            &self.namespace.parts,
            self.name.as_ref(),
        ))
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
pub(crate) struct NamesIterator<'data, I: Iterator<Item = DemangleToken<'data>>> {
    current: NamesIteratorPos<'data, I>,
    error: Option<anyhow::Error>,
}

#[derive(Clone)]
pub(crate) struct NamesIteratorPos<'data, I: Iterator<Item = DemangleToken<'data>>> {
    it: I,
    state: NamesIteratorState<I>,
    brace_depth: i32,
    as_final: Option<&'data str>,
    ended: bool,
}

impl<'data, I: Clone + Iterator<Item = DemangleToken<'data>>> NamesIterator<'data, I> {
    pub(crate) fn new(it: I) -> Self {
        Self {
            current: NamesIteratorPos {
                it,
                state: NamesIteratorState::Inactive,
                brace_depth: 0,
                as_final: None,
                ended: false,
            },
            error: None,
        }
    }

    /// Returns:
    ///  0: An iterator through the parts of the next name.
    ///  1: A token that can, if needed be used to produce a full copy of that name after the fact.
    ///
    /// The last returned name will be empty.
    pub(crate) fn next_name(
        &mut self,
    ) -> Result<Option<(NamePartsIterator<'_, 'data, I>, LazyName<'data, I>)>> {
        if let Some(error) = self.error.take() {
            return Err(error);
        }
        if self.current.ended {
            return Ok(None);
        }
        let name = LazyName {
            it: self.current.clone(),
        };
        Ok(Some((
            NamePartsIterator {
                it: self,
                ended: false,
            },
            name,
        )))
    }
}

pub(crate) struct LazyName<'data, I: Iterator<Item = DemangleToken<'data>>> {
    it: NamesIteratorPos<'data, I>,
}

impl<'data, I: Clone + Iterator<Item = DemangleToken<'data>>> LazyName<'data, I> {
    pub(crate) fn create_name(self) -> Result<Name> {
        let mut parts = Vec::new();
        for token in self.it {
            match token {
                NameToken::Part(part) => {
                    parts.push(Arc::from(part));
                }
                NameToken::EndName => {
                    return Ok(Name { parts });
                }
                NameToken::Error(error) => return Err(error),
            }
        }
        bail!("Reached end of `create_name`");
    }
}

/// Iterates over the parts of a name, where the source of that name is a `NamesIterator`. Handles
/// incomplete iteration by advancing to the next name.
pub(crate) struct NamePartsIterator<'it, 'data, I: Clone + Iterator<Item = DemangleToken<'data>>> {
    it: &'it mut NamesIterator<'data, I>,
    ended: bool,
}

impl<'data, I> Iterator for NamePartsIterator<'_, 'data, I>
where
    I: Clone + Iterator<Item = DemangleToken<'data>>,
{
    type Item = &'data str;

    fn next(&mut self) -> Option<Self::Item> {
        if self.ended {
            return None;
        }
        match self.it.current.next()? {
            NameToken::Part(text) => return Some(text),
            NameToken::EndName => {
                self.ended = true;
            }
            NameToken::Error(error) => self.it.error = Some(error),
        }
        None
    }
}

impl<'data, I> Drop for NamePartsIterator<'_, 'data, I>
where
    I: Clone + Iterator<Item = DemangleToken<'data>>,
{
    fn drop(&mut self) {
        // Make sure that we've consumed to the end before we're dropped, otherwise the next call to
        // `next_name` will get the remainder of the name for this iterator.
        while self.next().is_some() {}
    }
}

pub(crate) enum NameToken<'data> {
    Part(&'data str),
    EndName,
    Error(anyhow::Error),
}

impl<'data, I: Clone + Iterator<Item = DemangleToken<'data>>> Iterator
    for NamesIteratorPos<'data, I>
{
    type Item = NameToken<'data>;

    fn next(&mut self) -> Option<Self::Item> {
        while let Some(token) = self.it.next() {
            match token {
                DemangleToken::Text(text) => {
                    if text == "mut" {
                        continue;
                    }
                    if self.brace_depth > 0 {
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
                            .is_some_and(|t| std::ptr::eq(t.as_ptr(), text.as_ptr()))
                    {
                        // This text was already output as the final part of an as-name. Ignore it.
                        continue;
                    }
                    // Rust mangled names end with ::h{some hash}. We don't need this, so drop it.
                    if text.len() == 17
                        && text.starts_with('h')
                        && text[1..].bytes().all(|b| b.is_ascii_hexdigit())
                        && self.it.clone().next().is_none()
                    {
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
                                    _ => {
                                        self.it = return_point.clone();
                                        self.as_final = None;
                                        self.state = NamesIteratorState::Inactive;
                                        return Some(NameToken::EndName);
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
        self.ended = true;
        None
    }
}

impl<'input> DebugName<'input> {
    pub(crate) fn to_heap(&self) -> DebugName<'static> {
        DebugName {
            namespace: self.namespace.clone(),
            name: self.name.to_heap(),
        }
    }

    pub(crate) fn new(namespace: Namespace, name: &'input str) -> DebugName<'input> {
        DebugName {
            namespace,
            name: Utf8Bytes::Borrowed(name),
        }
    }
}

#[derive(Debug, Clone)]
enum NamesIteratorState<I> {
    /// We're not outputting a name and no 'as' token has been encountered yet.
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

impl SymbolAndName<'_> {
    pub(crate) fn symbol_or_debug_name(&self) -> Result<SymbolOrDebugName> {
        if let Some(debug_name) = self.debug_name.as_ref() {
            return Ok(SymbolOrDebugName::DebugName(debug_name.to_heap()));
        }
        if let Some(symbol) = self.symbol.as_ref() {
            return Ok(SymbolOrDebugName::Symbol(symbol.to_heap()));
        }
        bail!("Invalid SymbolAndName has neither");
    }
}

impl Display for Name {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let parts: Vec<String> = self.parts.iter().map(|p| p.to_string()).collect();
        write!(f, "{}", parts.join("::"))
    }
}

impl Display for SymbolAndName<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if let Some(name) = self.debug_name.as_ref() {
            Display::fmt(&name, f)?;
        }
        if let Some(sym) = self.symbol.as_ref() {
            Display::fmt("[", f)?;
            Display::fmt(&sym, f)?;
            Display::fmt("]", f)?
        }
        if self.debug_name.is_none() && self.symbol.is_none() {
            write!(f, "<missing symbol and debug name>")?;
        }
        Ok(())
    }
}

impl Display for Namespace {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut first = true;
        for p in &self.parts[..] {
            if first {
                first = false;
            } else {
                write!(f, "::")?;
            }
            Display::fmt(&p, f)?;
        }
        Ok(())
    }
}

impl Display for DebugName<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        Display::fmt(&self.namespace, f)?;
        if !self.namespace.is_empty() {
            Display::fmt(&"::", f)?;
        }
        Display::fmt(&*self.name, f)
    }
}

impl Display for SymbolOrDebugName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SymbolOrDebugName::Symbol(sym) => Display::fmt(&sym, f),
            SymbolOrDebugName::DebugName(debug_name) => Display::fmt(&debug_name, f),
        }
    }
}

impl Debug for Name {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Name({self})")
    }
}

pub(crate) fn split_simple(value: &str) -> Name {
    Name {
        parts: value.split("::").map(Arc::from).collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[track_caller]
    fn check(namespace: &[&str], input: &str, expected: &[&[&str]]) {
        let mut out = Vec::new();
        let mut name_parts = Vec::new();
        let namespace: Vec<Arc<str>> = namespace.iter().map(|s| Arc::from(*s)).collect();
        for token in NamesIterator::new(NonMangledIterator::new(&namespace, input)).current {
            match token {
                NameToken::Part(part) => name_parts.push(part),
                NameToken::EndName => out.push(std::mem::take(&mut name_parts)),
                NameToken::Error(error) => panic!("{error}"),
            }
        }
        assert_eq!(out, expected);
    }

    #[test]
    fn test_split_with_closure() {
        check(
            &[],
            "core::ptr::drop_in_place<std::rt::lang_start<()>::{{closure}}>",
            &[
                &["core", "ptr", "drop_in_place"],
                &["std", "rt", "lang_start"],
            ],
        );
    }

    #[test]
    fn test_split_as() {
        check(
            &[],
            "<alloc::string::String as core::fmt::Debug>::fmt",
            &[
                &["alloc", "string", "String"],
                &["core", "fmt", "Debug", "fmt"],
            ],
        );
    }

    #[test]
    fn test_split_with_comma() {
        check(
            &["std", "collections"],
            "HashMap<std::string::String, std::path::PathBuf>",
            &[
                &["std", "collections", "HashMap"],
                &["std", "string", "String"],
                &["std", "path", "PathBuf"],
            ],
        );
    }

    #[test]
    fn test_split_mut_ref() {
        check(
            &["std", "vec"],
            "Vec<&mut std::string::String>",
            &[&["std", "vec", "Vec"], &["std", "string", "String"]],
        );
    }

    #[test]
    fn test_split_vtable() {
        check(
            &[],
            "<std::rt::lang_start::{closure_env#0}<()> as core::ops::function::Fn<()>>::{vtable}",
            &[
                &["std", "rt", "lang_start"],
                &["core", "ops", "function", "Fn"],
            ],
        );
    }

    #[test]
    fn test_debug_name_display() {
        let name = DebugName::new(
            Namespace::empty().plus("std").plus("collections"),
            "HashMap<String, u32>",
        );
        assert_eq!(name.to_string(), "std::collections::HashMap<String, u32>");
    }
}
