use anyhow::bail;
use anyhow::Result;

/// Something that can be lazily computed.
pub(crate) trait Lazy<T> {
    /// Returns the computed value, computing it first if necessary.
    fn get(&mut self) -> Result<&T>;
}

struct LazyImpl<T, F: FnOnce() -> Result<T>> {
    value: Option<T>,
    compute_fn: Option<F>,
}

/// Returns an opaque type that can be lazily computed.
pub(crate) fn lazy<T, F>(compute_fn: F) -> impl Lazy<T>
where
    F: FnOnce() -> Result<T>,
{
    LazyImpl {
        value: None,
        compute_fn: Some(compute_fn),
    }
}

impl<T, F: FnOnce() -> Result<T>> Lazy<T> for LazyImpl<T, F> {
    fn get(&mut self) -> Result<&T> {
        if self.value.is_none() {
            let Some(c) = self.compute_fn.take() else {
                bail!("Lazy::get called after error already reported");
            };
            self.value = Some((c)()?);
        }
        Ok(self.value.as_ref().unwrap())
    }
}
