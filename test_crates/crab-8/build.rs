fn main() {
    maybe_abort(false);
}

// This function is private, so doesn't go into th symbol table. This can make it harder for us to
// find the source locations for references from this function.
fn maybe_abort(abort: bool) {
    if abort {
        crab_1::inlined_abort();
    }
}
