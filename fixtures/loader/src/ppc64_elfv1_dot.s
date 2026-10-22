# A dot-symbol function, as a ppc64 ELFv1 kernel spells them: `.wide` sizes
# the code and `wide` the `.opd` it starts, here two descriptors.
	.section .opd, "aw"
	.align	3
	.globl	wide
	.type	wide, @function
wide:	.quad	.wide, .TOC.@tocbase, 0
	.quad	.wide, .TOC.@tocbase, 0
	.size	wide, 48

	.text
	.globl	.wide
	.type	.wide, @function
.wide:	li	3, 1
	blr
	.size	.wide, 8
