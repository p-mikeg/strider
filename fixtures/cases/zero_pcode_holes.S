// AArch64 hand-written shapes that exercise two cfg-builder bugs
// around machine instructions that Sleigh lifts to *zero* pcode ops
// (`nop`, `autiasp`, several hint-class encodings).
//
// `_start` is an entry stub so the linker is happy with `-nostdlib
// -static`; both regression functions are kept reachable through it.
//
// 1) `nop_fallthrough` — triggers
//    "region at PcodeInsnAddr ... has no instructions".
//    The cfg builder explores the `cbz` branch target first; the
//    fall-through path then walks across a zero-pcode-op `nop` before
//    reaching the already-explored `.Ljoin`.  Pre-fix the builder
//    tried to finalise the current builder with empty `insns` and
//    `add_region` rejected it.
//
// 2) `autiasp_split` — triggers
//    "split address ... not found in region's instruction list".
//    A `cbz` targets the address of an `autiasp` (zero pcode ops)
//    sitting inside an already-built region.  Pre-fix the splitter's
//    lexicographic range test said yes but the exact-`position`
//    lookup said no.

    .arch armv8.3-a               // enable autiasp / paciasp encodings

    .text

    .global _start
    .type   _start, %function
_start:
    mov     x0, #0
    bl      nop_fallthrough
    bl      autiasp_split
    mov     w8, #93               // sys_exit on aarch64 Linux (harmless;
    svc     #0                    // we never run this — fixture is only lifted)
    .size   _start, .-_start

    // Bug 1: fall-through across a zero-pcode-op `nop` into a region
    // already explored from the `cbz` taken path.
    .global nop_fallthrough
    .type   nop_fallthrough, %function
nop_fallthrough:
    cbz     x0, .Lnf_join         // explores .Lnf_join first
    nop                           // fall-through region: ONLY a zero-
                                  // pcode-op insn before reaching the
                                  // already-explored .Lnf_join.
.Lnf_join:
    ret
    .size   nop_fallthrough, .-nop_fallthrough

    // Bug 2: `cbz` targets the address of `autiasp` (zero pcode ops)
    // sitting *inside* an already-built region, forcing a region
    // split into a hole that no recorded insn occupies.
    .global autiasp_split
    .type   autiasp_split, %function
autiasp_split:
    cbz     x0, .Las_end          // branch target = autiasp's address
    mov     x0, #1
.Las_end:
    autiasp                       // zero pcode ops — split target
    ret
    .size   autiasp_split, .-autiasp_split
