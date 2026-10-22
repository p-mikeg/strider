# R_X86_64_32 against `foo`, whose value must zero-extend (x86-64 psABI
# 4.4): GNU ld refuses the first, which only fits signed, and takes the
# second, which only fits unsigned.
	.text
	.globl	f
	.type	f, @function
f:	movl	$foo-0x20000000, %eax
	movl	$foo+0x80000000, %ecx
	ret

	.data
	.globl	foo
foo:	.long	0
