use std::ops::Deref;
use std::sync::Arc;

/// Like crate::bytes::Bytes, but for UTF-8 data.
#[derive(Eq, Clone, Ord, Debug)]
pub(crate) enum Utf8Bytes<'data> {
    Heap(Arc<str>),
    Borrowed(&'data str),
}

impl<'data> Utf8Bytes<'data> {
    pub(crate) fn borrowed(data: &str) -> Utf8Bytes {
        Utf8Bytes::Borrowed(data)
    }

    /// Create an instance that is heap-allocated and reference counted and thus can be used beyond
    /// the lifetime 'data.
    pub(crate) fn to_heap(&self) -> Utf8Bytes<'static> {
        Utf8Bytes::Heap(match self {
            Utf8Bytes::Heap(data) => Arc::clone(data),
            Utf8Bytes::Borrowed(data) => Arc::from(*data),
        })
    }

    pub(crate) fn data(&self) -> &str {
        match self {
            Utf8Bytes::Heap(data) => data,
            Utf8Bytes::Borrowed(data) => data,
        }
    }
}

impl<'data> Deref for Utf8Bytes<'data> {
    type Target = str;

    fn deref(&self) -> &Self::Target {
        self.data()
    }
}

impl<'data> PartialEq for Utf8Bytes<'data> {
    fn eq(&self, other: &Self) -> bool {
        self.data().eq(other.data())
    }
}

impl<'data> std::hash::Hash for Utf8Bytes<'data> {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.data().hash(state);
    }
}

impl<'data> PartialOrd for Utf8Bytes<'data> {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        self.data().partial_cmp(other.data())
    }
}

#[test]
fn comparison() {
    fn hash(sym: &Utf8Bytes) -> u64 {
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        sym.hash(&mut hasher);
        hasher.finish()
    }
    use std::hash::Hash;
    use std::hash::Hasher;

    let sym1 = Utf8Bytes::borrowed("sym1");
    let sym2 = Utf8Bytes::borrowed("sym2");
    assert_eq!(sym1, sym1.to_heap());
    assert!(sym1 < sym2);
    assert!(sym1.to_heap() < sym2);
    assert!(sym1 < sym2.to_heap());
    assert_eq!(hash(&sym1), hash(&sym1.to_heap()));
}
