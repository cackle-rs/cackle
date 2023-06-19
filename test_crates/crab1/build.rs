use std::net::ToSocketAddrs;

fn main() {
    "rust-lang.org:443"
        .to_socket_addrs()
        .expect("Failed to resolve rust-lang.org");
    assert!(crab5::do_something());
}
