# Cackle / cargo acl

A code ACL checker for Rust.

Cackle is a tool to analyse the transitive dependencies of your crate to see what kinds of APIs each
crate uses.

The idea is look for crates that are using APIs that you don't think they should be using. For
example a crate that from its description should just be doing some data processing, but is actually
using network APIs.

## Installation

Currently Cackle only works on Linux. See [PORTING.md](PORTING.md) for more details.

```sh
cargo install --locked cargo-acl
```

Or if you'd like to install from git:

```sh
cargo install --locked --git https://github.com/cackle-rs/cackle.git cargo-acl
```

Installing `bubblewrap` is recommended as it allows build scripts (build.rs), tests and rustc to be
run inside a sandbox.

On systems with `apt`, this can be done by running:

```sh
sudo apt install bubblewrap
```

## Usage

From the root of your project (the directory containing `Cargo.toml`), run:

```sh
cargo acl
```

This will interactively guide you through creating an initial `cackle.toml`. Some manual editing of
your `cackle.toml` is recommended. In particular, you should look through your dependency tree and
think about which crates export APIs that you'd like to restrict. e.g. if you're using a crate that
provides network APIs, you should declare this in your config. See [CONFIG.md](CONFIG.md) for more
details.

## Running from CI

Cackle can be run from GitHub actions. See the instructions in the
[cackle-action](https://github.com/cackle-rs/cackle-action) repository.

## Features

* Checks what APIs are used by each crate in your dependency tree.
* Ignores dead code, so if a crate uses an API, but in code that isn't called in your binary, then
  it doesn't count.
* Restrict which crates are allowed to use unsafe.
* A terminal UI that shows problems as they're found.
  * Preview the source where the API usage or unsafe was detected.
  * For API usages, show a backtrace of how that code is reachable.
  * Select from several edits that can be applied to your config file to allow the usage.
* Can run build scripts, tests in a sandbox to restrict network and filesystem access.
* The sandbox for each build script is configured separately, so if one build script needs extra
  access you can grant it to just that build script.
* Can run rustc in a sandbox, thus sandboxing all proc macros. This however is currently not
  granular, so if one proc macro needs more access it needs to be granted to all. Fortunately proc
  macros that need network access are relatively rare.

## Limitations and precautions

* A proc macro might detect that it's being run under Cackle and emit different code.
* Even without proc macros, a crate may only use problematic APIs only in certain configurations
  that don't match the configuration used when you run Cackle.
* This tool is intended to supplement and aid manual review of 3rd party code, not replace it.
* Your configuration might miss defining an API provided by a crate as falling into a certain
  category that you care about.
* There are undoubtedly countless ways that a determined person could circumvent detection that
  they're using some APIs. With time we may try to prevent such circumventions, but for now, you
  should definitely assume that circumvention is possible.

With all these limitations, what's the point? The goal really is to just raise the bar for what's
required to sneak problematic code unnoticed into some package. Use of Cackle should not replace any
manual code reviews of your dependencies that you would otherwise have done.

## How it works

See [HOW_IT_WORKS.md](HOW_IT_WORKS.md).

## FAQ

[FAQ](FAQ.md)

## Contributing

Contributions are very welcome. If you'd like to get involved, please reach out either by filing an
issue or emailing David Lattimore (email address is in the commit log).

## License

This software is distributed under the terms of both the MIT license and the Apache License (Version
2.0).

See LICENSE for details.

Unless you explicitly state otherwise, any contribution intentionally submitted for inclusion in
this crate by you, as defined in the Apache-2.0 license, shall be dual licensed as above, without
any additional terms or conditions.
