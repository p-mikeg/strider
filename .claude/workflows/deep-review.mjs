export const meta = {
  name: 'deep-review',
  description: 'Line-by-line multi-dimension review of strider production source, adversarially verified',
  phases: [
    { title: 'Review', detail: 'dimension-specialized agents read assigned files in full' },
    { title: 'Verify', detail: 'independent skeptic tries to refute each finding' },
    { title: 'Synthesize', detail: 'dedup, rank, write report' },
  ],
}

const ROOT = '/mnt/c/Users/mikeg/Documents/strider'

// ---- Dimension instruction blocks -------------------------------------------
const DIM = {
  asm: `SOUNDNESS — graph correctness vs assembly/p-code semantics. Verify the lifted IR / CFG / target description faithfully represents machine semantics. Hunt for: wrong p-code opcode handling, wrong signedness, wrong bit-width / truncation / sign-vs-zero extension, register-aliasing read/write mask & shift errors (sub-register slices), flag-bit computation errors, endianness (raw bytes vs integer decode), branch-delay / context-register (Thumb/x86-seg/MIPS16) handling, call/return calling-convention arity, stack-arg offset arithmetic, varnode SPACE confusion (REGISTER/UNIQUE/CONST/RAM), missing implicit reads/writes/memory-clobber. Cite the exact machine semantics the code violates and give a concrete instruction example.`,
  opt: `SOUNDNESS — optimization correctness in ALL edge cases. For each rewrite / fold / classification verify semantics are preserved for: integer overflow/wraparound at the DECLARED width, signed vs unsigned ops, shift count >= width, div/mod by zero, NaN / +-inf / signed-zero floats, the 1-bit boolean I1 width, wide consts (I80/I128/I256/I512), memory-SSA clobber soundness (MemPhi merge, Call clobber, aliasing, partial overlap), commutativity assumptions, control-edge removal & phi arity after region/branch collapse, fingerprint (proof) superset preservation. For every suspected bug construct a CONCRETE counterexample (input shape -> wrong output).`,
  self: `SOUNDNESS — code correctness against itself / its own contract. Find: off-by-one, wrong index, swapped args, inverted condition, unhandled enum variant / match arm, incorrect Option/Result handling, side-table remap gaps in compact()/retain_reachable(), stale NodeId/ValueId use after replace_all_uses/cull/kill, iterator invalidation while mutating, panics/unwrap on REACHABLE inputs (not validator-guaranteed invariants), missing asm-fingerprint propagation on a rewrite. Quote the contradicting lines.`,
  simplify: `SIMPLIFICATION / GENERALIZATION / HELPERS. Find duplicated or near-duplicated logic (macro: whole blocks; micro: repeated small idioms) and argument lists that should be replaced by a richer object. SPECIFICALLY flag any site threading (class,size,space) or (kind,width,...) separately when a NodeId/ValueId already determines them in O(1) via cached accessors — propose a helper taking the id. Scrutinize memory ops (load/store, sp_expr decompose, memory_ssa walk) for repeated decompose/lookup/alias logic that could unify. Give concrete helper signatures. Only propose behavior-preserving changes; note any that subtly change behavior.`,
  runtime: `RUNTIME COMPLEXITY. Verify every path is O(n)/O(n log n) in nodes/edges, never O(n^2). Flag: nested loops over all nodes, a full graph walk inside a per-node loop, linear scans (Vec::contains / .iter().find / .position) that should be SecondaryMap/FxHashMap lookups, re-walking from entry per query instead of caching reverse_postorder/postorder, sorting in a hot loop, per-node heap allocation in a hot path, recomputation that should be memoized. Confirm worklist/RPO/postorder usage is correct & not redundantly recomputed. State the loop nesting and what n is.`,
  clean: `CLEANLINESS / READABILITY. Identify: over-long functions that should decompose, deep nesting that early-return/let-else would flatten, unclear names, dead code, commented-out code, copy-paste drift, idiom inconsistent with surrounding Rust + clippy/workspace lints. Propose specific behavior-preserving refactors.`,
  filesplit: `FILE-SPLIT / MODULE STRUCTURE. The memory_ssa/ and sp_expr/ modules are reportedly messy and the sp_expr file split is weird. Map every public + private item across these files, group by responsibility (SP-offset decomposition; memory-SSA def/clobber walk; alias oracle; range arithmetic), and propose the correct file/module layout: which code belongs where, what should merge, what should split, and which logic across the two modules is actually the same and should unify. Produce a concrete target tree (file -> items) and the rationale. This is a DESIGN finding, not a bug.`,
}

// ---- Review units: every production source file is assigned ------------------
// dims: lenses applied by this unit's agent. files: paths relative to ROOT.
const UNITS = [
  // ---- memory subsystem (densest coverage) ----
  { id: 'MEM-sound', dims: ['opt', 'self'], files: ['crates/strider-opt/src/memory_ssa/mod.rs', 'crates/strider-opt/src/sp_expr/decompose.rs', 'crates/strider-opt/src/sp_expr/walk.rs', 'crates/strider-opt/src/sp_expr/ranges.rs', 'crates/strider-opt/src/sp_expr/mod.rs'] },
  { id: 'MEM-simplify', dims: ['simplify', 'runtime'], files: ['crates/strider-opt/src/memory_ssa/mod.rs', 'crates/strider-opt/src/sp_expr/decompose.rs', 'crates/strider-opt/src/sp_expr/walk.rs', 'crates/strider-opt/src/sp_expr/ranges.rs', 'crates/strider-opt/src/sp_expr/mod.rs'] },
  { id: 'MEM-filesplit', dims: ['filesplit'], files: ['crates/strider-opt/src/memory_ssa/mod.rs', 'crates/strider-opt/src/sp_expr/decompose.rs', 'crates/strider-opt/src/sp_expr/walk.rs', 'crates/strider-opt/src/sp_expr/ranges.rs', 'crates/strider-opt/src/sp_expr/mod.rs'] },

  // ---- strider-opt passes ----
  { id: 'CONSTFOLD', dims: ['opt', 'self', 'simplify'], files: ['crates/strider-opt/src/constant_fold/mod.rs', 'crates/strider-opt/src/constant_fold/rules.rs', 'crates/strider-opt/src/constant_fold/eval_int.rs', 'crates/strider-opt/src/constant_fold/eval_float.rs'] },
  { id: 'KNOWNBITS', dims: ['opt', 'runtime'], files: ['crates/strider-opt/src/known_bits/mod.rs'] },
  { id: 'REWRITE', dims: ['opt', 'self'], files: ['crates/strider-opt/src/rewrite_rule.rs'] },
  { id: 'PEEPHOLE', dims: ['self', 'simplify'], files: ['crates/strider-opt/src/peephole.rs'] },
  { id: 'LOADFWD', dims: ['opt', 'self'], files: ['crates/strider-opt/src/load_forward/mod.rs', 'crates/strider-opt/src/load_readonly/mod.rs', 'crates/strider-opt/src/stack_offset_detect/mod.rs'] },
  { id: 'INDIRECT', dims: ['opt', 'runtime', 'self'], files: ['crates/strider-opt/src/indirect_branch_resolve/mod.rs', 'crates/strider-opt/src/indirect_branch_resolve/table.rs'] },
  { id: 'VALRANGE', dims: ['opt', 'runtime', 'self'], files: ['crates/strider-opt/src/value_range/mod.rs'] },
  { id: 'ARGS', dims: ['opt', 'self'], files: ['crates/strider-opt/src/function_args/mod.rs', 'crates/strider-opt/src/call_stack_args/mod.rs'] },
  { id: 'FLAGCMP', dims: ['opt', 'self'], files: ['crates/strider-opt/src/flag_cmp_canonicalize/mod.rs', 'crates/strider-opt/src/if_cond_inversion/mod.rs'] },
  { id: 'PIPELINE', dims: ['self', 'simplify', 'runtime'], files: ['crates/strider-opt/src/pipeline.rs', 'crates/strider-opt/src/lib.rs', 'crates/strider-opt/src/options.rs'] },
  { id: 'COLLAPSE', dims: ['opt', 'self'], files: ['crates/strider-opt/src/phi_collapse/mod.rs', 'crates/strider-opt/src/region_collapse/mod.rs', 'crates/strider-opt/src/dead_branch/mod.rs', 'crates/strider-opt/src/cfg_detach/mod.rs'] },

  // ---- strider-ir ----
  { id: 'IRDATA', dims: ['simplify', 'self', 'runtime'], files: ['crates/strider-ir/src/function/data.rs'] },
  { id: 'IREDIT', dims: ['self', 'runtime', 'simplify'], files: ['crates/strider-ir/src/function/edit.rs', 'crates/strider-ir/src/function/state.rs'] },
  { id: 'IRWALK', dims: ['runtime', 'self'], files: ['crates/strider-ir/src/walk/mod.rs', 'crates/strider-ir/src/walk/cast/mod.rs'] },
  { id: 'VNIO', dims: ['asm', 'self'], files: ['crates/strider-ir/src/builder/vn_io.rs', 'crates/strider-ir/src/builder/vars.rs'] },
  { id: 'IRBUILD', dims: ['simplify', 'self'], files: ['crates/strider-ir/src/builder/builder_ext.rs', 'crates/strider-ir/src/builder/nodes.rs', 'crates/strider-ir/src/builder/build_trait.rs', 'crates/strider-ir/src/builder/mod.rs'] },
  { id: 'IRCALL', dims: ['asm', 'self'], files: ['crates/strider-ir/src/builder/call.rs'] },
  { id: 'IRVIEWER', dims: ['simplify', 'self'], files: ['crates/strider-ir/src/viewer.rs'] },
  { id: 'IRNODE', dims: ['self', 'simplify'], files: ['crates/strider-ir/src/node/kind.rs', 'crates/strider-ir/src/node/value_type.rs', 'crates/strider-ir/src/node/ops.rs', 'crates/strider-ir/src/node/value_kind.rs', 'crates/strider-ir/src/node_signature.rs'] },
  { id: 'IRVALIDATE', dims: ['self', 'runtime'], files: ['crates/strider-ir/src/validate/mod.rs', 'crates/strider-ir/src/validate/graph_invariants.rs', 'crates/strider-ir/src/validate/local_typing.rs', 'crates/strider-ir/src/validate/use_list_consistency.rs'] },
  { id: 'IRCFV', dims: ['self', 'runtime'], files: ['crates/strider-ir/src/control_flow_view.rs', 'crates/strider-ir/src/region.rs'] },
  { id: 'IRWIDE', dims: ['self', 'simplify'], files: ['crates/strider-ir/src/wide_const.rs', 'crates/strider-ir/src/graph/mod.rs', 'crates/strider-ir/src/graph/cache.rs'] },
  { id: 'IRDOT', dims: ['clean', 'simplify'], files: ['crates/strider-ir/src/function/dot/mod.rs', 'crates/strider-ir/src/function/dot/label.rs', 'crates/strider-ir/src/function/dot/render.rs', 'crates/strider-ir/src/function/dot/raw.rs'] },

  // ---- strider-graph ----
  { id: 'GRAPHCORE', dims: ['simplify', 'self', 'runtime'], files: ['crates/strider-graph/src/graph.rs', 'crates/strider-graph/src/storage.rs', 'crates/strider-graph/src/node_cache.rs', 'crates/strider-graph/src/cache.rs'] },
  { id: 'GRAPHMISC', dims: ['self', 'runtime'], files: ['crates/strider-graph/src/iter.rs', 'crates/strider-graph/src/petgraph_view.rs', 'crates/strider-graph/src/ids.rs'] },

  // ---- strider-lift ----
  { id: 'LIFTCTL', dims: ['asm', 'self'], files: ['crates/strider-lift/src/lift/control.rs', 'crates/strider-lift/src/lift/dispatch.rs'] },
  { id: 'LIFTARITH', dims: ['asm', 'self'], files: ['crates/strider-lift/src/lift/arithmetic.rs', 'crates/strider-lift/src/lift/integer.rs', 'crates/strider-lift/src/lift/boolean.rs'] },
  { id: 'LIFTCAST', dims: ['asm', 'self'], files: ['crates/strider-lift/src/lift/cast.rs', 'crates/strider-lift/src/lift/float.rs'] },
  { id: 'LIFTMISC', dims: ['asm', 'self'], files: ['crates/strider-lift/src/lift/call.rs', 'crates/strider-lift/src/lift/memory.rs', 'crates/strider-lift/src/lift/misc.rs', 'crates/strider-lift/src/lift/vn_io.rs', 'crates/strider-lift/src/lift/pcode_util.rs', 'crates/strider-lift/src/lift/mod.rs', 'crates/strider-lift/src/lift/function_lifter.rs'] },

  // ---- strider-cfg ----
  { id: 'CFGREGION', dims: ['asm', 'self', 'runtime'], files: ['crates/strider-cfg/src/builder/region_builder.rs'] },
  { id: 'CFGBUILD', dims: ['asm', 'self'], files: ['crates/strider-cfg/src/builder/mod.rs', 'crates/strider-cfg/src/builder/split.rs', 'crates/strider-cfg/src/indirect_resolver.rs'] },
  { id: 'CFGQUERY', dims: ['self', 'runtime'], files: ['crates/strider-cfg/src/query.rs', 'crates/strider-cfg/src/types.rs', 'crates/strider-cfg/src/options.rs'] },

  // ---- strider-pattern ----
  { id: 'PATTYPED', dims: ['simplify', 'self'], files: ['crates/strider-pattern/src/typed/value_ops.rs', 'crates/strider-pattern/src/typed/consts.rs', 'crates/strider-pattern/src/typed/wildcards.rs', 'crates/strider-pattern/src/typed/builder_like.rs', 'crates/strider-pattern/src/typed/mod.rs'] },
  { id: 'PATMATCH', dims: ['opt', 'runtime', 'self'], files: ['crates/strider-pattern/src/matcher/mod.rs', 'crates/strider-pattern/src/matcher/walk.rs', 'crates/strider-pattern/src/matcher/builder.rs', 'crates/strider-pattern/src/matcher/vertex.rs', 'crates/strider-pattern/src/matcher/graph.rs', 'crates/strider-pattern/src/matcher/match_pat.rs', 'crates/strider-pattern/src/matcher/cast_walk_through.rs'] },
  { id: 'PATTEMPL', dims: ['self', 'simplify'], files: ['crates/strider-pattern/src/template/mod.rs', 'crates/strider-pattern/src/template/builder.rs', 'crates/strider-pattern/src/template/graph.rs', 'crates/strider-pattern/src/template/template_pat.rs', 'crates/strider-pattern/src/template/ctx.rs', 'crates/strider-pattern/src/staging.rs'] },
  { id: 'PATBUILD', dims: ['simplify', 'self'], files: ['crates/strider-pattern/src/node_builders/flow.rs', 'crates/strider-pattern/src/node_builders/memory.rs', 'crates/strider-pattern/src/node_builders/node_pat.rs', 'crates/strider-pattern/src/node_builders/function_arg.rs', 'crates/strider-pattern/src/node_builders/phi.rs', 'crates/strider-pattern/src/node_builders/mod.rs', 'crates/strider-pattern/src/bindings.rs', 'crates/strider-pattern/src/macros_impl.rs', 'crates/strider-pattern/src/graph_ext.rs', 'crates/strider-pattern/src/match_result.rs', 'crates/strider-pattern/src/capture.rs'] },

  // ---- strider-target ----
  { id: 'TGTABI', dims: ['asm', 'self'], files: ['crates/strider-target/src/call_other_abi.rs'] },
  { id: 'TGTCC', dims: ['asm', 'self'], files: ['crates/strider-target/src/calling_convention/mod.rs', 'crates/strider-target/src/arch.rs', 'crates/strider-target/src/call_descriptor.rs'] },

  // ---- strider-reader ----
  { id: 'RDRRELOC', dims: ['self', 'runtime'], files: ['crates/strider-reader/src/elf/relocations.rs'] },
  { id: 'RDRREST', dims: ['self', 'simplify'], files: ['crates/strider-reader/src/lib.rs', 'crates/strider-reader/src/elf/sections.rs', 'crates/strider-reader/src/elf/load.rs', 'crates/strider-reader/src/elf/reader.rs', 'crates/strider-reader/src/elf/mod.rs'] },

  // ---- strider-py ----
  { id: 'PY-pattern', dims: ['simplify', 'self'], files: ['crates/strider-py/src/pattern.rs', 'crates/strider-py/src/matcher.rs', 'crates/strider-py/src/macros.rs'] },
  { id: 'PY-api', dims: ['self', 'simplify'], files: ['crates/strider-py/src/reader.rs', 'crates/strider-py/src/function.rs', 'crates/strider-py/src/opt.rs', 'crates/strider-py/src/run.rs', 'crates/strider-py/src/strider_cls.rs', 'crates/strider-py/src/node.rs'] },

  // ---- small generic + orchestrator ----
  { id: 'UTIL', dims: ['simplify', 'self', 'runtime'], files: ['crates/entity-utils/src/lib.rs', 'crates/graphwalk/src/lib.rs', 'crates/read-only-memory/src/lib.rs', 'crates/strider-orchestrator/src/lib.rs'] },
]

const FINDINGS_SCHEMA = {
  type: 'object',
  additionalProperties: false,
  required: ['findings'],
  properties: {
    findings: {
      type: 'array',
      items: {
        type: 'object',
        additionalProperties: false,
        required: ['dimension', 'severity', 'file', 'lines', 'title', 'problem', 'evidence', 'fix', 'confidence'],
        properties: {
          dimension: { type: 'string', enum: ['asm', 'opt', 'self', 'simplify', 'runtime', 'clean', 'filesplit'] },
          severity: { type: 'string', enum: ['high', 'medium', 'low'] },
          file: { type: 'string' },
          lines: { type: 'string' },
          title: { type: 'string' },
          problem: { type: 'string' },
          evidence: { type: 'string' },
          fix: { type: 'string' },
          confidence: { type: 'number' },
        },
      },
    },
  },
}

const VERDICT_SCHEMA = {
  type: 'object',
  additionalProperties: false,
  required: ['verdict', 'reasoning', 'corrected_severity'],
  properties: {
    verdict: { type: 'string', enum: ['confirmed', 'refuted', 'uncertain'] },
    reasoning: { type: 'string' },
    corrected_severity: { type: 'string', enum: ['high', 'medium', 'low'] },
  },
}

function reviewPrompt(unit) {
  const lenses = unit.dims.map((d) => `### Lens: ${d}\n${DIM[d]}`).join('\n\n')
  const fileList = unit.files.map((f) => `- ${ROOT}/${f}`).join('\n')
  return `You are reviewing a slice of the strider Rust binary-analysis codebase (sea-of-nodes IR, p-code lifting, optimizer). This is unit ${unit.id}.

Read EACH of these files IN FULL with the Read tool (no line limit; re-Read in chunks if long). Do NOT rely on grep, on file/function names, or on code comments — comments, CLAUDE.md, and any prior-review notes are UNVERIFIED claims you must check against the actual code:
${fileList}

Review every line under the following lenses ONLY. Report a finding only when you can point at specific lines and explain the concrete defect or improvement; no vague "could be cleaner" without a concrete proposal.

${lenses}

Domain facts you may rely on: booleans are the 1-bit integer I1; there is no Sub (it's Add(_,Neg)); IntConst payload is Small(u64) or Wide(WideConstId); NodeId/ValueId index cranelift-entity maps; SecondaryMap/FxHashMap are the keyed-collection types; reverse_postorder/postorder/Worklist are the traversal primitives; ReadOnlyMemory::read copies raw bytes (no endianness swap). Verify any such assumption against the code before relying on it.

For each finding set confidence in [0,1] = your prior that this is a TRUE, actionable defect (not a false positive). Be precise with file (repo-relative path) and lines. Set dimension to the lens name. If a file is clean under your lenses, return fewer/zero findings — do not invent issues.`
}

function verifyPrompt(f) {
  return `You are an adversarial verifier. A reviewer made this claim about the strider codebase. Your job is to REFUTE it by reading the actual code. Default to "refuted" if you cannot positively confirm the defect is real and actionable.

CLAIM (dimension=${f.dimension}, severity=${f.severity}):
title: ${f.title}
file: ${ROOT}/${f.file}
lines: ${f.lines}
problem: ${f.problem}
evidence: ${f.evidence}
proposed fix: ${f.fix}

Read the cited file(s) around those lines IN FULL (and any directly-related code: the function's callers/callees, the types involved, the relevant trait impls). Determine:
- For soundness claims (asm/opt/self): is there a REAL input that produces wrong behavior? If the claim asserts a miscompile, can you trace a concrete counterexample, or is the "buggy" path actually unreachable / already guarded elsewhere?
- For simplify/runtime/clean/filesplit: is the improvement real and behavior-preserving, or does it miss a reason the current shape exists (e.g. a borrow-checker constraint, an intentional perf choice, a behavioral subtlety)?

Verdict: 'confirmed' only if the defect is real and the fix direction is sound; 'refuted' if it's a false positive or the fix would break something; 'uncertain' if you genuinely cannot tell. Give corrected_severity (downgrade speculative soundness claims you couldn't substantiate). Reasoning must cite specific lines you read.`
}

// ---- Run -------------------------------------------------------------------
phase('Review')
log(`Reviewing ${UNITS.length} units across the production source...`)

const perUnit = await pipeline(
  UNITS,
  (unit) =>
    agent(reviewPrompt(unit), {
      label: `review:${unit.id}`,
      phase: 'Review',
      schema: FINDINGS_SCHEMA,
    }).then((r) => ({ unit, findings: (r?.findings || []).map((f) => ({ ...f, unit: unit.id })) })),
  ({ unit, findings }) => {
    // Verify high/medium soundness+runtime+simplify findings; pass low & clean/filesplit through (marked unverified).
    const needsVerify = (f) =>
      ['high', 'medium'].includes(f.severity) &&
      ['asm', 'opt', 'self', 'runtime', 'simplify'].includes(f.dimension)
    return parallel(
      findings.map((f) => () => {
        if (!needsVerify(f)) return Promise.resolve({ ...f, verdict: 'unverified', verify_reasoning: 'low-severity or design finding — passed through for human triage' })
        return agent(verifyPrompt(f), { label: `verify:${f.unit}:${f.title.slice(0, 30)}`, phase: 'Verify', schema: VERDICT_SCHEMA })
          .then((v) => ({ ...f, verdict: v?.verdict || 'uncertain', verify_reasoning: v?.reasoning || '', severity: v?.corrected_severity || f.severity }))
          .catch(() => ({ ...f, verdict: 'uncertain', verify_reasoning: 'verifier errored' }))
      }),
    ).then((verified) => ({ unit: unit.id, findings: verified.filter(Boolean) }))
  },
)

const allFindings = perUnit.filter(Boolean).flatMap((u) => u.findings)
const surviving = allFindings.filter((f) => f.verdict === 'confirmed' || f.verdict === 'unverified' || f.verdict === 'uncertain')
const refutedCount = allFindings.length - surviving.length

log(`Collected ${allFindings.length} raw findings; ${refutedCount} refuted; ${surviving.length} survive to synthesis.`)

phase('Synthesize')
const summary = await agent(
  `You are the synthesis lead for a deep code review of the strider Rust codebase. Below is the full set of surviving findings (JSON). Produce the consolidated review report.

Tasks:
1. DEDUPLICATE: merge findings that describe the same underlying issue (same file region or same cross-cutting pattern across files — e.g. the same "pass NodeId instead of (class,size,space)" idea found in several units). Keep the union of evidence.
2. RANK within each dimension by (severity, confidence). Dimensions: asm, opt, self = soundness (most important); simplify, runtime, filesplit, clean = quality.
3. For each kept finding give: a stable id (e.g. SND-01, SIMP-03, RT-02, FS-01, CLN-04), dimension, severity, verdict, file:lines, one-paragraph problem, the concrete fix, and confidence.
4. Write a top "Executive Summary": counts by dimension+severity, the 5-10 highest-priority items overall (soundness first), and the recommended fix ordering.
5. For the filesplit dimension, render the proposed target module tree explicitly.

Write the full report as markdown to ${ROOT}/.reviews/deep-review.md (create the directory if needed via the Bash tool: mkdir -p). Use clear headed sections per dimension and a stable-id table.

Then RETURN (as your final text) a concise plain-text executive summary: the counts, and the ranked list of high+medium soundness items (id, file:lines, one-line problem) plus the top simplify/runtime/filesplit recommendations. Keep it under ~400 lines.

FINDINGS JSON:
${JSON.stringify(surviving)}`,
  { label: 'synthesize', phase: 'Synthesize' },
)

return { totalRaw: allFindings.length, refuted: refutedCount, surviving: surviving.length, summary }
