fn fib(n: i64) -> i64 { if n <= 1 { return n; } fib(n - 1) + fib(n - 2) }
fn main() { println!("{}", std::hint::black_box(fib(std::hint::black_box(38)))); }
