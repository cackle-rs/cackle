pmacro1::create_write_to_file!();

fn main() {
    let value = crab1::crab1(42);
    println!("{value}");
    write_to_file("a.txt", "Hello");
    crab2::stuff::do_stuff();
}
