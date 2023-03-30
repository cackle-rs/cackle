# Cackle

**This is a work in progress and isn't ready for use yet. It's published just so that others can see
what's being worked on. It hasn't even been tested on a non-trivial crate yet.**

Cackle is a tool to analyse the transitive dependencies of your crate to see what kinds of APIs each
crate uses.

The idea is look for crates that are using APIs that you don't think they should be using. For
example a crate that from its description should just be transforming some data and returning the
result, but is actually using network APIs.

## Limitations and precautions

* Analyzing a crate could well end up executing arbitrary code provided by that crate. If this is a
  concern, then running in a sandbox is recommended.
* This tool is intended to supplement and aid manual review of 3rd party code, not replace it.
* There are undoubtedly countless ways that a determined person could circumvent detect that they're
  using some APIs. With time we may try to prevent such circumventions, but you shouldn't assume
  that this is in any way unable to be circumvented.

## License

This software is distributed under the terms of both the MIT license and the Apache License (Version
2.0).

See LICENSE for details.
