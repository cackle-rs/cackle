# How it works

Cackle performs a `cargo build` of your crate and wraps various binaries that cargo and rustc
invoke. By wrapping these binaries, cackle gets an opportunity to perform analysis during the build
process.

## Wrapping rustc

The code for this is `proxy_rustc` in src/proxy/subprocess.rs.

The first binary that cackle wraps is `rustc`. It wraps it by setting the environment variable
`RUSTC_WRAPPER`, which causes cargo to invoke `cackle` instead of the real `rustc`.

We adjust the command line and then invoke the real `rustc`. The most important adjustments we make
to the `rustc` command line are:

* We add `-Funsafe-code` unless `cackle.toml` says that the crate is allowed to use unsafe.
* We override the linker used by rustc so that we can wrap that as well.

## Wrapping the linker

The code for this is `proxy_linker` in src/proxy/subprocess.rs.

When cackle is invoked as the linker, we look through all the arguments to find object files that
are being linked. We then open them and build a graph of what symbols reference what other symbols.
We use debug information to determine which source file, and thus which crate each symbol came from.
We then check this against the permissions for each package in `cackle.toml`.

If no permissions were violated, we call through to the real linker.

If the output of the linker is a build script (we're compiling a build.rs), then we rename the
output and copy the cackle binary in its place. This lets us wrap build scripts.

## Wrapping build scripts

The code for this is `proxy_build_script` in src/proxy/subprocess.rs.

When cackle is invoked as a build script, we check cackle.toml to see if a sandbox configuration is
defined. If it is, then we invoke the build script via the sandbox, otherwise we invoke the build
script directly.
