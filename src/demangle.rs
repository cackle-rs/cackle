//! For display of mangled symbols, we use rustc-demangle. For analysis however, the heap allocation
//! required slows us down too much. That's where this module comes in. It lets us break apart a
//! mangled symbol and obtain the parts of symbol as references into the original string, thus
//! avoiding heap allocation. This demangler was built experimentally based on observed mangled
//! symbols. We almost certainly get stuff wrong.

use anyhow::anyhow;
use anyhow::bail;
use anyhow::Result;

#[derive(Debug)]
pub(crate) enum DemangleToken<'data> {
    Text(&'data str),
    Char(char),
    UnsupportedEscape(&'data str),
}

#[derive(Copy, Clone, Debug)]
pub(crate) struct DemangleIterator<'data> {
    outer: &'data str,
    inner: Option<&'data str>,
}

#[derive(Copy, Clone, Debug)]
pub(crate) struct NonMangledIterator<'data> {
    data: &'data str,
}

/// An iterator that processes a mangled string and provides demangled tokens.
impl<'data> DemangleIterator<'data> {
    pub(crate) fn new(data: &'data str) -> Self {
        if let Some(rest) = data.strip_prefix("_ZN").and_then(|d| d.strip_suffix('E')) {
            Self {
                outer: rest,
                inner: None,
            }
        } else {
            Self {
                outer: "",
                inner: None,
            }
        }
    }
}

/// An iterator that processes a non-mangled string and provides the same tokens as
/// `DemangleIterator`.
impl<'data> NonMangledIterator<'data> {
    pub(crate) fn new(data: &'data str) -> Self {
        Self { data }
    }
}

fn symbol(esc: &str) -> Result<char> {
    match esc {
        "LT" => Ok('<'),
        "GT" => Ok('>'),
        "LP" => Ok('('),
        "RP" => Ok(')'),
        "C" => Ok(','),
        "BP" => Ok('*'),
        "RF" => Ok('&'),
        esc => {
            if let Some(hex) = esc.strip_prefix('u') {
                return std::char::from_u32(u32::from_str_radix(hex, 16)?)
                    .ok_or_else(|| anyhow!("Invalid char"));
            }
            bail!("Unsupported demangle escape `{esc}`");
        }
    }
}

impl<'data> Iterator for DemangleIterator<'data> {
    type Item = DemangleToken<'data>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.inner == Some("") {
            self.inner = None;
        }
        if let Some(data) = self.inner.as_mut() {
            while let Some(rest) = data.strip_prefix('.') {
                *data = rest;
            }
            if let Some(rest) = data.strip_prefix('$') {
                if let Some(end_escape) = rest.find('$') {
                    *data = &rest[end_escape + 1..];
                    if let Ok(ch) = symbol(&rest[..end_escape]) {
                        return Some(DemangleToken::Char(ch));
                    }
                    return Some(DemangleToken::UnsupportedEscape(&rest[..end_escape]));
                }
            }
            let len = data.len();
            let end = data
                .find('.')
                .unwrap_or(len)
                .min(data.find('$').unwrap_or(len));
            let text = &data[..end];
            *data = &data[end..];
            return Some(DemangleToken::Text(text));
        }
        let data = &mut self.outer;
        if data.is_empty() {
            return None;
        }
        let num_digits = data.bytes().position(|byte| !byte.is_ascii_digit())?;
        let (length_str, rest) = data.split_at(num_digits);
        let length = length_str.parse().ok()?;
        if length > rest.len() {
            return None;
        }
        let (part, rest) = rest.split_at(length);
        *data = rest;
        if let Some(rest) = part.strip_prefix('_') {
            self.inner = Some(rest);
            return self.next();
        }
        if part.contains('$') {
            self.inner = Some(part);
            return self.next();
        }

        Some(DemangleToken::Text(part))
    }
}

impl<'data> Iterator for NonMangledIterator<'data> {
    type Item = DemangleToken<'data>;

    fn next(&mut self) -> Option<DemangleToken<'data>> {
        while let Some(rest) = self.data.strip_prefix(':') {
            self.data = rest;
        }
        if self.data.is_empty() {
            return None;
        }
        let end = self
            .data
            .chars()
            .position(|ch| "<>[](){};&*:, ".contains(ch))
            .unwrap_or(self.data.len());
        if end == 0 {
            let ch = self.data.chars().next().unwrap();
            self.data = &self.data[1..];
            return Some(DemangleToken::Char(ch));
        }
        let (text, rest) = self.data.split_at(end);
        self.data = rest;
        Some(DemangleToken::Text(text))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[track_caller]
    fn check(mangled: &str, expected: &[&str]) {
        fn token_to_string(token: DemangleToken) -> String {
            match token {
                DemangleToken::Text("") => {
                    panic!("Invalid empty text token from iterator");
                }
                DemangleToken::Text(text) => text.to_owned(),
                DemangleToken::Char(ch) => ch.to_string(),
                DemangleToken::UnsupportedEscape(esc) => esc.to_owned(),
            }
        }

        let actual: Vec<String> = DemangleIterator::new(mangled)
            .map(token_to_string)
            .collect();
        assert_eq!(actual, expected);

        if expected.is_empty() {
            return;
        }

        // Check consistency with rustc-demangle.
        let demangled = rustc_demangle::demangle(mangled).to_string();
        let tokens: Vec<_> = NonMangledIterator::new(&demangled)
            .map(token_to_string)
            .collect();
        assert_eq!(tokens, expected);
    }

    #[test]
    fn test_non_mangled() {
        check("", &[]);
        check("foo", &[]);
    }

    #[test]
    fn test_invalid() {
        check("_Z10", &[]);
    }

    #[test]
    fn test_simple() {
        check(
            "_ZN3std2fs5write17h0f72782372833d23E",
            &["std", "fs", "write", "h0f72782372833d23"],
        );
    }

    #[test]
    fn test_nested() {
        check("_ZN58_$LT$alloc..string..String$u20$as$u20$core..fmt..Debug$GT$3fmt17h3b29bd412ff2951fE",
            &["<", "alloc", "string", "String", " ", "as", " ", "core", "fmt", "Debug", ">", "fmt", "h3b29bd412ff2951f"]
        );
    }

    #[test]
    fn test_generics() {
        check("_ZN4core3ptr85drop_in_place$LT$std..rt..lang_start$LT$$LP$$RP$$GT$..$u7b$$u7b$closure$u7d$$u7d$$GT$17h0bb7e9fe967fc41cE",
            &["core", "ptr", "drop_in_place", "<", "std", "rt", "lang_start", "<", "(", ")", ">",
                "{", "{", "closure", "}", "}", ">", "h0bb7e9fe967fc41c"]
        );
    }

    #[test]
    fn test_literal_number() {
        check("_ZN104_$LT$proc_macro2..Span$u20$as$u20$syn..span..IntoSpans$LT$$u5b$proc_macro2..Span$u3b$$u20$1$u5d$$GT$$GT$10into_spans17h8cc941d826bfc6f7E",
            &["<", "proc_macro2", "Span", " ", "as", " ", "syn", "span", "IntoSpans", "<",
                "[", "proc_macro2", "Span", ";", " ", "1", "]", ">", ">", "into_spans", "h8cc941d826bfc6f7"]
        );
    }

    #[test]
    fn test_other() {
        check(
            "_ZN5alloc5boxed16Box$LT$T$C$A$GT$11from_raw_in17he8866793064ad1a4E",
            &[
                "alloc",
                "boxed",
                "Box",
                "<",
                "T",
                ",",
                "A",
                ">",
                "from_raw_in",
                "he8866793064ad1a4",
            ],
        );
        check(
            "_ZN4core3ptr7mut_ptr31_$LT$impl$u20$$BP$mut$u20$T$GT$17wrapping_byte_sub17hc0db533e028f9792E",
            &[
                "core",
                "ptr",
                "mut_ptr",
                "<",
                "impl",
                " ",
                "*",
                "mut",
                " ",
                "T",
                ">",
                "wrapping_byte_sub",
                "hc0db533e028f9792",
            ],
        );
        check(
            "_ZN55_$LT$$RF$T$u20$as$u20$core..convert..AsRef$LT$U$GT$$GT$6as_ref17hc407bb9d235949dfE",
            &[
                "<",
                "&",
                "T",
                " ",
                "as",
                " ",
                "core",
                "convert",
                "AsRef",
                "<",
                "U",
                ">",
                ">",
                "as_ref",
                "hc407bb9d235949df",
            ],
        );
    }
}
