pmacro1::crate_write_to_file!();

fn main() {
    let value = crab1::crab1(42);
    crab2::stuff::do_stuff();
    println!("{value}");
    write_to_file("a.txt", "Hello");
}
