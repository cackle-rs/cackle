# Frequently asked questions

If you've got a question, file an issue and we'll attempt to answer it and possibly add it to this
FAQ.

## Cackle reports that a crate uses unsafe, but it doesn't

Try compiling the crate with `cargo rustc -- -F unsafe-code` and see what errors it reports. Use of
`#[no_mangle]` or `#[link_section =...]` for example count as use of unsafe.

Additionally, having the `unsafe` keyword anywhere in a crates sources will cause it to be marked as
using unsafe, even if that keyword gets discarded by a macro.

## Why do some problems only show in the UI once I've fixed others

Cackle analyses your dependencies as the cargo build progresses. When disallowed APIs or disallowed
unsafe code is encountered, that part of the build is paused until the problems encountered are
fixed or until the build is aborted. Once you fix the problems by selecting fixes through the UI,
the build continues and finds more problems with later crates.

## It's very slow having to manually review each permission

It's probably a good idea to spend some time thinking about whether each crate that uses an API has
a legitimate use for it. That said, if you'd like to just "accept all" so to speak, you can press
"a" to accept all permissions that have only a single edit. You'll still be prompted for problems
that have more than one edit. When you're done, you could then look over the generated cackle.toml
to see if anything jumps out at you as using an API that it shouldn't. One advantage of not doing
accept-all is that the user interface gives you information about each usage, which can help in
understanding why a package is using a particular API or unsafe.

## Do build scripts get run before you grant them needed permissions

They don't. Compilation of a build script will pause until you grant it permission to use whatever
APIs it need. If you quit out of the Cackle UI without granting required permissions, then
compilation of the build script will abort.

## Why do you analyse object files rather than looking at the source AST

Cackle did originally use rust-analyzer. When I switched to binary analysis, my main motivation was
wanting to get accurate span information for code that originated from macros. I wouldn't rule out
switching back to source analysis at some point. It's possible that we could even end up with both
binary and source-based analysis. Ideally what I'd like would be if we could get rustc emit HIR in
some stable format. e.g. a JSON dump of the AST with all paths resolved and with span information.
