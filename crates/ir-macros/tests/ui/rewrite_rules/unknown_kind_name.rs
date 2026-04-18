use ir_macros::rewrite_rules;

fn main() {
    // FooBar is not a known LHS kind; the parser returns it as an OutputCapture,
    // then fails because the remaining `(x)` cannot be consumed as `=>`.
    let _ = rewrite_rules! {
        FooBar(x) => x,
    };
}
