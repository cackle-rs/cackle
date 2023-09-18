#[no_mangle]
pub extern "C" fn shared1_entry1() {
    let v = ["a.txt"];
    crab_1::read_file(v[0]);
}

#[no_mangle]
pub extern "C" fn shared1_entry2() {
    println!("{:?}", std::env::var("HOME"));
    crab_2::stuff::do_stuff();
}

#[allow(dead_code)]
pub fn an_unused_function() {
    // Since this function isn't used, the use of the "process" API should be ignored. Even though
    // it's marked as public, it should be discarded when linking the shared object.
    std::process::Command::new("ls").output().unwrap();
}
