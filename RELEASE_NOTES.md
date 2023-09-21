# Version 0.3.0
* Renamed to cargo-acl
* UI now only activates when necessary.
* `check` and `ui` subcommands removed - now just run with no subcommand and turn the UI off with
  `--no-ui` or `-n`.
* `cargo` subcommand removed. Instead of `cackle cargo test`, now you run `cargo acl test`.
* Now available as a github action.
* Allow APIs based on what kind of binary is being built (test, build script or other)
* `sandbox.make_writable` can be used to create directories that need to be writable
* Automatic edits now use dotted notation within `pkg.x` rather than defining a separate
  `pkg.x.build` etc.
* Backtraces can now display sources from the rust standard library.
* Various other bug fixes

# Version 0.2.0
* Fixed a few false-attribution problems.
* Syntax highlight code snippets.
* Optimised Cackle's analysis speed ~4x faster.
* Added `cargo` subcommand. e.g. `cackle cargo test`.
* Supports running tests in sandbox.
* Sandbox config now supports making select directories writable.
* Support showing a backtrace of how an API usage location is reachable.
* Output from `cargo build` is now shown when running `cackle check`.
* Added automated config edit to exclude a path from an API.
* Binary releases now available on github.
