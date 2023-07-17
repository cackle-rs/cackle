use std::net::ToSocketAddrs;

fn main() {
    if option_env!("CACKLE_TEST_NO_NET").is_none() {
        "rust-lang.org:443"
            .to_socket_addrs()
            .expect("Failed to resolve rust-lang.org");
    }
    assert!(crab5::do_something());
}
