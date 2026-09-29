fn sum_to(n: i64) -> i64 { let mut t: i64 = 0; for i in 0..n { t = t.wrapping_add(i.wrapping_mul(3)).wrapping_sub(i / 7); } t }
fn main() { println!("{}", std::hint::black_box(sum_to(std::hint::black_box(100_000_000)))); }
