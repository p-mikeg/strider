//! A pattern chained in a loop is refused through its `root()`, never by
//! overflowing the stack, whichever recursion the chain goes through: a
//! decoration, a shared operand, the operand check, or the build side.

use strider_ir_test_utils::Tb;
use strider_pattern::{
    Capture, CaptureExt, MatchOnly, MatchPat, Matcher, Pattern, TemplatePat, WithOutput, anything,
    capture_typed, float_is_nan, float_le, int_add, region, var,
};

const LINKS: usize = 20_000;

fn add_5_3() -> strider_ir::Function {
    let mut t = Tb::empty();
    let a = t.u64(5);
    let b = t.u64(3);
    let sum = t.add(a, b);
    t.ret_val(sum)
}

fn refusal(pat: &Pattern) -> String {
    let function = add_5_3();
    match Matcher::new(&function).find_all(pat) {
        Ok(hits) => panic!("pattern must be refused, got {} matches", hits.len()),
        Err(e) => e.to_string(),
    }
}

/// On the 2 MiB a spawned thread gets by default.
fn on_thread(f: impl FnOnce() -> String + Send + 'static) -> String {
    std::thread::Builder::new()
        .stack_size(2 << 20)
        .spawn(f)
        .unwrap()
        .join()
        .unwrap()
}

fn widths() -> MatchOnly {
    let mut p = anything().of_width(8);
    for _ in 0..LINKS {
        p = p.of_width(8);
    }
    p
}

#[test]
fn a_decoration_chain_is_refused() {
    let err = on_thread(|| refusal(&widths().into_pattern()));
    assert!(err.contains("stack"), "{err}");
}

#[test]
fn a_decoration_chain_as_an_operand_is_refused() {
    let err = on_thread(|| refusal(&int_add(widths(), anything()).into_pattern()));
    assert!(err.contains("stack"), "{err}");
}

#[test]
fn a_shared_operand_chain_is_refused() {
    let err = on_thread(|| {
        let mut p = float_is_nan(anything());
        for _ in 0..LINKS {
            p = float_is_nan(p);
        }
        refusal(&p.into_pattern())
    });
    assert!(err.contains("deep"), "{err}");
    let err = on_thread(|| {
        let mut p = float_le(anything(), anything());
        for _ in 0..LINKS {
            p = float_le(p, anything());
        }
        refusal(&p.into_pattern())
    });
    assert!(err.contains("deep"), "{err}");
}

#[test]
fn a_build_side_chain_is_refused() {
    let err = on_thread(|| {
        let c = Capture::new();
        let mut p = capture_typed(c, var(c));
        for _ in 0..LINKS {
            p = capture_typed(c, p);
        }
        TemplatePat::into_template(p)
            .root()
            .unwrap_err()
            .to_string()
    });
    assert!(err.contains("stack"), "{err}");
}

/// Each output vertex is a frame of its own when the node matches.
#[test]
fn a_chain_of_output_vertices_is_refused() {
    let err = on_thread(|| {
        let c = Capture::new();
        let mut p = region();
        for _ in 0..LINKS {
            p = p.output(0).capture(c);
        }
        refusal(&p.build())
    });
    assert!(err.contains("nodes"), "{err}");
}

/// A shared operand is walked once per slot that names it, so a chain of
/// them unfolds past any count: the refusal says so rather than printing a
/// saturated one.
#[test]
fn an_unfolding_past_the_counter_is_refused_without_a_saturated_count() {
    let mut p = float_is_nan(anything());
    for _ in 0..100 {
        p = float_is_nan(p);
    }
    let err = refusal(&p.into_pattern());
    assert!(err.contains("nodes"), "{err}");
    assert!(!err.contains(&usize::MAX.to_string()), "{err}");
}
