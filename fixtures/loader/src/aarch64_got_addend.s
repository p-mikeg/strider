/* GOT references to `foo + 16`. AAELF64 asks for the slot holding foo + 16,
 * which GNU ld drops the addend of and lld rejects. The symbols are numbered
 * foo, baz, bar, so the slot two past foo's is bar's; `h` takes bar's and
 * baz's slots with no addend. */
	.data
	.globl	foo
foo:	.quad	1, 2, 3, 4
	.globl	baz
baz:	.quad	6
	.globl	bar
bar:	.quad	5

	.text
	.globl	f
	.type	f, %function
f:	ldr	x0, :got:foo+16
	ldr	x0, [x0]
	ret
	.globl	g
	.type	g, %function
g:	adrp	x0, :got:foo+16
	ldr	x0, [x0, :got_lo12:foo+16]
	ldr	x0, [x0]
	ret
	.globl	h
	.type	h, %function
h:	ldr	x0, :got:bar
	ldr	x1, :got:baz
	ldr	x0, [x0]
	ret
