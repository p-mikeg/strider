// fixtures/cases/elf_relocs.c
//
// Fixture for ET_DYN relocation handling.  Compiled as a shared
// library (-shared -fPIC), this produces an ET_DYN ELF whose
// `.rodata` table `dispatch_table` is populated with function
// pointers that are NOT resolved at link time — the linker emits a
// `R_X86_64_RELATIVE` relocation per slot, and any analyser that
// reads the table without applying relocations sees zeros where the
// function addresses should be.
//
// `dispatch_via_table` reads the table by index and tail-calls
// through the loaded pointer.  Without `apply_elf_relocations`, the
// loaded `dispatch_table` slots are zero so strider follows the
// indirect call into address 0 (an obvious bug); with relocations
// applied, the slots contain the real function addresses and the
// indirect call resolves cleanly.
//
// The companion `compute_via_helper` function calls a same-image
// helper directly — on PIE binaries the linker resolves this to a
// PC-relative `call rel32` at link time, so it survives without
// relocation processing.  Tests use both shapes to distinguish
// "linker-resolved" call sites from "loader-resolved" ones.

int helper_a(int x) { return x + 100; }
int helper_b(int x) { return x + 200; }
int helper_c(int x) { return x + 300; }
int helper_d(int x) { return x + 400; }

typedef int (*handler_t)(int);

// Function-pointer table; each slot lands in `.data.rel.ro` (or
// `.rodata` with `-z relro -z now`) and gets a R_*_RELATIVE
// relocation pointing at the matching helper.
const handler_t dispatch_table[4] = {
    helper_a,
    helper_b,
    helper_c,
    helper_d,
};

// Reads `dispatch_table[idx & 3]` and tail-calls through it.  If
// the relocations aren't applied, `dispatch_table[*]` reads zero
// and the call lands at address 0.
int dispatch_via_table(int idx, int x) {
    return dispatch_table[idx & 3](x);
}

// Direct call to a same-image helper.  PIE / shared-lib linkers
// resolve this at link time (PC-relative `call rel32`), so it
// works without runtime relocation.
int compute_via_helper(int x) {
    return helper_a(x);
}
