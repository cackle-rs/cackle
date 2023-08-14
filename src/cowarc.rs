use std::hash::Hash;
use std::ops::Deref;
use std::sync::Arc;

pub(crate) type Bytes<'data> = CowArc<'data, [u8]>;

/// Provides a way to hold some data that is either borrowed, or on the heap. In this respect, it's
/// a bit like a Cow, but uses reference counting when storing on the heap, so Clone is always O(1).
/// Unlike a Cow, it's never writable, so the 'w' in the name is perhaps not technically accurate.
///
/// The intended use case is to create some data by borrowing, then later, when non-borrowed data is
/// needed, call `to_heap`, which gives us an instance with a 'static lifetime.
///
/// All comparison, hashing etc is done based on the stored data. i.e. two instances that store the
/// data, with one being on the heap and the other not, should behave the same.
#[derive(Debug)]
pub(crate) enum CowArc<'data, T: ?Sized> {
    Heap(Arc<T>),
    Borrowed(&'data T),
}

impl<'data, T: ?Sized> CowArc<'data, T> {
    /// Returns a reference to the data contained within. Note that the returned reference is valid
    /// for the lifetime of `self`, not for 'data, since if we're stored on the heap, we can't
    /// provide a reference that's valid for 'data, which may be longer (and likely 'static).
    pub(crate) fn data(&self) -> &T {
        match self {
            CowArc::Heap(data) => data,
            CowArc::Borrowed(data) => data,
        }
    }
}

impl<'data, T: ?Sized> Clone for CowArc<'data, T> {
    fn clone(&self) -> Self {
        match self {
            Self::Heap(arg0) => Self::Heap(Arc::clone(arg0)),
            Self::Borrowed(arg0) => Self::Borrowed(arg0),
        }
    }
}

impl<'data, V: Clone> CowArc<'data, [V]> {
    /// Create an instance that is heap-allocated and reference counted and thus can be used beyond
    /// the lifetime 'data.
    pub(crate) fn to_heap(&self) -> CowArc<'static, [V]> {
        CowArc::Heap(match self {
            CowArc::Heap(data) => Arc::clone(data),
            CowArc::Borrowed(data) => Arc::from(*data),
        })
    }
}

impl<'data, T: ?Sized> Deref for CowArc<'data, T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        self.data()
    }
}

impl<'data, T: PartialEq + ?Sized> PartialEq for CowArc<'data, T> {
    fn eq(&self, other: &Self) -> bool {
        self.data().eq(other.data())
    }
}

impl<'data, T: Hash + ?Sized> Hash for CowArc<'data, T> {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.data().hash(state);
    }
}

impl<'data, T: PartialOrd + ?Sized> PartialOrd for CowArc<'data, T> {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        self.data().partial_cmp(other.data())
    }
}

impl<'data, T: Eq + ?Sized> Eq for CowArc<'data, T> {}

impl<'data, T: Ord + ?Sized> Ord for CowArc<'data, T> {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.data().cmp(other.data())
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

    let sym1 = Bytes::Borrowed(b"sym1");
    let sym2 = Bytes::Borrowed(b"sym2");
    assert_eq!(sym1, sym1.to_heap());
    assert_eq!(sym1, sym1.clone());
    assert!(sym1 < sym2);
    assert!(sym1.to_heap() < sym2);
    assert!(sym1 < sym2.to_heap());
    assert_eq!(hash(&sym1), hash(&sym1.to_heap()));
}
