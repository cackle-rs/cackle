use std::ops::Deref;
use std::sync::Arc;

/// Some bytes that might be borrowed or might be on the heap. A bit like a Cow, but uses reference
/// counting when storing on the heap. All comparison, hashing etc is done based on the stored data.
/// i.e. two instances that store the data, with one being on the heap and the other not, should
/// behave the same.
#[derive(Eq, Clone, Ord, Debug)]
pub(crate) enum Bytes<'data> {
    Heap(Arc<[u8]>),
    Borrowed(&'data [u8]),
}

impl<'data> Bytes<'data> {
    pub(crate) fn borrowed(data: &[u8]) -> Bytes {
        Bytes::Borrowed(data)
    }

    /// Create an instance that is heap-allocated and reference counted and thus can be used beyond
    /// the lifetime 'data.
    pub(crate) fn to_heap(&self) -> Bytes<'static> {
        Bytes::Heap(match self {
            Bytes::Heap(data) => Arc::clone(data),
            Bytes::Borrowed(data) => Arc::from(*data),
        })
    }

    pub(crate) fn data(&self) -> &[u8] {
        match self {
            Bytes::Heap(data) => data,
            Bytes::Borrowed(data) => data,
        }
    }
}

impl<'data> Deref for Bytes<'data> {
    type Target = [u8];

    fn deref(&self) -> &Self::Target {
        self.data()
    }
}

impl<'data> PartialEq for Bytes<'data> {
    fn eq(&self, other: &Self) -> bool {
        self.data().eq(other.data())
    }
}

impl<'data> std::hash::Hash for Bytes<'data> {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.data().hash(state);
    }
}

impl<'data> PartialOrd for Bytes<'data> {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        self.data().partial_cmp(other.data())
    }
}

#[test]
fn comparison() {
    fn hash(sym: &Bytes) -> u64 {
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        sym.hash(&mut hasher);
        hasher.finish()
    }
    use std::hash::Hash;
    use std::hash::Hasher;

    let sym1 = Bytes::borrowed(b"sym1");
    let sym2 = Bytes::borrowed(b"sym2");
    assert_eq!(sym1, sym1.to_heap());
    assert!(sym1 < sym2);
    assert!(sym1.to_heap() < sym2);
    assert!(sym1 < sym2.to_heap());
    assert_eq!(hash(&sym1), hash(&sym1.to_heap()));
}
