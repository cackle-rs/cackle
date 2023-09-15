# Configuration format

Cackle is configured via a `cackle.toml`, which by default is located in the package or workspace
root.

## API definitions

Example:

```toml
[api.process]
include = [
    "std::process",
]
exclude = [
    "std::process::abort",
    "std::process::exit",
]
```

Here we define an API called "process". Any package that references symbols in `std::process` is
considered to use this API except if the symbol referenced is `std::process::abort` or
`std::process::exit`, which are excluded from the `process` API.

We can define as many APIs as we like. If an API is declared, then packages need permission in order
to use those APIs.

## Importing standard library API definitions

Cackle has some built-in API definitions for the Rust standard library that can optionally be used.

```toml
import_std = [
    "fs",
    "net",
    "process",
    "env",
    "terminate",
]
```

## Package permissions

We can grant permissions to a package to use APIs or use unsafe. e.g.:

```toml
[pkg.crab1]
allow_unsafe = true
allow_apis = [
    "fs",
    "process",
]
```

Here we declare a package called `crab1` and say that it is allowed to use the `fs` and `process`
APIs. We also say that it's allowed to use unsafe code.

We can also conditionally grant permissions to use APIs only from particular kinds of binaries. For
example, if we wanted to allow `crab1` to use the `fs` API, but only in code that is only reachable
from test code, we can do that as follows:

```toml
[pkg.crab1]
dep.test.allow_apis = [
    "fs",
]
```

Similarly, we can allow the API to be used, but only in code that is reachable from build scripts:

```toml
[pkg.crab1]
dep.build.allow_apis = [
    "fs",
]
```

If we want to allow an API to be used specifically by `crab1`'s build script, we can do that as follows:

```toml
[pkg.crab1]
build.allow_apis = [
    "fs",
]
```

Allowed APIs inherit as follows:

* pkg.N
  * pkg.N.dep.build (any build script)
    * pkg.N.build (N's build script)
  * pkg.N.dep.test (any test)
    * pkg.N.test (N's tests)

So granting an API usage to `pkg.N` means it can be used in any kind of binary.

## Sandbox

```toml
[sandbox]
kind = "Bubblewrap"
```

Here we declare that we'd like to use `Bubblewrap` (installed as `bwrap`) as our sandbox. Bubblewrap
is currently the only supported kind of sandbox. The sandbox will be used for running build scripts
(build.rs).

If for some reason you don't want to sandbox a particular build script, you can disable the sandbox
just for that build script.

```toml
[pkg.foo.build]
sandbox.kind = "Disabled"
```

If a build script needs network access, you can relax the sandbox to allow it as follows:

```toml
[pkg.foo.build]
sandbox.allow_network = true
```

Tests can also be run in a sandbox using the `cargo` subcommand, for example:

```sh
cackle cargo test
```

This builds the tests, performing the same permission checks as doing `cackle check`, then runs the
tests, with a sandbox if one is configured.

The sandbox used for tests is configured under `pkg.{pkg-name}.test`. e.g.:

```toml
[pkg.foo.test]
sandbox.kind = "Disabled"
```

Tests and build scripts already have write access to a temporary directory, however, if for some
reason they need to write to some directory in your source folder, this can be permitted as follows:

```toml
[pkg.foo.test]
sandbox.bind_writable = [
    "test_outputs",
]
```

This will allow tests to write to the "test_outputs" subdirectory within the directory containing
your `Cargo.toml`. All directories listed in `bind_writable` must exist.

If you'd like to automatically create a writable directory if it doesn't already exist, then
`make_writable` behaves the same, but will create the directory before starting the sandbox.

```toml
[pkg.foo.test]
sandbox.make_writable = [
    "test_outputs",
]
```

## Importing API definitions from an external crate

If you depend on a crate that publishes `cackle/export.toml`, you can import API definitions from
this as follows:

```toml
[pkg.some-dependency]
import = [
    "fs",
]
```

API definitions imported like this will be namespaced by prefixing them with the crate that exported
them. For example:

```toml
[pkg.my-bin]
allow_apis = [
    "some-dependency::fs",
]
```

If you're the owner of a crate that provides APIs that you'd like classified, you can create
`cackle/export.toml` in your crate.

## Build options

### Specifying features

Features to be be passed to `cargo build` can be specified in `cackle.toml` as follows:

```toml
features = ["feature1", "feature2"]
```

### Selecting build targets

Arbitrary build flags can be passed to `cargo build` using the `build_flags` option. The default is
to pass `--all-targets`.

```toml
[common]
build_flags = ["--all-targets"]
```

If you'd like to not analyse tests, examples etc, you might override this to just the empty array
`[]`. Or if you want to analyse tests, but not examples you might set it to `["--tests"]`. For
available options run `cargo build --help`.

### Custom build profile

By default, Cackle builds with a custom profile named "cackle" which inherits from the "dev"
profile. If you'd like to use a different profile, you can override the profile in the configuration
file. e.g.

```toml
[common]
profile = "cackle-release"
```

You can also override with the `--profile` flag, which takes precidence over the config file.

Cackle supports analysing references even when inlining occurs, so it can work to some extent even
with optimisations enabled, however it's more likely that you'll run into false attribution bugs,
where an API usage is attributed to the wrong package. So unless you really need optimisation for
some reason, it's recommended to set `opt-level = 0`.

Split debug info is not yet supported, so you should turn it off.

Here's an example of what you might put in your `Cargo.toml`:

```toml
[profile.cackle-release]
inherits = "release"
opt-level = 0
split-debuginfo = "off"
strip = false
debug = 2
lto = "off"
```

## Version number

The field `common.version` is the only required field in the config file.

```toml
[common]
version = 1
```

At present, the only supported version is 1. If we decide to change the default values for any
fields in future, we'll add a new supported version number and 1 will continue to have the defaults
it has now. In this regard, `common.version` is a bit like `package.edition` in `Cargo.toml`. It's
intended as a way to preserve old behaviour while making breaking changes, in particular breaking
changes that might otherwise go unnoticed.
