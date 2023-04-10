#[inline(never)]
pub fn crab1(v: u32) -> u32 {
    unsafe {
        let mut v2: [u8; 4] = core::mem::transmute(v);
        v2.reverse();
        println!("hello from crab1");
        core::mem::transmute(v2)
    }
}
