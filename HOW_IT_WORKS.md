# How it works

Cackle performs a `cargo build` of your crate and wraps rustc, the linker and any build scripts. By
wrapping these binaries, cackle gets an opportunity to perform analysis during the build process.

## Wrapping rustc

The code for this is `proxy_rustc` in src/proxy/subprocess.rs.

The first binary that cackle wraps is `rustc`. It wraps it by setting the environment variable
`RUSTC_WRAPPER`, which causes cargo to invoke `cackle` instead of the real `rustc`.

We adjust the command line and then invoke the real `rustc`. The most important adjustments we make
to the `rustc` command line are:

* We add `-Funsafe-code` unless `cackle.toml` says that the crate is allowed to use unsafe.
* We override the linker used by rustc so that we can wrap that as well.
* We force emitting of debug info, which is needed for later analysis.

Once `rustc` completes, we parse the emitted `deps` file to get a list of the source files that were
used. We then notify the parent process that `rustc` completed, telling it what source files were
used.

In addition to telling the parent process what source files were used, we also parse the source
files, looking for the `unsafe` token. This is an additional layer of unsafe detection besides
adding `-Funsafe-code` since `-Funsafe-code` is insufficient to prevent some uses of unsafe.

## Wrapping the linker

The code for this is `proxy_linker` in src/proxy/subprocess.rs.

When cackle is invoked as the linker, first invoke the actual linker. We then look through all the
arguments passed to the linker to determine:

* What object files and rlibs are being linked
* What binary output (executable or shared object) is being produced

We pass this information to the main cackle process. The main process stores this information for
later analysis when the current `rustc` invocation finishes. The reason it doesn't analyse the
linker invocation is because it needs the list of source files for the current crate, which we get
from the deps file, which is written by rustc.

When `rustc` does finish, the parent process the analyses the `LinkInfo` to determine what APIs were
used and by which crates. For more details on this analysis, see [API analysis](#api-analysis).

If the output of the linker is a build script or a test, then we rename the output and put a shell
script in its place. This lets us wrap build scripts and tests.

## Wrapping build scripts

The code for this is `proxy_build_script` in src/proxy/subprocess.rs.

When cargo invokes a build script or test, it's actually running a shell script that we put in its
place. This shell script invokes cackle, telling it what kind of binary is being invoked and where
the actual binary is located. Cackle then checks to see if the binary being invoked needs to be run
in a sandbox.

## API analysis

The code for this is in `src/symbol_graph.rs`.

When rust invokes our proxy linker, it notifies the main cackle process to tell it which binary file
was linked and which object files were used as inputs.

Cackle reads relocations from the object files. Relocation are generally a reference from one symbol
to another, although both the source and target of the relocation can also be a linker section, with
no symbol involved, which adds a little complexity.

In order to check if a reference is permitted, we need to know:

* What crate the reference came from
* What API was referenced

We determine the crate that the reference came from as follows:

* The reference is always attached to a section of an object file. That section may have a symbol
  definition in it. If it does, we look for that symbol in the output binary.
  * If the output binary doesn't have that symbol, then we fall back to using debug information for
    the symbol.
  * If we have neither a symbol definition nor debug information for the symbol, then we ignore the
    reference, since it's from dead code and we don't care about APIs used by dead code.
  * If the output binary does have that symbol, then we use the offset of relocation relative to the
    symbol to determine the relocation address within the output binary.
* Assuming we have a source location for where the relocation was applied, we use the deps files
  written by the rust compiler when it compiles each crate to determine which crate (or in rare
  circumstances crates) the source file belongs to.

We determine what API was referenced as follows:

* Look at the target of the relocation. If it's a section that doesn't define a symbol, then collect
  all symbols referenced by that section recursively until we have just a list of referenced symbols.
* For each symbol, use both the demangled name of the symbol and the name provided by the debug
  information for that symbol. For most symbols, the symbol name is redundant as the debug name
  generally provides more information. There are however a few cases where the symbol contains
  information that the debug name doesn't, so we still need to process both.
* We then split the debug name and symbol into names and look for any defined APIs in `cackle.toml`
  that are the prefix of these names.
* Where a function uses an API and also has a name that matches that same API, we ignore the usage
  by that function. The usage will be attributed to whatever uses that function. The idea here is
  that if a crate defines a generic function, we don't want API usage to be attributed to that crate
  just because some other crate instantiated the generic function with some type that matched an
  API. e.g. if the either crate defines `Either<L,R>` and some other crate uses `Either<Path,Path>`,
  we want to attribute the filesystem API only to the latter crate, not to the `either` crate.
