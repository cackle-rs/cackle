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

Example:

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
