## Porting to other operating systems

Currently only Linux is supported. The following sections describe known issues that would need to
be resolved in order to get it working on other operating systems.

### Mac

Mac fortunately uses ELF for binaries and DWARF for debug info. Unfortunately it looks like it
doesn't put each function into a separate section, which our code currently relies upon. It may be
possible to convince it to use a separate section per function, but if not, it may be necessary to
figure out the start and end address of each function and combine that with the address of each
relocation in order to figure out the function graph.

There also appears to be some differences in how DWARF is used.

### Windows

Windows uses both a different object file format and a different format for debug info. The library
that we use for reading object files (`object`) apparently supports the format used on Windows,
although it's likely that some of our code would still need some adjusting.

The larger bit of work is handling the debug info format used on Windows.
