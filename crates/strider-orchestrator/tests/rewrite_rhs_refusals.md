# A rewrite RHS must be buildable

`strider_opt::rewrite_rule(lhs, rhs)` bounds `rhs` on `TemplatePat`, so a
match-only RHS is a compile error. The control builds:

```rust
# use strider_orchestrator::opt::rewrite_rule;
# use strider_pattern::{Capture, int_add, template, var};
let (a, b) = (Capture::new(), Capture::new());
let _rule = rewrite_rule(int_add(var(a), var(b)), template::int_add(var(b), var(a)));
```

## Wildcards and match-only leaves

```rust,compile_fail,E0277
# use strider_orchestrator::opt::rewrite_rule;
# use strider_pattern::*;
# let (a, b, c) = (Capture::new(), Capture::new(), Capture::new());
rewrite_rule(int_add(var(a), var(b)), anything());
```

```rust,compile_fail,E0277
# use strider_orchestrator::opt::rewrite_rule;
# use strider_pattern::*;
# let (a, b, c) = (Capture::new(), Capture::new(), Capture::new());
rewrite_rule(int_add(var(a), var(b)), any_int());
```

```rust,compile_fail,E0277
# use strider_orchestrator::opt::rewrite_rule;
# use strider_pattern::*;
# let (a, b, c) = (Capture::new(), Capture::new(), Capture::new());
rewrite_rule(int_add(var(a), var(b)), any_float());
```

```rust,compile_fail,E0277
# use strider_orchestrator::opt::rewrite_rule;
# use strider_pattern::*;
# let (a, b, c) = (Capture::new(), Capture::new(), Capture::new());
rewrite_rule(int_add(var(a), var(b)), any_bool());
```

```rust,compile_fail,E0277
# use strider_orchestrator::opt::rewrite_rule;
# use strider_pattern::*;
# let (a, b, c) = (Capture::new(), Capture::new(), Capture::new());
rewrite_rule(int_add(var(a), var(b)), any_int_const());
```

```rust,compile_fail,E0277
# use strider_orchestrator::opt::rewrite_rule;
# use strider_pattern::*;
# let (a, b, c) = (Capture::new(), Capture::new(), Capture::new());
rewrite_rule(int_add(var(a), var(b)), any_float_const());
```

```rust,compile_fail,E0277
# use strider_orchestrator::opt::rewrite_rule;
# use strider_pattern::*;
# let (a, b, c) = (Capture::new(), Capture::new(), Capture::new());
rewrite_rule(int_add(var(a), var(b)), any_bool_const());
```

```rust,compile_fail,E0277
# use strider_orchestrator::opt::rewrite_rule;
# use strider_pattern::*;
# let (a, b, c) = (Capture::new(), Capture::new(), Capture::new());
rewrite_rule(int_add(var(a), var(b)), int_const_all_ones());
```

```rust,compile_fail,E0277
# use strider_orchestrator::opt::rewrite_rule;
# use strider_pattern::*;
# let (a, b, c) = (Capture::new(), Capture::new(), Capture::new());
rewrite_rule(int_add(var(a), var(b)), int_const([1u8, 2]));
```

```rust,compile_fail,E0277
# use strider_orchestrator::opt::rewrite_rule;
# use strider_pattern::*;
# let (a, b, c) = (Capture::new(), Capture::new(), Capture::new());
rewrite_rule(int_add(var(a), var(b)), int_const_any_width([1i64, 2]));
```

```rust,compile_fail,E0277
# use strider_orchestrator::opt::rewrite_rule;
# use strider_pattern::*;
# let (a, b, c) = (Capture::new(), Capture::new(), Capture::new());
rewrite_rule(int_add(var(a), var(b)), bool_const(a));
```

```rust,compile_fail,E0277
# use strider_orchestrator::opt::rewrite_rule;
# use strider_pattern::*;
# let (a, b, c) = (Capture::new(), Capture::new(), Capture::new());
rewrite_rule(int_add(var(a), var(b)), float_const(a));
```

```rust,compile_fail,E0277
# use strider_orchestrator::opt::rewrite_rule;
# use strider_pattern::*;
# let (a, b, c) = (Capture::new(), Capture::new(), Capture::new());
rewrite_rule(int_add(var(a), var(b)), value_of_width(32));
```

```rust,compile_fail,E0277
# use strider_orchestrator::opt::rewrite_rule;
# use strider_pattern::*;
# let (a, b, c) = (Capture::new(), Capture::new(), Capture::new());
rewrite_rule(int_add(var(a), var(b)), inputs_of_width(32, var(a)));
```

```rust,compile_fail,E0277
# use strider_orchestrator::opt::rewrite_rule;
# use strider_pattern::*;
# let (a, b, c) = (Capture::new(), Capture::new(), Capture::new());
rewrite_rule(int_add(var(a), var(b)), bool_inputs(var(a)));
```

```rust,compile_fail,E0277
# use strider_orchestrator::opt::rewrite_rule;
# use strider_pattern::*;
# let (a, b, c) = (Capture::new(), Capture::new(), Capture::new());
rewrite_rule(int_add(var(a), var(b)), initial_var());
```

```rust,compile_fail,E0277
# use strider_orchestrator::opt::rewrite_rule;
# use strider_pattern::*;
# let (a, b, c) = (Capture::new(), Capture::new(), Capture::new());
rewrite_rule(int_add(var(a), var(b)), initial_var_for(strider_ir_test_utils::reg_vn(0, 8)));
```

```rust,compile_fail,E0277
# use strider_orchestrator::opt::rewrite_rule;
# use strider_pattern::*;
# let (a, b, c) = (Capture::new(), Capture::new(), Capture::new());
rewrite_rule(int_add(var(a), var(b)), any_int_binary(var(a), var(b)));
```

```rust,compile_fail,E0277
# use strider_orchestrator::opt::rewrite_rule;
# use strider_pattern::*;
# let (a, b, c) = (Capture::new(), Capture::new(), Capture::new());
rewrite_rule(int_add(var(a), var(b)), any_int_unary(var(a)));
```

```rust,compile_fail,E0277
# use strider_orchestrator::opt::rewrite_rule;
# use strider_pattern::*;
# let (a, b, c) = (Capture::new(), Capture::new(), Capture::new());
rewrite_rule(int_add(var(a), var(b)), any_int_cmp(var(a), var(b)));
```

```rust,compile_fail,E0277
# use strider_orchestrator::opt::rewrite_rule;
# use strider_pattern::*;
# let (a, b, c) = (Capture::new(), Capture::new(), Capture::new());
rewrite_rule(int_add(var(a), var(b)), any_float_binary(var(a), var(b)));
```

```rust,compile_fail,E0277
# use strider_orchestrator::opt::rewrite_rule;
# use strider_pattern::*;
# let (a, b, c) = (Capture::new(), Capture::new(), Capture::new());
rewrite_rule(int_add(var(a), var(b)), any_float_unary(var(a)));
```

```rust,compile_fail,E0277
# use strider_orchestrator::opt::rewrite_rule;
# use strider_pattern::*;
# let (a, b, c) = (Capture::new(), Capture::new(), Capture::new());
rewrite_rule(int_add(var(a), var(b)), any_float_cmp(var(a), var(b)));
```

```rust,compile_fail,E0277
# use strider_orchestrator::opt::rewrite_rule;
# use strider_pattern::*;
# let (a, b, c) = (Capture::new(), Capture::new(), Capture::new());
rewrite_rule(int_add(var(a), var(b)), any_bool_binary(var(a), var(b)));
```

```rust,compile_fail,E0277
# use strider_orchestrator::opt::rewrite_rule;
# use strider_pattern::*;
# let (a, b, c) = (Capture::new(), Capture::new(), Capture::new());
rewrite_rule(int_add(var(a), var(b)), float_le(var(a), var(b)));
```

```rust,compile_fail,E0277
# use strider_orchestrator::opt::rewrite_rule;
# use strider_pattern::*;
# let (a, b, c) = (Capture::new(), Capture::new(), Capture::new());
rewrite_rule(int_add(var(a), var(b)), float_is_nan(var(a)));
```

## Predicates and decorations

```rust,compile_fail,E0277
# use strider_orchestrator::opt::rewrite_rule;
# use strider_pattern::*;
# let (a, b, c) = (Capture::new(), Capture::new(), Capture::new());
rewrite_rule(int_add(var(a), var(b)), predicate(|_, _| true));
```

```rust,compile_fail,E0277
# use strider_orchestrator::opt::rewrite_rule;
# use strider_pattern::*;
# let (a, b, c) = (Capture::new(), Capture::new(), Capture::new());
rewrite_rule(int_add(var(a), var(b)), var(a).when_match(|_, _, _| true));
```

```rust,compile_fail,E0277
# use strider_orchestrator::opt::rewrite_rule;
# use strider_pattern::*;
# let (a, b, c) = (Capture::new(), Capture::new(), Capture::new());
rewrite_rule(int_add(var(a), var(b)), var(a).filter(|_, _| true));
```

```rust,compile_fail,E0277
# use strider_orchestrator::opt::rewrite_rule;
# use strider_pattern::*;
# let (a, b, c) = (Capture::new(), Capture::new(), Capture::new());
rewrite_rule(int_add(var(a), var(b)), template::int_add(var(a), var(b)).ordered());
```

```rust,compile_fail,E0277
# use strider_orchestrator::opt::rewrite_rule;
# use strider_pattern::*;
# let (a, b, c) = (Capture::new(), Capture::new(), Capture::new());
rewrite_rule(int_add(var(a), var(b)), var(a).of_width(32));
```

```rust,compile_fail,E0277
# use strider_orchestrator::opt::rewrite_rule;
# use strider_pattern::*;
# let (a, b, c) = (Capture::new(), Capture::new(), Capture::new());
rewrite_rule(int_add(var(a), var(b)), var(a).value_ty(strider_ir::node::ValueType::int(32)));
```

```rust,compile_fail,E0277
# use strider_orchestrator::opt::rewrite_rule;
# use strider_pattern::*;
# let (a, b, c) = (Capture::new(), Capture::new(), Capture::new());
rewrite_rule(int_add(var(a), var(b)), var(a).bool_valued());
```

## Alternations

```rust,compile_fail,E0277
# use strider_orchestrator::opt::rewrite_rule;
# use strider_pattern::*;
# let (a, b, c) = (Capture::new(), Capture::new(), Capture::new());
rewrite_rule(int_add(var(a), var(b)), one_of![var(a), var(b)]);
```

```rust,compile_fail,E0277
# use strider_orchestrator::opt::rewrite_rule;
# use strider_pattern::*;
# let (a, b, c) = (Capture::new(), Capture::new(), Capture::new());
rewrite_rule(int_add(var(a), var(b)), first_of![var(a), var(b)]);
```

```rust,compile_fail,E0277
# use strider_orchestrator::opt::rewrite_rule;
# use strider_pattern::*;
# let (a, b, c) = (Capture::new(), Capture::new(), Capture::new());
rewrite_rule(int_add(var(a), var(b)), boxed_alt(var(a)));
```

## A `.capture()` over anything but a `var()`

```rust,compile_fail,E0277
# use strider_orchestrator::opt::rewrite_rule;
# use strider_pattern::*;
# let (a, b, c) = (Capture::new(), Capture::new(), Capture::new());
rewrite_rule(int_add(var(a), var(b)), template::int_add(var(a), var(b)).capture(c));
```

```rust,compile_fail,E0277
# use strider_orchestrator::opt::rewrite_rule;
# use strider_pattern::*;
# let (a, b, c) = (Capture::new(), Capture::new(), Capture::new());
rewrite_rule(int_add(var(a), var(b)), var(a).capture(b).capture(c));
```

```rust,compile_fail,E0277
# use strider_orchestrator::opt::rewrite_rule;
# use strider_pattern::*;
# let (a, b, c) = (Capture::new(), Capture::new(), Capture::new());
rewrite_rule(int_add(var(a), var(b)), anything().capture(c));
```

## Node builders, none of which has a template form

```rust,compile_fail,E0277
# use strider_orchestrator::opt::rewrite_rule;
# use strider_pattern::*;
# let (a, b, c) = (Capture::new(), Capture::new(), Capture::new());
rewrite_rule(int_add(var(a), var(b)), call());
```

```rust,compile_fail,E0277
# use strider_orchestrator::opt::rewrite_rule;
# use strider_pattern::*;
# let (a, b, c) = (Capture::new(), Capture::new(), Capture::new());
rewrite_rule(int_add(var(a), var(b)), call_other());
```

```rust,compile_fail,E0277
# use strider_orchestrator::opt::rewrite_rule;
# use strider_pattern::*;
# let (a, b, c) = (Capture::new(), Capture::new(), Capture::new());
rewrite_rule(int_add(var(a), var(b)), ret());
```

```rust,compile_fail,E0277
# use strider_orchestrator::opt::rewrite_rule;
# use strider_pattern::*;
# let (a, b, c) = (Capture::new(), Capture::new(), Capture::new());
rewrite_rule(int_add(var(a), var(b)), indirect_branch());
```

```rust,compile_fail,E0277
# use strider_orchestrator::opt::rewrite_rule;
# use strider_pattern::*;
# let (a, b, c) = (Capture::new(), Capture::new(), Capture::new());
rewrite_rule(int_add(var(a), var(b)), unreachable());
```

```rust,compile_fail,E0277
# use strider_orchestrator::opt::rewrite_rule;
# use strider_pattern::*;
# let (a, b, c) = (Capture::new(), Capture::new(), Capture::new());
rewrite_rule(int_add(var(a), var(b)), entry());
```

```rust,compile_fail,E0277
# use strider_orchestrator::opt::rewrite_rule;
# use strider_pattern::*;
# let (a, b, c) = (Capture::new(), Capture::new(), Capture::new());
rewrite_rule(int_add(var(a), var(b)), region());
```

```rust,compile_fail,E0277
# use strider_orchestrator::opt::rewrite_rule;
# use strider_pattern::*;
# let (a, b, c) = (Capture::new(), Capture::new(), Capture::new());
rewrite_rule(int_add(var(a), var(b)), switch());
```

```rust,compile_fail,E0277
# use strider_orchestrator::opt::rewrite_rule;
# use strider_pattern::*;
# let (a, b, c) = (Capture::new(), Capture::new(), Capture::new());
rewrite_rule(int_add(var(a), var(b)), if_else());
```

```rust,compile_fail,E0277
# use strider_orchestrator::opt::rewrite_rule;
# use strider_pattern::*;
# let (a, b, c) = (Capture::new(), Capture::new(), Capture::new());
rewrite_rule(int_add(var(a), var(b)), load());
```

```rust,compile_fail,E0277
# use strider_orchestrator::opt::rewrite_rule;
# use strider_pattern::*;
# let (a, b, c) = (Capture::new(), Capture::new(), Capture::new());
rewrite_rule(int_add(var(a), var(b)), store());
```

```rust,compile_fail,E0277
# use strider_orchestrator::opt::rewrite_rule;
# use strider_pattern::*;
# let (a, b, c) = (Capture::new(), Capture::new(), Capture::new());
rewrite_rule(int_add(var(a), var(b)), phi());
```

```rust,compile_fail,E0277
# use strider_orchestrator::opt::rewrite_rule;
# use strider_pattern::*;
# let (a, b, c) = (Capture::new(), Capture::new(), Capture::new());
rewrite_rule(int_add(var(a), var(b)), phi_for(strider_ir_test_utils::reg_vn(0, 8)));
```

```rust,compile_fail,E0277
# use strider_orchestrator::opt::rewrite_rule;
# use strider_pattern::*;
# let (a, b, c) = (Capture::new(), Capture::new(), Capture::new());
rewrite_rule(int_add(var(a), var(b)), mem_phi());
```

```rust,compile_fail,E0277
# use strider_orchestrator::opt::rewrite_rule;
# use strider_pattern::*;
# let (a, b, c) = (Capture::new(), Capture::new(), Capture::new());
rewrite_rule(int_add(var(a), var(b)), function_arg(0));
```

```rust,compile_fail,E0277
# use strider_orchestrator::opt::rewrite_rule;
# use strider_pattern::*;
# let (a, b, c) = (Capture::new(), Capture::new(), Capture::new());
rewrite_rule(int_add(var(a), var(b)), function_arg_float(0));
```

```rust,compile_fail,E0277
# use strider_orchestrator::opt::rewrite_rule;
# use strider_pattern::*;
# let (a, b, c) = (Capture::new(), Capture::new(), Capture::new());
rewrite_rule(int_add(var(a), var(b)), any_function_arg());
```

```rust,compile_fail,E0277
# use strider_orchestrator::opt::rewrite_rule;
# use strider_pattern::*;
# let (a, b, c) = (Capture::new(), Capture::new(), Capture::new());
rewrite_rule(int_add(var(a), var(b)), function_arg_reg(strider_ir_test_utils::reg_vn(0, 8), 0));
```

```rust,compile_fail,E0277
# use strider_orchestrator::opt::rewrite_rule;
# use strider_pattern::*;
# let (a, b, c) = (Capture::new(), Capture::new(), Capture::new());
rewrite_rule(int_add(var(a), var(b)), function_arg_stack(rsleigh::VnSpace::RAM, 8, 0));
```

## A value op over a match-only operand, nested two deep

```rust,compile_fail,E0277
# use strider_orchestrator::opt::rewrite_rule;
# use strider_pattern::*;
# let (a, b, c) = (Capture::new(), Capture::new(), Capture::new());
rewrite_rule(int_add(var(a), var(b)), template::int_add(var(a), template::int_neg(anything())));
```

```rust,compile_fail,E0277
# use strider_orchestrator::opt::rewrite_rule;
# use strider_pattern::*;
# let (a, b, c) = (Capture::new(), Capture::new(), Capture::new());
rewrite_rule(int_add(var(a), var(b)), template::int_xor(template::int_and(var(a), any_int_const()), var(b)));
```

```rust,compile_fail,E0277
# use strider_orchestrator::opt::rewrite_rule;
# use strider_pattern::*;
# let (a, b, c) = (Capture::new(), Capture::new(), Capture::new());
rewrite_rule(int_add(var(a), var(b)), int_add(var(a), int_mul(var(b), load())));
```

## A sealed `Pattern`

```rust,compile_fail,E0277
# use strider_orchestrator::opt::rewrite_rule;
# use strider_pattern::*;
# let (a, b, c) = (Capture::new(), Capture::new(), Capture::new());
rewrite_rule(int_add(var(a), var(b)), int_add(var(a), var(b)).into_pattern());
```

```rust,compile_fail,E0277
# use strider_orchestrator::opt::rewrite_rule;
# use strider_pattern::*;
# let (a, b, c) = (Capture::new(), Capture::new(), Capture::new());
rewrite_rule(int_add(var(a), var(b)), load().build());
```

```rust,compile_fail,E0277
# use strider_orchestrator::opt::rewrite_rule;
# use strider_pattern::*;
# let (a, b, c) = (Capture::new(), Capture::new(), Capture::new());
rewrite_rule(int_add(var(a), var(b)), int_add(var(a), var(b)).into_pattern().ignore_casts());
```

```rust,compile_fail,E0277
# use strider_orchestrator::opt::rewrite_rule;
# use strider_pattern::*;
# let (a, b, c) = (Capture::new(), Capture::new(), Capture::new());
rewrite_rule(int_add(var(a), var(b)), int_add(var(a), var(b)).into_pattern().ignore_casts_mask(strider_pattern::CastMask::all()));
```

## The hidden `Expr`

The front-end tree is not a template either, whatever it holds:

```rust,compile_fail,E0277
# use strider_orchestrator::opt::rewrite_rule;
# use strider_pattern::*;
# let (a, b, c) = (Capture::new(), Capture::new(), Capture::new());
rewrite_rule(int_add(var(a), var(b)), anything().into_expr());
```

```rust,compile_fail,E0277
# use strider_orchestrator::opt::rewrite_rule;
# use strider_pattern::*;
# let (a, b, c) = (Capture::new(), Capture::new(), Capture::new());
rewrite_rule(int_add(var(a), var(b)), template::int_add(var(a), anything().into_expr()));
```

## A wrapper of one's own

No type outside strider-pattern is a template, so a wrapper around the
hidden `Expr` cannot reach `rewrite_rule` either:

```rust,compile_fail,E0277
# use strider_orchestrator::opt::rewrite_rule;
# use strider_pattern::*;
struct Wrapper(Capture);
impl IntoExpr for Wrapper {
    fn into_expr(self) -> Expr {
        anything().capture(self.0).into_expr()
    }
}
impl TemplatePat for Wrapper {}
# let (a, b) = (Capture::new(), Capture::new());
rewrite_rule(int_add(var(a), var(b)), Wrapper(a));
```
