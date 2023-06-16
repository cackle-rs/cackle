use anyhow::Result;
use rustc_demangle::demangle;
use std::fmt::Debug;
use std::fmt::Display;

#[derive(Hash, PartialEq, Eq, Clone, PartialOrd, Ord)]
pub(crate) struct Symbol {
    bytes: Vec<u8>,
}

impl Symbol {
    pub(crate) fn new<T: Into<Vec<u8>>>(bytes: T) -> Self {
        Self {
            bytes: bytes.into(),
        }
    }

    pub(crate) fn parts(&self) -> Result<Vec<Vec<String>>> {
        let name = demangle(std::str::from_utf8(&self.bytes)?).to_string();
        let mut all_parts = Vec::new();
        let mut part = String::new();
        let mut parts = Vec::new();
        for ch in name.chars() {
            if ch == '(' || ch == ')' {
                // Ignore parenthesis.
            } else if ch == '<' || ch == '>' {
                if !part.is_empty() {
                    parts.push(std::mem::take(&mut part));
                }
                if !parts.is_empty() {
                    all_parts.push(std::mem::take(&mut parts));
                }
            } else if ch == ':' {
                if !part.is_empty() {
                    parts.push(std::mem::take(&mut part));
                }
            } else {
                part.push(ch);
            }
        }
        if !part.is_empty() {
            parts.push(std::mem::take(&mut part));
        }
        if !parts.is_empty() {
            all_parts.push(std::mem::take(&mut parts));
        }
        // Rust mangled names end with ::h{some hash}. We don't need this, so drop it.
        if let Some(last_parts) = all_parts.last() {
            if let [last] = last_parts.as_slice() {
                if all_parts.len() >= 2
                    && last.len() == 17
                    && last.starts_with('h')
                    && u64::from_str_radix(&last[1..], 16).is_ok()
                {
                    all_parts.pop();
                }
            }
        }
        Ok(all_parts)
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
            // For valid UTF-8, we pretend to be a tuple struct containing the string.
            f.debug_tuple("Symbol").field(&sym_string).finish()
        } else {
            // For invalid UTF-8, fall back to a default debug formatting.
            f.debug_struct("Symbol")
                .field("bytes", &self.bytes)
                .finish()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Symbol;

    fn borrow(input: &[Vec<String>]) -> Vec<Vec<&str>> {
        input
            .iter()
            .map(|part| part.iter().map(|s| s.as_str()).collect())
            .collect()
    }

    #[test]
    fn test_parts() {
        let symbol = Symbol::new(*b"_ZN4core3ptr85drop_in_place$LT$std..rt..lang_start$LT$$LP$$RP$$GT$..$u7b$$u7b$closure$u7d$$u7d$$GT$17h0bb7e9fe967fc41cE");
        println!("{symbol}");
        assert_eq!(
            borrow(&symbol.parts().unwrap()),
            vec![
                vec!["core", "ptr", "drop_in_place"],
                vec!["std", "rt", "lang_start"],
                vec!["{{closure}}"],
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
}
