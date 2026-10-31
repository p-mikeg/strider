/* ELFv2 local entries. `stop` and `hook` set up the TOC at their global
 * entry and declare a local one 8 bytes in, so the linker points each
 * same-TOC `bl` at sym + 8. `hook_alt`, 4 bytes into `hook`, declares its
 * local entry 4 bytes in, so the two globals share one. `sink` sets up
 * none, so its two entries meet.
 *
 * `noret_call` calls `stop`, which never returns, only when r3 is nonzero,
 * then joins the other path: the `nop` after the call is reached only by
 * falling through it. `relay` calls `hook` with r4 live and passes it on to
 * `sink` untouched. */
	.abiversion 2
	.text

	.globl	stop
	.type	stop, @function
stop:
0:	addis	2, 12, .TOC.-0b@ha
	addi	2, 2, .TOC.-0b@l
	.localentry	stop, .-stop
1:	b	1b
	.size	stop, .-stop

	.globl	hook
	.type	hook, @function
hook:
0:	addis	2, 12, .TOC.-0b@ha
	.globl	hook_alt
hook_alt:
	addi	2, 2, .TOC.-0b@l
	.localentry	hook, .-hook
	.localentry	hook_alt, .-hook_alt
	blr
	.size	hook, .-hook

	.globl	sink
	.type	sink, @function
sink:
	blr
	.size	sink, .-sink

	.globl	noret_call
	.type	noret_call, @function
noret_call:
0:	addis	2, 12, .TOC.-0b@ha
	addi	2, 2, .TOC.-0b@l
	.localentry	noret_call, .-noret_call
	mflr	0
	std	0, 16(1)
	stdu	1, -32(1)
	cmpdi	3, 0
	beq	0, .Ljoin
	bl	stop
	nop
.Ljoin:
	addi	3, 3, 1
	addi	1, 1, 32
	ld	0, 16(1)
	mtlr	0
	blr
	.size	noret_call, .-noret_call

	.globl	relay
	.type	relay, @function
relay:
0:	addis	2, 12, .TOC.-0b@ha
	addi	2, 2, .TOC.-0b@l
	.localentry	relay, .-relay
	mflr	0
	std	0, 16(1)
	stdu	1, -32(1)
	bl	hook
	nop
	bl	sink
	nop
	addi	1, 1, 32
	ld	0, 16(1)
	mtlr	0
	blr
	.size	relay, .-relay

/* `tail_relay` reads r14 and tail-calls `hook`, which the linker points
 * at `hook`'s local entry. */
	.globl	tail_relay
	.type	tail_relay, @function
tail_relay:
0:	addis	2, 12, .TOC.-0b@ha
	addi	2, 2, .TOC.-0b@l
	.localentry	tail_relay, .-tail_relay
	add	4, 4, 14
	b	hook
	.size	tail_relay, .-tail_relay
