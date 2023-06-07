# Configuration format

Cackle is configured via a cackle.toml, which by default is located in the package or workspace
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

Here we declare a package called `crab1` and say that it is allowed to use the APIs `fs` and
`process`. We also say that it's allowed to use unsafe code.

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
[pkg.foo.build.sandbox]
kind = "Disabled"
```

If a build script needs network access, you can relax the sandbox to allow it as follows:

```toml
[pkg.foo.build.sandbox]
allow_network = true
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

## Ignoring unreachable code

If you're using a package that uses some sensitive APIs, but you're not using the parts of the
package that use those APIs, you can opt to ignore unreachable code in that package. e.g.

```toml
[pkg.some-dependency]
ignore_unreachable = true
```

It's worth noting though, that if there's a bug in our code to compute reachability, then this may
mean that API usage gets ignored when it shouldn't be. For this reason, it's only recommended to use
this when you've checked that the API usage is in a part of the package that you're sure isn't being
used.

Reachability is computed based on the graph of symbol references starting from some roots (e.g.
main).

This option currently won't work if you're compiling a shared object (other than a proc macro),
since we haven't yet implemented a way to find the roots of a shared object for the purposes of
computing reachability.

Reachability only ever applies to API usage. Reachability doesn't affect checking for unsafe code.

## Specifying features

Features to be be passed to `cargo build` can be specified in `cackle.toml` as follows:

```toml
features = ["feature1", "feature2"]
```
