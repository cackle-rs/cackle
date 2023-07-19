# Cackle

A code ACL checker for Rust.

Cackle is a tool to analyse the transitive dependencies of your crate to see what kinds of APIs each
crate uses.

The idea is look for crates that are using APIs that you don't think they should be using. For
example a crate that from its description should just be transforming some data and returning the
result, but is actually using network APIs.

## Installation

Currently Cackle only works on Linux. See [PORTING.md](PORTING.md) for more details.

Right now Cackle is under fairly active development and the version on crates.io is very very
out-of-date to the point of being unusable. So installing directly from git is recommended.

```sh
cargo install --git https://github.com/davidlattimore/cackle.git cackle
```

Installing `bubblewrap` is recommended as it allows build.rs build scripts to be run inside a
sandbox.

On systems with `apt`, this can be done by running:

```sh
sudo apt install bubblewrap
```

## Usage

From the root of your project (the directory containing `Cargo.toml`), run:

```sh
cackle ui
```

This will interactively guide you through creating an initial `cackle.toml`. Some manual editing of
your `cackle.toml` is recommended. In particular, you should look through your dependency tree and
think about which crates export APIs that you'd like to restrict. e.g. if you're using a crate that
provides network APIs, you should declare this in your config. See [CONFIG.md](CONFIG.md) for more
details.

## Configuration file format

See [CONFIG.md](CONFIG.md).

## Non-interactive use

If you're running Cackle in a non-interactive workflow, e.g. as part of CI, you might like to
disable the user interface for a slightly lighter build. Add `--no-default-features` to your `cargo
install` command-line.

## Limitations and precautions

* A proc macro might detect that it's being run under Cackle and emit different code.
* Even without proc macros, a crate may only use problematic APIs only in certain configurations
  that don't match the configuration used when you run Cackle.
* Analyzing a crate could well end up executing arbitrary code provided by that crate. If this is a
  concern, then running in a sandbox is recommended.
* This tool is intended to supplement and aid manual review of 3rd party code, not replace it.
* Your configuration might miss defining an API provided by a crate as falling into a certain
  category that you care about.
* There are undoubtedly countless ways that a determined person could circumvent detection that
  they're using some APIs. With time we may try to prevent such circumventions, but for now, you should definitely assume that circumvention is possible.

With all these limitations, what's the point? The goal really is to just raise the bar for what's
required to sneak problematic code unnoticed into some package. Use of Cackle should not replace any
manual code reviews of your dependencies that you would otherwise have done.

## Security policy

It's unlikely that Cackle will ever be completely impossible to circumvent. That doesn't mean that
it isn't useful though. Think of it like an antivirus that only knows about 90% of viruses.

If you've found a neat way to circumvent Cackle to sneak in some API usages that it shouldn't allow,
great, especially if there's a way to plug the hole. If there isn't a practical way to plug the
hole, then my thoughts are that we probably shouldn't provide detailed instructions for people who
want to perform supply-chain attacks. The goal is to make things as hard for them as possible.

So I'd say, if the problem is fixable, feel free to just file a bug or send a PR. If it's not
fixable, or you're not sure, feel free to just email me. You can find my email address by looking
through the commit logs for David Lattimore.

## Backward compatibility

We have a version number field in the configuration file. At least initially and for minor, obscure
changes, bug fixes etc, there probably won't be a version bump. This means that updating to a newer
version of Cackle might result in errors that require a change to your cackle.toml. In cases where a
change is being made that we think would require substantial unnecessary changes to people's
cackle.toml, we'll put the change behind a new configuration option and set the default for that
option based on the version field.

## How it works

See [HOW_IT_WORKS.md](HOW_IT_WORKS.md).

## FAQ

[FAQ](FAQ.md)

## License

This software is distributed under the terms of both the MIT license and the Apache License (Version
2.0).

See LICENSE for details.
