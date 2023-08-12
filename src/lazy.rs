use std::marker::PhantomData;

use anyhow::bail;
use anyhow::Result;

/// Something that can be lazily computed.
pub(crate) trait Lazy<T, P> {
    /// Returns the computed value, computing it first if necessary. `param` can be used to aid in
    /// computation.
    fn get(&mut self, param: &P) -> Result<&T>;
}

struct LazyImpl<T, P, F: FnOnce(&P) -> Result<T>> {
    value: Option<T>,
    compute_fn: Option<F>,
    _p: PhantomData<P>,
}

/// Returns an opaque type that can be lazily computed. Most of what's needed to compute the value
/// should be captured by the closure `compute_fn`, however if there is stuff that can't be captured
/// by the closure due to lifetime issues, it can be passed in as the param to `Lazy::get`.
pub(crate) fn lazy<T, P, F>(compute_fn: F) -> impl Lazy<T, P>
where
    F: FnOnce(&P) -> Result<T>,
{
    LazyImpl {
        value: None,
        compute_fn: Some(compute_fn),
        _p: PhantomData,
    }
}

impl<T, P, F: FnOnce(&P) -> Result<T>> Lazy<T, P> for LazyImpl<T, P, F> {
    fn get(&mut self, param: &P) -> Result<&T> {
        if self.value.is_none() {
            let Some(c) = self.compute_fn.take() else {
                bail!("Lazy::get called after error already reported");
            };
            self.value = Some((c)(param)?);
        }
        Ok(self.value.as_ref().unwrap())
    }
}
