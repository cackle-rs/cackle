use crate::bytes::Bytes;
use crate::names::Name;
use anyhow::Result;
use rustc_demangle::demangle;
use std::fmt::Debug;
use std::fmt::Display;
use std::str::Utf8Error;

/// A symbol from an object file. The symbol might be valid UTF-8 or not. It also may or may not be
/// mangled. Storage may be borrowed or on the heap.
#[derive(Eq, Clone, Ord, PartialEq, PartialOrd, Hash)]
pub(crate) struct Symbol<'data> {
    bytes: Bytes<'data>,
}

impl<'data> Symbol<'data> {
    pub(crate) fn borrowed(data: &[u8]) -> Symbol {
        Symbol {
            bytes: Bytes::borrowed(data),
        }
    }

    /// Create an instance that is heap-allocated and reference counted and thus can be used beyond
    /// the lifetime 'data.
    pub(crate) fn to_heap(&self) -> Symbol<'static> {
        Symbol {
            bytes: self.bytes.to_heap(),
        }
    }

    /// Returns the data that we store.
    fn data(&self) -> &[u8] {
        &self.bytes
    }

    fn to_str(&self) -> Result<&str, Utf8Error> {
        std::str::from_utf8(self.data())
    }

    /// Splits the name of this symbol into names. See `crate::names::split_names` for details.
    pub(crate) fn names(&self) -> Result<Vec<Name>> {
        let name = demangle(self.to_str()?).to_string();
        let mut all_names = crate::names::split_names(&name);
        // Rust mangled names end with ::h{some hash}. We don't need this, so drop it.
        if all_names.len() >= 2 {
            if let Some(last_name) = all_names.last_mut() {
                if let Some(last) = last_name.parts.last() {
                    if last.len() == 17
                        && last.starts_with('h')
                        && u64::from_str_radix(&last[1..], 16).is_ok()
                    {
                        last_name.parts.pop();
                        if last_name.parts.is_empty() {
                            all_names.pop();
                        }
                    }
                }
            }
        }
        Ok(all_names)
    }

    pub(crate) fn len(&self) -> usize {
        self.data().len()
    }

    pub(crate) fn module_name(&self) -> Option<String> {
        self.names().ok()?.first()?.parts.get(1).cloned()
    }
}

impl<'data> Display for Symbol<'data> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if let Ok(sym_string) = self.to_str() {
            write!(f, "{:#}", demangle(sym_string))?;
        } else {
            write!(f, "INVALID-UTF-8({:?})", self.data())?;
        }
        Ok(())
    }
}

impl<'data> Debug for Symbol<'data> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if let Ok(sym_string) = self.to_str() {
            // For valid UTF-8, we just print as a string. We want something that fits on one line,
            // even when using the alternate format, so that we can efficiently display lists of
            // symbols.
            Debug::fmt(sym_string, f)
        } else {
            // For invalid UTF-8, fall back to a default debug formatting.
            f.debug_struct("Symbol")
                .field("bytes", &self.data())
                .finish()
        }
    }
}

#[test]
fn test_names() {
    fn borrow(input: &[Name]) -> Vec<Vec<&str>> {
        input
            .iter()
            .map(|name| name.parts.iter().map(|s| s.as_str()).collect())
            .collect()
    }

    let symbol = Symbol::borrowed(b"_ZN4core3ptr85drop_in_place$LT$std..rt..lang_start$LT$$LP$$RP$$GT$..$u7b$$u7b$closure$u7d$$u7d$$GT$17h0bb7e9fe967fc41cE");
    assert_eq!(
        borrow(&symbol.names().unwrap()),
        vec![
            vec!["core", "ptr", "drop_in_place"],
            vec!["std", "rt", "lang_start"],
            vec!["{{closure}}"],
        ]
    );

    let symbol = Symbol::borrowed(
        b"_ZN58_$LT$alloc..string..String$u20$as$u20$core..fmt..Debug$GT$3fmt17h3b29bd412ff2951fE",
    );
    assert_eq!(
        borrow(&symbol.names().unwrap()),
        vec![
            vec!["alloc", "string", "String"],
            vec!["core", "fmt", "Debug", "fmt"]
        ]
    );
}

#[test]
fn test_display() {
    let symbol = Symbol::borrowed(b"_ZN4core3ptr85drop_in_place$LT$std..rt..lang_start$LT$$LP$$RP$$GT$..$u7b$$u7b$closure$u7d$$u7d$$GT$17h0bb7e9fe967fc41cE");
    assert_eq!(
        symbol.to_string(),
        "core::ptr::drop_in_place<std::rt::lang_start<()>::{{closure}}>"
    );
}

#[test]
fn comparison() {
    fn hash(sym: &Symbol) -> u64 {
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        sym.hash(&mut hasher);
        hasher.finish()
    }
    use std::hash::Hash;
    use std::hash::Hasher;

    let sym1 = Symbol::borrowed(b"sym1");
    let sym2 = Symbol::borrowed(b"sym2");
    assert_eq!(sym1, sym1.to_heap());
    assert!(sym1 < sym2);
    assert!(sym1.to_heap() < sym2);
    assert!(sym1 < sym2.to_heap());
    assert_eq!(hash(&sym1), hash(&sym1.to_heap()));
}
