use anyhow::Result;
use rustc_demangle::demangle;
use std::fmt::Debug;
use std::fmt::Display;
use std::sync::Arc;

use crate::names::Name;

#[derive(Hash, PartialEq, Eq, Clone, PartialOrd, Ord)]
pub(crate) struct Symbol {
    bytes: Arc<[u8]>,
}

impl Symbol {
    pub(crate) fn new<T: Into<Vec<u8>>>(bytes: T) -> Self {
        Self {
            bytes: Arc::from(bytes.into()),
        }
    }

    /// Splits the name of this symbol into names. See `crate::names::split_names` for details.
    pub(crate) fn names(&self) -> Result<Vec<Name>> {
        let name = demangle(std::str::from_utf8(&self.bytes)?).to_string();
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
        self.bytes.len()
    }
}

impl Display for Symbol {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if let Ok(sym_string) = std::str::from_utf8(&self.bytes) {
            write!(f, "{:#}", demangle(sym_string))?;
        } else {
            write!(f, "INVALID-UTF-8({:?})", &self.bytes)?;
        }
        Ok(())
    }
}

impl Debug for Symbol {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if let Ok(sym_string) = std::str::from_utf8(&self.bytes) {
            // For valid UTF-8, we just print as a string. We want something that fits on one line,
            // even when using the alternate format, so that we can efficiently display lists of
            // symbols.
            Debug::fmt(sym_string, f)
        } else {
            // For invalid UTF-8, fall back to a default debug formatting.
            f.debug_struct("Symbol")
                .field("bytes", &self.bytes)
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

    let symbol = Symbol::new(*b"_ZN4core3ptr85drop_in_place$LT$std..rt..lang_start$LT$$LP$$RP$$GT$..$u7b$$u7b$closure$u7d$$u7d$$GT$17h0bb7e9fe967fc41cE");
    assert_eq!(
        borrow(&symbol.names().unwrap()),
        vec![
            vec!["core", "ptr", "drop_in_place"],
            vec!["std", "rt", "lang_start"],
            vec!["{{closure}}"],
        ]
    );

    let symbol = Symbol::new(
        *b"_ZN58_$LT$alloc..string..String$u20$as$u20$core..fmt..Debug$GT$3fmt17h3b29bd412ff2951fE",
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
    let symbol = Symbol::new(*b"_ZN4core3ptr85drop_in_place$LT$std..rt..lang_start$LT$$LP$$RP$$GT$..$u7b$$u7b$closure$u7d$$u7d$$GT$17h0bb7e9fe967fc41cE");
    assert_eq!(
        symbol.to_string(),
        "core::ptr::drop_in_place<std::rt::lang_start<()>::{{closure}}>"
    );
}
