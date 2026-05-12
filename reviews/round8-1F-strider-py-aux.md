# Round 8 / 1F — `strider-py` + `dot` + `graphwalk` + `entity-utils` audit

**Branch:** `review/ai2`.  Independent audit; round-7 reviews not consulted.

## Coverage

All `crates/strider-py/src/**/*.rs` (14 files), `crates/strider-py/tests/python/**/*.py`, `crates/strider-py/strider/**/*.py`, `crates/strider-py/Cargo.toml` + `pyproject.toml` + README, all of `crates/dot/src/**/*.rs` + tests, all of `crates/graphwalk/src/**/*.rs` + tests, all of `crates/entity-utils/src/**/*.rs`.

## Findings

### CRITICAL: `PyVnSpace::__hash__` hashes the stack address of `self.inner`, violating Python's hash/equality contract

- **Severity:** HIGH (silent wrong-result on every `dict[VnSpace]` / `set[VnSpace]` lookup).
- **Where:** `crates/strider-py/src/sleigh.rs:144-148`.
- **What's wrong:**
  ```rust
  fn __hash__(&self) -> u64 {
      let p: *const () = (&self.inner) as *const _ as *const ();
      p as usize as u64
  }
  ```
  `__eq__` compares by inner-pointer / value, but `__hash__` hashes the **address of the field** within this particular `PyVnSpace` heap allocation.  Two separate Python objects wrapping `VnSpace::RAM` (created by two `VnSpace.ram()` calls) live at different heap addresses → different hashes.
  Python contract: `a == b ⇒ hash(a) == hash(b)`.  Violated whenever `VnSpace.ram() == VnSpace.ram()` holds (it does) but the two objects hash differently.  `d = {VnSpace.ram(): 1}; d[VnSpace.ram()]` raises `KeyError`.
- **Fix:** Hash the stable inner identity (the rsleigh `VnSpace` underlying pointer/id), not the Rust field's heap address.  Match on known variants if `rsleigh::VnSpace` exposes no integer id.

### MED: `PyMatch.__getitem__` and `PyPartialMatch.__getitem__` silently truncate `u128` constants via `as i128`

- **Severity:** MED (silent wrong value for any U128 const with bit 127 set).
- **Where:** `crates/strider-py/src/matcher.rs:55`; `crates/strider-py/src/pattern.rs:364`.
- **What's wrong:** `get_uint` returns `Option<u128>`; both `__getitem__` shortcuts do `(v as i128).into_py(py)` — a wrapping reinterpret, not a range check.  Any U128 value with bit 127 set becomes a negative Python int.  E.g. `0xFFFFFFFF_FFFFFFFF_FFFFFFFF_FFFFFFFF` returns `-1`.

  The full `Match.uint(c)` accessor (separate method) returns `u128` correctly.  The `__getitem__` shortcut differs silently — `m["cap"] != m.uint("cap")` for big values.
- **Fix:**
  ```rust
  return Ok(v.into_py(py));  // PyO3 converts u128 directly
  ```

### MED: `Vn.__hash__` omits `addr_space` — equal-offset/size varnodes in different spaces collide

- **Severity:** MED (perf only; doesn't violate contract because equal Vns always have same space).
- **Where:** `crates/strider-py/src/sleigh.rs:211-214`.
- **What's wrong:** `__hash__` mixes `addr_off` and `size` but not `addr_space`.  RAM[0x10]:8 and REGISTER[0x10]:8 hash identically but compare unequal — bucket chains O(n).
- **Fix:** Mix in a stable space discriminant (or hash the inner `Vn` directly via `Hash` derive on `rsleigh::Vn`).

### MED: `test_public_api_snapshot.py` `EXPECTED_PATTERN` is incomplete — many registered names not pinned

- **Severity:** MED (regression-detection gap; no current bug).
- **Where:** `crates/strider-py/tests/python/test_public_api_snapshot.py:73-118`.
- **What's wrong:** Snapshot test only fails when expected names disappear; doesn't check for unexpected extras AND missing names from the public API.  Many registered functions / classes are absent from `EXPECTED_PATTERN`:
  - Functions: `bit_not`, `xor`, `phi_for`, `mem_phi`, `value_phi`, `int_cmp`, `initial_var_for`, `function_arg_reg`, `function_arg_stack`, `signed_int_const`.
  - Classes: `CastMask`, `PhiPat`, `MemPhiPat`, `ValuePhiPat`, `CallOtherPat`, `FunctionArgPat`, `LoadPat`, `StorePat`, `StackStorePat`, `StackStorePhiPat`, `IfPat`, `RetPat`.
- **Fix:** Extend `EXPECTED_PATTERN` to include the full set of intentionally-public names, and add a positive check that the actual `dir()` set matches the expected set both ways.

## Areas verified correct (informational)

- **GIL handling**: `run_via_orchestrator` releases GIL via `py.allow_threads`; `PyMemReaderAdapter::read` re-acquires per call via `Python::with_gil`.  No deadlock potential (PyO3 handles re-entrant acquisition).
- **`PyPartialMatch` `*const ir::Graph` safety**: pointer set just before predicate, cleared immediately after via `clear_graph_ptr`.  `Mutex<Option<*const _>>` guards re-entrant access.  `unsendable` blocks thread crossing.  Sound.
- **`unsafe { std::env::set_var(...) }` in `lib.rs`**: guarded by env-var precondition, called from `#[pymodule]` init under Python's import lock.
- **`PyMemoryMap::read`**: round-7 endianness fix is correct; `target::Endianness::read_u64` used post round-8.
- **`PyOptimizerPipeline` pass-count pinning**: 6/4/2 matches Rust counterparts.
- **`DenseEntitySet::insert` `bool` return**: callers in `worklist.rs` use `contains` before insert, so return value isn't consumed; not stale.
- **`graphwalk::PreOrder` cycle handling**: visited tracker; correct.
- **`dot::json_quote`**: escapes `"`, `\`, control chars, and `<` → `<` to prevent script-injection.
- **`dot::escape_dot_label`**: pass-through DOT escapes; doubles bare backslashes.
- **Str-keyed capture interning**: `OnceLock<Mutex<HashMap<String, Capture>>>` — thread-safe, reserved-name guard correct.
- **Typed exception hierarchy**: all 6 inherit `StriderError`; `register()` adds them to `sys.modules["strider.errors"]`.
- **`at_any(addrs: Vec<u64>)`**: `u64: Into<i128>` holds — correct.
- **`reoptimize`**: stable + optionally destructive; correct post-orchestrator.

## Summary

- **1 HIGH** — `PyVnSpace::__hash__` hashes Rust field address.
- **3 MED** — `__getitem__` u128→i128 truncation; `Vn.__hash__` missing `addr_space`; `EXPECTED_PATTERN` snapshot incomplete.
