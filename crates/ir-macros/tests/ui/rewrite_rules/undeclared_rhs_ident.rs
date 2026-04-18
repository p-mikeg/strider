use ir_macros::rewrite_rules;

fn main() {
    let _ = rewrite_rules! {
        (x + IntConst(0)) => y,
    };
}
