use ir_macros::rewrite_rules;

fn main() {
    // c1 is bound as IntConst (u64), x is bound as Output (NodeOutputId).
    // Using c1 & x at the RHS top level asks the macro to treat c1 as NodeOutputId,
    // which is a type mismatch caught at compile time.
    let _ = rewrite_rules! {
        (IntConst(c1) + x) => c1 & x,
    };
}
