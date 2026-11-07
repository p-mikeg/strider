// MIPS64 r2 hand-written after gcc 11's `-O2 -mno-abicalls` output for two
// switches over bit fields of one instruction word, the shape of the Linux
// MIPS kernel's `__mm_isBranchInstr`:
//
//     int bitfield_dispatch(struct { u64 pad, insn; } *d, int y) {
//         u32 w = *(u32 *)((char *)d + 8);  /* insn's high half on mips64be */
//         if (y & 1)
//             switch (w >> 26) { /* 0..7, 16..23, 61 */ default: ... }
//         switch ((w >> 21) & 0x1f) { /* 0, 3, .., 27 */ default: ... }
//     }
//
// gcc guards the `op` switch with `srl t,w,26; sltiu t,t,62` but indexes its
// table with `ext i,w,26,6`: the guard and the index are different
// expressions of the same six bits.  The guard sizes the table at 62 entries
// while the field alone admits 64, and the `rs` table follows directly, so
// `op` slots 62 and 63 are `rs` entries 0 and 1.  The `op` switch has 18
// distinct arms, 17 cases returning `100 + case` and the default.
//
// Built for mips64be and mips64le; every other arch fails to assemble it and
// skips it.  Absolute (`%highest` .. `%lo`) table bases rather than gcc's
// default abicalls GOT load, which no read-only memory serves.

	.set	noreorder
	.set	nomacro
	.text

	.globl	bitfield_dispatch
	.type	bitfield_dispatch, @function
	.ent	bitfield_dispatch
bitfield_dispatch:
	andi	$3,$5,0x1
	lw	$2,8($4)
	beq	$3,$0,.Lrs
	nop

	srl	$3,$2,26
	sltiu	$3,$3,62
	beq	$3,$0,.Lop_default
	ext	$2,$2,26,6
	lui	$3,%highest(.Lop_table)
	lui	$4,%hi(.Lop_table)
	daddiu	$3,$3,%higher(.Lop_table)
	andi	$2,$2,0x00ff
	daddiu	$4,$4,%lo(.Lop_table)
	dsll	$3,$3,32
	dsll	$2,$2,3
	daddu	$3,$3,$4
	daddu	$2,$3,$2
	ld	$2,0($2)
	jr	$2
	nop

.Lrs:
	ext	$3,$2,21,5
	sltiu	$3,$3,28
	beq	$3,$0,.Lrs_default
	ext	$2,$2,21,5
	lui	$3,%highest(.Lrs_table)
	lui	$4,%hi(.Lrs_table)
	daddiu	$3,$3,%higher(.Lrs_table)
	andi	$2,$2,0x00ff
	daddiu	$4,$4,%lo(.Lrs_table)
	dsll	$3,$3,32
	dsll	$2,$2,3
	daddu	$3,$3,$4
	daddu	$2,$3,$2
	ld	$2,0($2)
	jr	$2
	nop

.Lop_0:
	jr	$31
	li	$2,100
.Lop_1:
	jr	$31
	li	$2,101
.Lop_2:
	jr	$31
	li	$2,102
.Lop_3:
	jr	$31
	li	$2,103
.Lop_4:
	jr	$31
	li	$2,104
.Lop_5:
	jr	$31
	li	$2,105
.Lop_6:
	jr	$31
	li	$2,106
.Lop_7:
	jr	$31
	li	$2,107
.Lop_16:
	jr	$31
	li	$2,116
.Lop_17:
	jr	$31
	li	$2,117
.Lop_18:
	jr	$31
	li	$2,118
.Lop_19:
	jr	$31
	li	$2,119
.Lop_20:
	jr	$31
	li	$2,120
.Lop_21:
	jr	$31
	li	$2,121
.Lop_22:
	jr	$31
	li	$2,122
.Lop_23:
	jr	$31
	li	$2,123
.Lop_61:
	jr	$31
	li	$2,161
.Lop_default:
	jr	$31
	li	$2,0
.Lrs_0:
	jr	$31
	li	$2,200
.Lrs_3:
	jr	$31
	li	$2,203
.Lrs_6:
	jr	$31
	li	$2,206
.Lrs_9:
	jr	$31
	li	$2,209
.Lrs_12:
	jr	$31
	li	$2,212
.Lrs_15:
	jr	$31
	li	$2,215
.Lrs_18:
	jr	$31
	li	$2,218
.Lrs_21:
	jr	$31
	li	$2,221
.Lrs_24:
	jr	$31
	li	$2,224
.Lrs_27:
	jr	$31
	li	$2,227
.Lrs_default:
	jr	$31
	li	$2,1
	.end	bitfield_dispatch
	.size	bitfield_dispatch, .-bitfield_dispatch

	.section	.rodata
	.align	3
.Lop_table:
	.dword	.Lop_0, .Lop_1, .Lop_2, .Lop_3
	.dword	.Lop_4, .Lop_5, .Lop_6, .Lop_7
	.dword	.Lop_default, .Lop_default, .Lop_default, .Lop_default
	.dword	.Lop_default, .Lop_default, .Lop_default, .Lop_default
	.dword	.Lop_16, .Lop_17, .Lop_18, .Lop_19
	.dword	.Lop_20, .Lop_21, .Lop_22, .Lop_23
	.dword	.Lop_default, .Lop_default, .Lop_default, .Lop_default
	.dword	.Lop_default, .Lop_default, .Lop_default, .Lop_default
	.dword	.Lop_default, .Lop_default, .Lop_default, .Lop_default
	.dword	.Lop_default, .Lop_default, .Lop_default, .Lop_default
	.dword	.Lop_default, .Lop_default, .Lop_default, .Lop_default
	.dword	.Lop_default, .Lop_default, .Lop_default, .Lop_default
	.dword	.Lop_default, .Lop_default, .Lop_default, .Lop_default
	.dword	.Lop_default, .Lop_default, .Lop_default, .Lop_default
	.dword	.Lop_default, .Lop_default, .Lop_default, .Lop_default
	.dword	.Lop_default, .Lop_61
.Lrs_table:
	.dword	.Lrs_0, .Lrs_default, .Lrs_default, .Lrs_3
	.dword	.Lrs_default, .Lrs_default, .Lrs_6, .Lrs_default
	.dword	.Lrs_default, .Lrs_9, .Lrs_default, .Lrs_default
	.dword	.Lrs_12, .Lrs_default, .Lrs_default, .Lrs_15
	.dword	.Lrs_default, .Lrs_default, .Lrs_18, .Lrs_default
	.dword	.Lrs_default, .Lrs_21, .Lrs_default, .Lrs_default
	.dword	.Lrs_24, .Lrs_default, .Lrs_default, .Lrs_27
