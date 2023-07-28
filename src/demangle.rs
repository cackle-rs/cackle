//! Mostly we use the rustc-demangle crate for demangling, but that does heap allocation, so is too
//! slow for some uses. This module contains our own simplistic name demangling that we use in cases
//! where we need more performance. It only supports the features we need for the use cases required
//! by those uses.

pub(crate) struct DemangleIterator<'data> {
    data: &'data str,
}

impl<'data> DemangleIterator<'data> {
    pub(crate) fn new(data: &'data str) -> Self {
        if let Some(rest) = data.strip_prefix("_ZN") {
            Self { data: rest }
        } else {
            Self { data: "" }
        }
    }
}

impl<'data> Iterator for DemangleIterator<'data> {
    type Item = &'data str;

    fn next(&mut self) -> Option<Self::Item> {
        let num_digits = self.data.bytes().position(|byte| !byte.is_ascii_digit())?;
        let (length_str, rest) = self.data.split_at(num_digits);
        let length = length_str.parse().ok()?;
        if length >= rest.len() {
            return None;
        }
        let (part, rest) = rest.split_at(length);
        self.data = rest;
        if part.starts_with('_') {
            // We don't currently support nested mangled stuff, so bail.
            return None;
        }

        Some(part)
    }
}

#[cfg(test)]
mod tests {
    use super::DemangleIterator;

    fn parts(mangled: &str) -> Vec<&str> {
        DemangleIterator::new(mangled).collect()
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
    fn test_unsupported() {
        assert!(
            parts("_ZN58_$LT$alloc..string..String$u20$as$u20$core..fmt..Debug$GT$3fmt17h3b29bd412ff2951fE")
            .is_empty()
        );
    }
}
