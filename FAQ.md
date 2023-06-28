# Frequently asked questions

If you've got a question, file an issue and we'll attempt to answer it and possibly add it to this
FAQ.

## Cackle reports that a crate uses unsafe, but it doesn't

Try compiling the crate with `cargo rustc -- -F unsafe-code` and see what errors it reports. Use of
`#[no_mangle]` or `#[link_section =...]` for example count as use of unsafe.

Additionally, having the `unsafe` keyword anywhere in a crates sources will cause it to be marked as
using unsafe, even if that keyword gets discarded by a macro.
