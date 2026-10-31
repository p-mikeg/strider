//! A node builder in a raw slot (`input`, `any_input`, `phi_token`,
//! `call_other().arg`) nests through whichever of its outputs feeds the slot,
//! so a call there binds its control or memory edge as well as a value.

use strider_ir::Function;
use strider_orchestrator::AnalyzeOptions;
use strider_pattern::{
    Matcher, Pattern, call, call_other, first_of, load, one_of, region, ret, store,
};

use crate::common::fixtures::{FIXTURE_ARCHES, Fixture};

/// Every x86 and x64 fixture function that analyses.
fn functions() -> Vec<Function> {
    let mut out = Vec::new();
    for arch in FIXTURE_ARCHES
        .iter()
        .filter(|a| matches!(a.dir, "x86" | "x64"))
    {
        for fixture in Fixture::all(arch) {
            let (mut strider, cc) = fixture.strider();
            for (addr, _) in fixture.functions() {
                if let Ok(r) = strider.analyze(addr, &cc, &AnalyzeOptions::default(), None) {
                    out.push(r.function);
                }
            }
        }
    }
    out
}

fn count(functions: &[Function], pat: &Pattern) -> usize {
    functions
        .iter()
        .map(|f| Matcher::new(f).find_all(pat).expect("find_all").len())
        .sum()
}

/// `$at` over `$x`, `one_of![$x]` and `first_of![$x]`.
macro_rules! three {
    ($fns:expr, |$p:ident| $at:expr, $x:expr) => {
        [
            {
                let $p = $x;
                count($fns, &$at.build())
            },
            {
                let $p = one_of![$x];
                count($fns, &$at.build())
            },
            {
                let $p = first_of![$x];
                count($fns, &$at.build())
            },
        ]
    };
}

#[test]
fn a_call_in_a_raw_slot_binds_any_of_its_edges() {
    let fns = &functions();
    let rows = [
        (
            "region().any_input(call())",
            three!(fns, |x| region().any_input(x), call()),
            22,
        ),
        (
            "load().input(0, call())",
            three!(fns, |x| load().input(0, x), call()),
            203,
        ),
        (
            "call().input(0, call())",
            three!(fns, |x| call().input(0, x), call()),
            194,
        ),
        (
            "ret().any_input(call_other())",
            three!(fns, |x| ret().any_input(x), call_other()),
            32,
        ),
        (
            "call_other().arg(1, call())",
            three!(fns, |x| call_other().arg(1, x), call()),
            32,
        ),
        // A memory slot still takes the token alone.
        (
            "store().mem(call().res())",
            three!(fns, |x| store().mem(x), call().res()),
            229,
        ),
        (
            "load().mem(call().res())",
            three!(fns, |x| load().mem(x), call().res()),
            203,
        ),
    ];
    for (name, got, want) in rows {
        assert_eq!(got, [want; 3], "{name}");
    }
}
