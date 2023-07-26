## Porting to other operating systems

Currently only Linux is supported. The following sections describe known issues that would need to
be resolved in order to get it working on other operating systems.

If you'd like to work on porting, you can build on other operating systems with:

```bash
cargo build --features unsupported-os
```

### Mac

Fortunately Mac, like Linux, uses ELF for binaries and DWARF for debug info. However it looks like
Mac possibly doesn't put each symbol into a separate section of the object file like happens on
Linux. This may cause problems for Cackle that would need to be resolved. Someone with a Mac would
need to try running it and investigate what goes wrong. Please reach out if you have a Mac and would
like to help with this.

### Windows

Windows uses both a different object file format and a different format for debug info. The library
that we use for reading object files (`object`) apparently supports the format used on Windows,
although it's likely that some of our code would still need some adjusting.

The larger bit of work is handling the debug info format used on Windows.
