---
name: strider-cli-runner
description: Use the strider example binary for visual debugging — lift a fixture ELF, dump cfg.html / graph.html / graph-opt.html, and triage IR shape before invoking pattern or opt skills.
---

# strider-cli-runner

## When to invoke

User has a binary in hand (a fixture or a user-supplied ELF) and wants visual debugging without writing Rust. Triggers include:

- "Run the strider CLI on `<binary>`."
- "Dump `cfg.html` / `graph.html` / `graph-opt.html` for `<entry>`."
- "Lift this binary and show me the IR."
- "Visualise the IR for the function at `0x<addr>`."
- Triage step before `strider-debug-pattern`, `strider-flagcmp-rule-author`, or any opt-pass debugging.
- Bug-report reproducer requested.

## When NOT to invoke

- User wants to write a new pattern → `strider-pattern-author` directly.
- User wants to add a new feature, not investigate one → development skill.
- User has a unit-test failure with a graphmock-built graph — no real ELF needed; debug the test directly.

## Files this skill operates on

- `crates/strider/examples/strider.rs` — the example binary. Edit only when adding flags or pointing it at a different fixture.
- `fixtures/out/<arch>/<binary>.elf` — input binary. Pre-built via `make -C fixtures`.
- Output (created in workspace root):
  - `cfg.html` / `cfg.dot` — basic-block CFG.
  - `graph.html` / `graph.dot` — unoptimised IR (post-lift, pre-opt).
  - `graph-opt.html` / `graph-opt.dot` — IR after the full optimizer pipeline.

## Procedure

1. **Build the fixture if not present.** Round-8 fix renamed the example's default input from `test.elf` to `arithmetic.elf`. The example reads:

   ```rust
   let binary_path = "fixtures/out/x86/arithmetic.elf";
   let symbol = "add";
   ```

   from `crates/strider/examples/strider.rs:14-15`. If `fixtures/out/x86/arithmetic.elf` doesn't exist:

   ```bash
   make -C fixtures ARCH=x86 CASE=arithmetic
   ```

   For a different arch / case, build via `make -C fixtures ARCH=<arch> CASE=<case>`. The full matrix is built via `make -C fixtures` (slow — only do this once).

2. **Run the example.** From the workspace root:

   ```bash
   cargo run -p strider --example strider
   ```

   The example writes `cfg.html`, `cfg.dot`, `graph.html`, `graph.dot`, `graph-opt.html`, `graph-opt.dot` to the workspace root and prints progress to stdout. It uses `Builder::new(...)` (defaults to LE + x86_64); for non-x86 fixtures, edit the `arch` and `CallingConvention` lines around `crates/strider/examples/strider.rs:21-27`.

3. **For a non-x86 fixture, edit the example.** The relevant lines are:

   ```rust
   let arch = strider::SleighArch::x86();      // change to ::aarch64() etc.
   let strider = strider::Strider::new(
       arch,
       sleigh.regs()?,
       strider::CallingConvention::x86_cdecl(), // change to ::aarch64_aapcs64() etc.
   )?;
   ```

   And the binary path at `crates/strider/examples/strider.rs:14`. Don't commit this — the example is meant to be a transient debugging surface; revert before any commit.

4. **Open the three HTMLs in a browser.** They are self-contained (the dot crate inlines the graphviz output as SVG). Recommended order:
   - `cfg.html` — basic-block layout. Confirms the CFG has the expected edges (Branch / Fallthrough / IfCaseTrue / IfCaseFalse).
   - `graph.html` — unoptimised IR. Surface-level lift correctness: are the `InitialVar` / `ControlState` / `VarPhi` nodes wired up sensibly?
   - `graph-opt.html` — post-pipeline IR. The optimised shape patterns will see. If a pattern fails to match, this is the dump to inspect.

5. **For Python-side reproduction:**

   ```bash
   uv run python -c "
   import strider
   from strider import SleighArch, CallingConvention, MemoryMap

   arch = SleighArch.x86()
   cc   = CallingConvention.x86_cdecl()
   mem  = MemoryMap.from_elf('fixtures/out/x86/arithmetic.elf')
   bfg  = strider.run(arch, cc, mem, entry=mem.symbol('add'))
   print(len(list(bfg.regions())))
   "
   ```

   The Python binding's `strider.run(...)` is the equivalent of the Rust example's pipeline.

6. **Asm-fingerprint readout for any node of interest.** In Rust:

   ```rust
   let fp: &[u64] = graph.asm_fingerprint(node_id);
   println!("node {node_id:?} fingerprint: {:#x?}", fp);
   ```

   In Python (after a pattern match):

   ```python
   addrs = match.asm_fingerprint(c)  # list[int]
   print(f"capture c attributable to: {[hex(a) for a in addrs]}")
   ```

   Cross-reference against `objdump -d fixtures/out/<arch>/<case>.elf` to locate the contributing source instructions. This is the round-7 attribution-aid contract: every reachable non-exempt node carries the union of contributing-asm-instruction addresses.

7. **If `validate` fails, the example surfaces it via `anyhow::Error`.** The error string includes the failing layer (A / B / C) and the offending `NodeId`. To get the opt-in Layer-C `check_asm_fingerprints` check, edit the example to call `validate_with_options(&graph.graph, graph.entry, ValidateOptions { check_asm_fingerprints: true })` directly after the pipeline.

8. **Hand off.** If the dump shows:
   - A pattern returning zero matches → `strider-debug-pattern`.
   - A flag tree surviving past `ConstantFold` → `strider-flagcmp-rule-author`.
   - An indirect-branch placeholder unresolved → `strider-indirect-shape-author`.
   - A Layer-C validate failure → `strider-validation-invariant-extend` or `strider-fingerprint-audit`.
   - A wrong-arch CallOther dispatch → `strider-orchestrator-extend` (the `Builder::for_arch` foot-gun).

## Verification

- All three HTMLs (`cfg.html`, `graph.html`, `graph-opt.html`) generated in the workspace root.
- Visual inspection matches user's expectation of the source binary.
- `cargo run -p strider --example strider` exits with status 0.

## Exit criteria

- User has a reproducible CLI invocation captured in the conversation.
- IR dump (the three HTMLs) exists for the target function.
- The example's `binary_path` / `symbol` / `arch` / `cc` are recorded so the user can re-run.
- Hand-off to a debugging skill is identified if the dump shows a problem.

## Pitfalls

- **Editing `crates/strider/examples/strider.rs` and committing the change.** The example is a transient debugging surface. Default state points at `fixtures/out/x86/arithmetic.elf::add` post-round-8 (was `test.elf` pre-round-8 — that file was a leftover and never auto-built). Revert any per-investigation edit before commit.
- **Forgetting to build the fixture.** `cargo run -p strider --example strider` does NOT trigger fixture builds. If `fixtures/out/x86/arithmetic.elf` is missing, the example fails at `reader::load_elf` with a file-not-found error.
- **Running with the wrong arch/CC.** The example uses `SleighArch::x86()` + `x86_cdecl` by default. Lifting an AArch64 ELF without changing both lines silently produces a garbage CFG (Sleigh decodes ARM bytes as x86 prefixes). The CFG dump will look obviously wrong.
- **Confusing `graph.html` with `graph-opt.html`.** Pattern queries run against post-opt IR by default. Looking at `graph.html` to debug a missed pattern match wastes time — `graph-opt.html` is the relevant view.
- **Browser caching.** HTMLs use the same names across runs. Force-refresh (Ctrl-F5) or the browser shows stale SVG.
- **Skipping the asm-fingerprint readout.** It's the fastest way to confirm a captured node is attributable to the expected machine instruction. Use it before assuming the lift is wrong.

## Related skills

- `strider-debug-pattern` — when the dump reveals a pattern is wrong.
- `strider-flagcmp-rule-author` — when a flag tree survives optimisation.
- `strider-indirect-shape-author` — when an indirect-branch placeholder remains unresolved post-opt.
- `strider-validation-invariant-extend` / `strider-fingerprint-audit` — when validate fails or a node has an empty fingerprint.
- `strider-orchestrator-extend` — when the dump shows an `UnknownCallOtherError` on a non-x86 arch.
