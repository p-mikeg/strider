// x86-64 hand-written jumps through a value that is one of a few constants
// chosen by branches, a phi of constants once lifted:
//
// `phi_table_slot` reads its target from one of three table slots, `tbl`,
// `tbl+8` and `tbl+0x20`.  The two slots between the second and the third
// hold `.Lslot_2` and `.Lslot_3`, which this jump can never read, so its
// arms are `.Lslot_0`, `.Lslot_1` and `.Lslot_4`.
//
// `phi_label` picks one of two labels with `cmovne` and jumps to it, the
// shape gcc -O2 gives a computed goto whose index is a known constant on
// every path.  Its arms are `.Llabel_0` and `.Llabel_1`; `.Llabel_unused`
// between them is never a target.
//
// Built for x64; every other arch fails to assemble it and skips it.

	.text

	.globl	phi_table_slot
	.type	phi_table_slot, @function
phi_table_slot:
	mov	$tbl, %rax
	test	%edi, %edi
	jne	1f
	mov	$tbl+8, %rax
	test	%esi, %esi
	jne	1f
	mov	$tbl+0x20, %rax
1:	jmp	*(%rax)
.Lslot_0:
	mov	$1, %eax
	ret
.Lslot_1:
	mov	$2, %eax
	ret
.Lslot_2:
	mov	$3, %eax
	ret
.Lslot_3:
	mov	$4, %eax
	ret
.Lslot_4:
	mov	$5, %eax
	ret
	.size	phi_table_slot, .-phi_table_slot

	.globl	phi_label
	.type	phi_label, @function
phi_label:
	mov	$.Llabel_0, %eax
	mov	$.Llabel_1, %edx
	test	%edi, %edi
	cmovne	%rdx, %rax
	jmp	*%rax
.Llabel_0:
	mov	$10, %eax
	ret
.Llabel_unused:
	mov	$11, %eax
	ret
.Llabel_1:
	mov	$12, %eax
	ret
	.size	phi_label, .-phi_label

	.section	.rodata
	.p2align	3
tbl:
	.quad	.Lslot_0, .Lslot_1, .Lslot_2, .Lslot_3, .Lslot_4
