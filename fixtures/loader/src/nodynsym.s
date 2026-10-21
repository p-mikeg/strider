# Two words naming weak undefined symbols, linked as the arm64 kernel is:
# `-shared -Bsymbolic` with `.dynsym` discarded, so `.rela.dyn` keeps
# R_X86_64_64 entries whose r_sym indexes a dynamic table that is gone.
	.text
	.globl	f
f:
	ret

	.section .rodata, "a"
	.weak	missing_a
	.weak	missing_b
	.globl	slots
slots:
	.quad	missing_a
	.quad	missing_b
