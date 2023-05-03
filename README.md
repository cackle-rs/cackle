# Cackle

**This is a work in progress and isn't ready for use yet. It's published just so that others can see
what's being worked on.

Cackle is a tool to analyse the transitive dependencies of your crate to see what kinds of APIs each
crate uses.

The idea is look for crates that are using APIs that you don't think they should be using. For
example a crate that from its description should just be transforming some data and returning the
result, but is actually using network APIs.

## Installation

Currently Cackle only works on Linux. Support for OSX should be possible, but it's working yet.
Windows will be more tricky, since we need debug info in order to work out where a bit of code came
from. Right now, we only support DWARF debug info.

```sh
cargo install cackle
```

Installing `bubblewrap` is recommended as it allows build.rs build scripts to be run inside a
sandbox.

On systems with `apt`, this can be done by running:

```sh
sudo apt install bubblewrap
```

## Limitations and precautions

* Exported macros that expand to unsafe code cause neither the crate defining the macro, nor the
  crate using the macro to be flagged as using unsafe. This can be used to circumvent all other
  protections.
* A proc macro might detect that it's being run under Cackle and emit different code.
* Even without proc macros, a crate may only use problematic APIs only in certain configurations
  that don't match the configuration used when you run Cackle.
* Analyzing a crate could well end up executing arbitrary code provided by that crate. If this is a
  concern, then running in a sandbox is recommended.
* This tool is intended to supplement and aid manual review of 3rd party code, not replace it.
* Your configuration might miss defining an API provided by a crate as falling into a certain
  category that you care about.
* There are undoubtedly countless ways that a determined person could circumvent detection that
  they're using some APIs. With time we may try to prevent such circumventions, but you shouldn't
  assume that this is in any way unable to be circumvented.

With all these limitations, what's the point? The goal really is to just raise the bar for what's
required to sneak problematic code unnoticed into some package. Use of Cackle should not replace any
manual code reviews of your dependencies that you would otherwise have done.

## How it works

See [HOW_IT_WORKS.md](HOW_IT_WORKS.md).

## License

This software is distributed under the terms of both the MIT license and the Apache License (Version
2.0).

See LICENSE for details.
