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
