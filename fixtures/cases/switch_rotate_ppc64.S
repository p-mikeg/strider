// gcc 11 `-O2 -mcpu=power8` output for ppc64le, with only the .file/.ident
// lines dropped, of
//
//     unsigned s_shift(unsigned x, unsigned y) {
//         switch (x >> 28) { /* 0..10, each returning its own f(y) */ default: return y; }
//     }
//
// gcc rotates `x` into the low word with `rldicl 9,3,36,32`, guards the
// truncated low word with `cmplwi 9,10`, and indexes the table with
// `rldic 9,9,2,58` of the whole rotated register: the guard and the index
// read the rotate through different masks.  The table has 11 entries.
//
// Built for ppc64le and ppc64be; every other arch fails to assemble or link
// it and skips it.

	.machine power8
	.abiversion 2
	.section	".text"
	.section	".toc","aw"
	.align 3
.LC0:
	.quad	.L4
	.section	".text"
	.align 2
	.p2align 4,,15
	.globl s_shift
	.type	s_shift, @function
s_shift:
.LCF0:
0:	addis 2,12,.TOC.-.LCF0@ha
	addi 2,2,.TOC.-.LCF0@l
	.localentry	s_shift,.-s_shift
	rldicl 9,3,36,32
	cmplwi 0,9,10
	bgt 0,.L15
	addis 10,2,.LC0@toc@ha
	ld 10,.LC0@toc@l(10)
	rldic 9,9,2,58
	lwax 9,10,9
	add 9,9,10
	mtctr 9
	bctr
	.p2align 2
	.align 2
.L4:
	.long .L14-.L4
	.long .L13-.L4
	.long .L12-.L4
	.long .L11-.L4
	.long .L10-.L4
	.long .L9-.L4
	.long .L8-.L4
	.long .L7-.L4
	.long .L6-.L4
	.long .L5-.L4
	.long .L3-.L4
	.p2align 4,,15
.L5:
	lis 10,0x446f
	ori 10,10,0x8657
	mulhwu 9,4,10
	subf 4,9,4
	srwi 4,4,1
	add 9,9,4
	srwi 3,9,6
.L2:
	rldicl 3,3,0,32
	blr
	.p2align 4,,15
.L3:
	lis 10,0x21fb
	ori 10,10,0x7813
	mulhwu 9,4,10
	subf 10,9,4
	srwi 10,10,1
	add 9,9,10
	srwi 9,9,6
	mulli 9,9,113
	subf 3,9,4
	b .L2
	.p2align 4,,15
.L14:
	slwi 9,4,1
	add 3,9,4
	b .L2
	.p2align 4,,15
.L13:
	addi 3,4,17
	b .L2
	.p2align 4,,15
.L12:
	xori 3,4,0x1d
	b .L2
	.p2align 4,,15
.L11:
	addi 3,4,-41
	b .L2
	.p2align 4,,15
.L10:
	slwi 3,4,5
	b .L2
	.p2align 4,,15
.L9:
	srwi 3,4,6
	b .L2
	.p2align 4,,15
.L8:
	ori 3,4,0x4f
	b .L2
	.p2align 4,,15
.L7:
	mulli 3,4,83
	b .L2
	.p2align 4,,15
.L6:
	andi. 3,4,0x61
	b .L2
.L15:
	mr 3,4
	b .L2
	.long 0
	.byte 0,0,0,0,0,0,0,0
	.size	s_shift,.-s_shift
	.section	.note.GNU-stack,"",@progbits
