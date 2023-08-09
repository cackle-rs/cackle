//! For display of mangled symbols, we use rustc-demangle. For analysis however, the heap allocation
//! required slows us down too much. That's where this module comes in. It lets us break apart a
//! mangled symbol and obtain the parts of symbol as references into the original string, thus
//! avoiding heap allocation. This demangler was built experimentally based on observed mangled
//! symbols. We almost certainly get stuff wrong.

#[derive(Debug)]
pub(crate) enum DemangleToken<'data> {
    Text(&'data str),
    Escape(Escape<'data>),
}

#[derive(Debug)]
pub(crate) struct Escape<'data>(&'data str);

#[derive(Copy, Clone, Debug)]
pub(crate) struct DemangleIterator<'data> {
    outer: &'data str,
    inner: Option<&'data str>,
}

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

impl<'data> Escape<'data> {
    pub(crate) fn symbol(&self) -> Option<char> {
        match self.0 {
            "LT" => Some('<'),
            "GT" => Some('>'),
            "LP" => Some('('),
            "RP" => Some(')'),
            esc => {
                if let Some(hex) = esc.strip_prefix('u') {
                    return std::char::from_u32(u32::from_str_radix(hex, 16).ok()?);
                }
                None
            }
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
                    return Some(DemangleToken::Escape(Escape(&rest[..end_escape])));
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

#[cfg(test)]
mod tests {
    use super::*;

    fn parts(mangled: &str) -> Vec<String> {
        fn collect_parts(mut it: DemangleIterator, out: &mut Vec<String>) {
            loop {
                let prev_it = it;
                let Some(part) = it.next() else { break };
                match part {
                    DemangleToken::Text("") => {
                        panic!("Invalid empty text token from iterator: {prev_it:?}");
                    }
                    DemangleToken::Text(text) => out.push(text.to_owned()),
                    DemangleToken::Escape(escape) => {
                        if let Some(sym) = escape.symbol() {
                            out.push(sym.to_string())
                        } else {
                            out.push(escape.0.to_owned())
                        }
                    }
                }
            }
        }

        let it = DemangleIterator::new(mangled);
        let mut out = Vec::new();
        collect_parts(it, &mut out);
        out
    }

    #[test]
    fn test_non_mangled() {
        assert!(parts("").is_empty());
        assert!(parts("foo").is_empty());
    }

    #[test]
    fn test_invalid() {
        assert!(parts("_Z10").is_empty());
    }

    #[test]
    fn test_simple() {
        assert_eq!(
            parts("_ZN3std2fs5write17h0f72782372833d23E"),
            vec!["std", "fs", "write", "h0f72782372833d23"]
        );
    }

    #[test]
    fn test_nested() {
        assert_eq!(
            parts("_ZN58_$LT$alloc..string..String$u20$as$u20$core..fmt..Debug$GT$3fmt17h3b29bd412ff2951fE"),
            vec!["<", "alloc", "string", "String", " ", "as", " ", "core", "fmt", "Debug", ">", "fmt", "h3b29bd412ff2951f"]
        );
    }

    #[test]
    fn test_generics() {
        assert_eq!(
            parts("_ZN4core3ptr85drop_in_place$LT$std..rt..lang_start$LT$$LP$$RP$$GT$..$u7b$$u7b$closure$u7d$$u7d$$GT$17h0bb7e9fe967fc41cE"),
            vec!["core", "ptr", "drop_in_place", "<", "std", "rt", "lang_start", "<", "(", ")", ">",
                "{", "{", "closure", "}", "}", ">", "h0bb7e9fe967fc41c"]
        );
    }

    #[test]
    fn test_literal_number() {
        assert_eq!(
            parts("_ZN104_$LT$proc_macro2..Span$u20$as$u20$syn..span..IntoSpans$LT$$u5b$proc_macro2..Span$u3b$$u20$1$u5d$$GT$$GT$10into_spans17h8cc941d826bfc6f7E"),
            vec!["<", "proc_macro2", "Span", " ", "as", " ", "syn", "span", "IntoSpans", "<",
                "[", "proc_macro2", "Span", ";", " ", "1", "]", ">", ">", "into_spans", "h8cc941d826bfc6f7"]
        );
    }
}
