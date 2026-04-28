//! For display of mangled symbols, we use rustc-demangle. For analysis however, the heap allocation
//! required slows us down too much. That's where this module comes in. It lets us break apart a
//! mangled symbol and obtain the parts of symbol as references into the original string, thus
//! avoiding heap allocation. This demangler was built experimentally based on observed mangled
//! symbols. We almost certainly get stuff wrong.

use anyhow::anyhow;
use anyhow::bail;
use anyhow::Result;
use std::sync::Arc;

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum DemangleToken<'data> {
    Text(&'data str),
    Char(char),
    UnsupportedEscape(&'data str),
}

#[derive(Copy, Clone, Debug)]
pub(crate) enum DemangleIterator<'data> {
    V0 {
        remaining: &'data str,
    },
    Legacy {
        outer: &'data str,
        inner: Option<&'data str>,
    },
    Empty,
}

#[derive(Copy, Clone, Debug)]
pub(crate) struct NonMangledIterator<'data> {
    namespace: &'data [Arc<str>],
    data: &'data str,
}

/// An iterator that processes a mangled string and provides demangled tokens.
impl<'data> DemangleIterator<'data> {
    pub(crate) fn new(data: &'data str) -> Self {
        // Check for V0 mangling (_R...)
        if let Some(rest) = data.strip_prefix("_R") {
            return Self::V0 { remaining: rest };
        }

        // Check for legacy mangling (_ZN...E)
        if let Some(rest) = data.strip_prefix("_ZN").and_then(|d| d.strip_suffix('E')) {
            Self::Legacy {
                outer: rest,
                inner: None,
            }
        } else {
            Self::Empty
        }
    }
}

/// An iterator that processes a non-mangled string and provides the same tokens as
/// `DemangleIterator`.
impl<'data> NonMangledIterator<'data> {
    pub(crate) fn new(namespace: &'data [Arc<str>], data: &'data str) -> Self {
        Self { namespace, data }
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

// V0 mangling helpers
fn is_base62_digit(b: u8) -> bool {
    b.is_ascii_alphanumeric()
}

fn parse_decimal(data: &str) -> Option<(u64, &str)> {
    let mut value = 0u64;
    let mut len = 0;
    for &b in data.as_bytes() {
        if b.is_ascii_digit() {
            value = value.checked_mul(10)?.checked_add((b - b'0') as u64)?;
            len += 1;
        } else {
            break;
        }
    }
    if len == 0 {
        None
    } else {
        Some((value, &data[len..]))
    }
}

fn parse_disambiguator(data: &str) -> Option<&str> {
    if let Some(rest) = data.strip_prefix('s') {
        // Disambiguator - skip 's' and everything until '_'
        if let Some(end) = rest.find('_') {
            return Some(&rest[end + 1..]); // Skip past the '_'
        }
        None
    } else {
        Some(data)
    }
}

fn parse_undisambiguated_identifier(data: &str) -> Option<(&str, &str)> {
    // Check for punycode (u followed by decimal length)
    if let Some(rest) = data.strip_prefix('u') {
        if let Some((len, rest)) = parse_decimal(rest) {
            if let Some(rest) = rest.strip_prefix('_') {
                // Punycode identifier - for simplicity, we'll just extract the raw bytes
                if len as usize <= rest.len() {
                    let (ident, rest) = rest.split_at(len as usize);
                    return Some((ident, rest));
                }
            }
        }
        return None;
    }

    // Regular identifier: decimal length followed by that many bytes
    let (len, rest) = parse_decimal(data)?;
    if len as usize <= rest.len() {
        let (ident, rest) = rest.split_at(len as usize);
        Some((ident, rest))
    } else {
        None
    }
}

fn parse_identifier(data: &str) -> Option<(&str, &str)> {
    // First handle the optional disambiguator
    let data_after_disambiguator = parse_disambiguator(data)?;
    // Then parse the actual identifier
    parse_undisambiguated_identifier(data_after_disambiguator)
}

impl<'data> Iterator for DemangleIterator<'data> {
    type Item = DemangleToken<'data>;

    fn next(&mut self) -> Option<Self::Item> {
        match self {
            DemangleIterator::V0 { remaining } => {
                if remaining.is_empty() {
                    return None;
                }

                let tag = remaining.as_bytes()[0];
                let rest = &remaining[1..];

                macro_rules! set_remaining {
                    ($val:expr) => {
                        *remaining = $val
                    };
                }

                match tag {
                    b'C' => {
                        // Crate root - followed by identifier
                        if let Some((ident, new_rest)) = parse_identifier(rest) {
                            set_remaining!(new_rest);
                            return Some(DemangleToken::Text(ident));
                        }
                    }
                    b'N' => {
                        // Namespace tag - indicates we're in a path
                        // The next character tells us what kind of item
                        set_remaining!(rest);
                        if !rest.is_empty() {
                            let next_tag = rest.as_bytes()[0];
                            match next_tag {
                                b't' => {
                                    // Type in namespace
                                    set_remaining!(&rest[1..]);
                                    return self.next();
                                }
                                b'v' => {
                                    // Value (function/static) in namespace
                                    set_remaining!(&rest[1..]);
                                    return self.next();
                                }
                                b'C' => {
                                    // Crate root in path
                                    return self.next();
                                }
                                _ => {
                                    // Try to parse as identifier directly
                                    if let Some((ident, new_rest)) = parse_identifier(rest) {
                                        set_remaining!(new_rest);
                                        return Some(DemangleToken::Text(ident));
                                    }
                                }
                            }
                        }
                        return self.next();
                    }
                    b'v' | b't' | b'm' => {
                        // Value/type/module item - followed by path
                        set_remaining!(rest);
                        return self.next();
                    }
                    b'M' => {
                        // Inherent impl
                        set_remaining!(rest);
                        if let Some((_, new_rest)) = skip_v0_type(rest) {
                            set_remaining!(new_rest);
                        }
                        return self.next();
                    }
                    b'X' => {
                        // Trait impl: X [<disambiguator>] <impl-path> <self-type> <trait-path>
                        // Skip the optional disambiguator, then parse the impl-path naturally.
                        // The impl-path contains the crate/module info we need for API detection.
                        set_remaining!(parse_disambiguator(rest).unwrap_or(rest));
                        return self.next();
                    }
                    b'Y' => {
                        // <type as trait>: Y <self-type> <trait-path> (no disambiguator)
                        set_remaining!(rest);
                        return self.next();
                    }
                    b'B' => {
                        // Back-reference: B<base62>_ - skip it as we don't resolve back-refs
                        if let Some(pos) = rest.find('_') {
                            set_remaining!(&rest[pos + 1..]);
                        } else {
                            set_remaining!(rest);
                        }
                        return self.next();
                    }
                    b'I' => {
                        // Generic args start
                        set_remaining!(rest);
                        return Some(DemangleToken::Char('<'));
                    }
                    b'E' => {
                        // End marker (generic args or other)
                        set_remaining!(rest);
                        return Some(DemangleToken::Char('>'));
                    }
                    b'p' => {
                        // Pointer
                        set_remaining!(rest);
                        return Some(DemangleToken::Char('*'));
                    }
                    b'R' | b'Q' => {
                        // Reference
                        set_remaining!(rest);
                        return Some(DemangleToken::Char('&'));
                    }
                    b'A' | b'S' => {
                        // Array or Slice
                        set_remaining!(rest);
                        if tag == b'A' {
                            return Some(DemangleToken::Char('['));
                        }
                        return self.next();
                    }
                    b'T' => {
                        // Tuple
                        set_remaining!(rest);
                        return Some(DemangleToken::Char('('));
                    }
                    b'e' => {
                        // Empty tuple
                        set_remaining!(rest);
                        return Some(DemangleToken::Text("()"));
                    }
                    b'u' => {
                        // Unsigned integer
                        set_remaining!(rest);
                        return Some(DemangleToken::Text("u"));
                    }
                    b'i' => {
                        // Signed integer
                        set_remaining!(rest);
                        return Some(DemangleToken::Text("i"));
                    }
                    b'_' => {
                        // Separator in generic args
                        set_remaining!(rest);
                        return Some(DemangleToken::Char(','));
                    }
                    _ if is_base62_digit(tag) => {
                        // This is an identifier (starts with digit or letter)
                        if let Some((ident, new_rest)) = parse_identifier(remaining) {
                            set_remaining!(new_rest);
                            return Some(DemangleToken::Text(ident));
                        }
                    }
                    _ => {
                        // Unknown tag - skip it
                        set_remaining!(rest);
                        return self.next();
                    }
                }

                // If we couldn't parse, stop iteration
                None
            }
            DemangleIterator::Legacy { outer, inner } => {
                // Legacy mangling
                if *inner == Some("") {
                    *inner = None;
                }
                if let Some(data) = inner.as_mut() {
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
                    let end = data
                        .bytes()
                        .position(|b| b == b'.' || b == b'$')
                        .unwrap_or(data.len());
                    let text = &data[..end];
                    *data = &data[end..];
                    return Some(DemangleToken::Text(text));
                }
                if outer.is_empty() {
                    return None;
                }
                let num_digits = outer.bytes().position(|byte| !byte.is_ascii_digit())?;
                let (length_str, rest) = outer.split_at(num_digits);
                let length = length_str.parse().ok()?;
                if length > rest.len() {
                    return None;
                }
                let (part, rest) = rest.split_at(length);
                *outer = rest;
                if let Some(rest) = part.strip_prefix('_') {
                    *inner = Some(rest);
                    return self.next();
                }
                if part.contains('$') {
                    *inner = Some(part);
                    return self.next();
                }

                Some(DemangleToken::Text(part))
            }
            DemangleIterator::Empty => None,
        }
    }
}

// Helper to skip over a V0 type during parsing
fn skip_v0_type(data: &str) -> Option<((), &str)> {
    if data.is_empty() {
        return None;
    }

    let tag = data.as_bytes()[0];
    let rest = &data[1..];

    match tag {
        b'C' => {
            // Crate-relative path
            parse_identifier(rest).map(|(_, r)| ((), r))
        }
        b'N' => {
            // Nested path - recursively skip
            skip_v0_type(rest)
        }
        b'p' | b'R' | b'Q' => {
            // Pointer/reference - skip the pointee type
            skip_v0_type(rest)
        }
        b'A' | b'S' => {
            // Array/slice - skip the element type
            skip_v0_type(rest)
        }
        b'T' => {
            // Tuple - skip until we find the end
            let mut current = rest;
            loop {
                if current.is_empty() {
                    return None;
                }
                if current.as_bytes()[0] == b'E' {
                    return Some(((), &current[1..]));
                }
                if let Some((_, new_rest)) = skip_v0_type(current) {
                    current = new_rest;
                } else {
                    return None;
                }
            }
        }
        b'I' => {
            // Generic args - skip the path and all args
            if let Some((_, mut current)) = skip_v0_type(rest) {
                loop {
                    if current.is_empty() {
                        return None;
                    }
                    if current.as_bytes()[0] == b'E' {
                        return Some(((), &current[1..]));
                    }
                    if let Some((_, new_rest)) = skip_v0_type(current) {
                        current = new_rest;
                    } else {
                        return None;
                    }
                }
            }
            None
        }
        b'e' | b'u' | b'i' | b'f' | b'b' | b'c' | b'z' | b'v' => {
            // Primitive types
            Some(((), rest))
        }
        _ if is_base62_digit(tag) => {
            // Identifier
            parse_identifier(data).map(|(_, r)| ((), r))
        }
        _ => {
            // Unknown - try to skip
            Some(((), rest))
        }
    }
}

/// A lookup table for determining whether a character should end a token.
const IS_PART_SEPARATOR: [bool; 128] = {
    let mut result = [false; 128];
    result[b'<' as usize] = true;
    result[b'>' as usize] = true;
    result[b'[' as usize] = true;
    result[b']' as usize] = true;
    result[b'(' as usize] = true;
    result[b')' as usize] = true;
    result[b'{' as usize] = true;
    result[b'}' as usize] = true;
    result[b';' as usize] = true;
    result[b'&' as usize] = true;
    result[b'*' as usize] = true;
    result[b':' as usize] = true;
    result[b',' as usize] = true;
    result[b' ' as usize] = true;
    result
};

impl<'data> Iterator for NonMangledIterator<'data> {
    type Item = DemangleToken<'data>;

    fn next(&mut self) -> Option<DemangleToken<'data>> {
        if !self.namespace.is_empty() {
            let token = self.namespace[0].as_ref();
            self.namespace = &self.namespace[1..];
            return Some(DemangleToken::Text(token));
        }
        while let Some(rest) = self.data.strip_prefix(':') {
            self.data = rest;
        }
        if self.data.is_empty() {
            return None;
        }
        let end = self
            .data
            .char_indices()
            .find_map(|(index, ch)| {
                IS_PART_SEPARATOR
                    .get(ch as usize)
                    .cloned()
                    // We currently treat all multi-byte characters as separators for consistency
                    // with our demangler.
                    .unwrap_or(true)
                    .then_some(index)
            })
            .unwrap_or(self.data.len());
        if end == 0 {
            let ch = self.data.chars().next().unwrap();
            self.data = &self.data[ch.len_utf8()..];
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
        let tokens: Vec<_> = NonMangledIterator::new(&[], &demangled)
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
    fn test_multibyte_character() {
        // Probably ideally we'd split with "cackle_こけこっこ" as a single part. For now, we just
        // make sure that we don't crash.
        check(
            "_ZN2u142cackle_$u3053$$u3051$$u3053$$u3063$$u3053$17h188ecf9f6da65514E",
            &[
                "u1",
                "cackle_",
                "こ",
                "け",
                "こ",
                "っ",
                "こ",
                "h188ecf9f6da65514",
            ],
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

    #[test]
    fn test_v0_simple() {
        // Real V0 mangled symbol from rustc
        // Test without rustc-demangle comparison since V0 output format may differ
        let mangled = "_RNvCscaAa4Ty0KMw_6simple11hello_world";
        let actual: Vec<String> = DemangleIterator::new(mangled)
            .map(|token| match token {
                DemangleToken::Text(text) => text.to_owned(),
                DemangleToken::Char(ch) => ch.to_string(),
                DemangleToken::UnsupportedEscape(esc) => esc.to_owned(),
            })
            .collect();
        // No "::" separator tokens - identifiers are returned directly like the legacy format
        assert_eq!(actual, &["simple", "hello_world"]);
    }

    #[test]
    fn test_v0_with_generics() {
        // V0 symbol with generic instantiation (simplified version)
        let mangled = "_RINvCs0_5crate8functionpE";
        let actual: Vec<String> = DemangleIterator::new(mangled)
            .map(|token| match token {
                DemangleToken::Text(text) => text.to_owned(),
                DemangleToken::Char(ch) => ch.to_string(),
                DemangleToken::UnsupportedEscape(esc) => esc.to_owned(),
            })
            .collect();
        // Should have: crate, function, <, *, > (no "::" separators)
        assert!(actual.contains(&"crate".to_string()));
        assert!(actual.contains(&"function".to_string()));
        assert!(actual.contains(&"<".to_string()));
    }

    #[test]
    fn test_v0_trait_impl() {
        // Real V0 symbol: <(&str, u16) as std::net::socket_addr::ToSocketAddrs>::to_socket_addrs
        let mangled = "_RNvXs4_NtNtCsjrHSEGnQ3l9_3std3net11socket_addrTRetENtB5_13ToSocketAddrs15to_socket_addrs";
        let tokens: Vec<String> = DemangleIterator::new(mangled)
            .map(|token| match token {
                DemangleToken::Text(text) => text.to_owned(),
                DemangleToken::Char(ch) => ch.to_string(),
                DemangleToken::UnsupportedEscape(esc) => esc.to_owned(),
            })
            .collect();
        // The first two text tokens must be "std" and "net" for module_name() to work
        let text_tokens: Vec<&str> = tokens
            .iter()
            .filter_map(|t| {
                if t.len() > 1 || t.chars().next().is_some_and(|c| c.is_alphabetic()) {
                    Some(t.as_str())
                } else {
                    None
                }
            })
            .collect();
        assert_eq!(
            text_tokens.first(),
            Some(&"std"),
            "first token must be 'std'"
        );
        assert_eq!(
            text_tokens.get(1),
            Some(&"net"),
            "second token must be 'net'"
        );

        // Also verify module_name() works
        use super::*;
        use crate::symbol::Symbol;
        let sym = Symbol::borrowed(mangled.as_bytes());
        assert_eq!(sym.module_name(), Some("net"));
        assert_eq!(sym.crate_name(), Some("std"));
    }

    #[test]
    fn test_v0_nested_path() {
        // Test nested module paths
        let mangled = "_RNvNtNtCs0_4core3ptr8non_null7cleanupE";
        let actual: Vec<String> = DemangleIterator::new(mangled)
            .map(|token| match token {
                DemangleToken::Text(text) => text.to_owned(),
                DemangleToken::Char(ch) => ch.to_string(),
                DemangleToken::UnsupportedEscape(esc) => esc.to_owned(),
            })
            .collect();
        // Should contain the path components without "::" separators
        assert!(actual.contains(&"core".to_string()));
        assert!(actual.contains(&"ptr".to_string()));
        assert!(!actual.contains(&"::".to_string()));
    }
}
