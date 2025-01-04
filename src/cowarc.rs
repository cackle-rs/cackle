use std::hash::Hash;
use std::ops::Deref;
use std::sync::Arc;

pub(crate) type Bytes<'data> = CowArc<'data, [u8]>;
pub(crate) type Utf8Bytes<'data> = CowArc<'data, str>;

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

impl<T: ?Sized> CowArc<'_, T> {
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

impl<T: ?Sized> Clone for CowArc<'_, T> {
    fn clone(&self) -> Self {
        match self {
            Self::Heap(arg0) => Self::Heap(Arc::clone(arg0)),
            Self::Borrowed(arg0) => Self::Borrowed(arg0),
        }
    }
}

impl<V: Clone> CowArc<'_, [V]> {
    /// Create an instance that is heap-allocated and reference counted and thus can be used beyond
    /// the lifetime 'data.
    pub(crate) fn to_heap(&self) -> CowArc<'static, [V]> {
        CowArc::Heap(match self {
            CowArc::Heap(data) => Arc::clone(data),
            CowArc::Borrowed(data) => Arc::from(*data),
        })
    }
}

impl CowArc<'_, str> {
    /// Create an instance that is heap-allocated and reference counted and thus can be used beyond
    /// the lifetime 'data.
    pub(crate) fn to_heap(&self) -> CowArc<'static, str> {
        CowArc::Heap(match self {
            CowArc::Heap(data) => Arc::clone(data),
            CowArc::Borrowed(data) => Arc::from(*data),
        })
    }
}

impl<T: ?Sized> Deref for CowArc<'_, T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        self.data()
    }
}

impl<T: PartialEq + ?Sized> PartialEq for CowArc<'_, T> {
    fn eq(&self, other: &Self) -> bool {
        self.data().eq(other.data())
    }
}

impl<T: Hash + ?Sized> Hash for CowArc<'_, T> {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.data().hash(state);
    }
}

impl<T: PartialOrd + ?Sized> PartialOrd for CowArc<'_, T> {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        self.data().partial_cmp(other.data())
    }
}

impl<T: Eq + ?Sized> Eq for CowArc<'_, T> {}

impl<T: Ord + ?Sized> Ord for CowArc<'_, T> {
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
