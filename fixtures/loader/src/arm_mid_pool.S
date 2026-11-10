/* A literal pool after a call to an unmarked no-return function, between
   code the ELF's mapping symbols mark: `$d` must keep it undecoded. */
    .file "arm_mid_pool.S"
    .syntax unified
    .text
    .arm
    .global _start
_start: b _start
    .global abort
    .type abort, %function
abort:
    b abort
    .size abort, .-abort
    .global mid_pool
    .type mid_pool, %function
mid_pool:
    push {r4, lr}
    cmp r0, #0
    bne 2f
    bl abort
.Lpool:
    .word 0x00012345
    .word 0x00010064
2:  ldr r1, .Lpool
    add r0, r0, r1
    pop {r4, pc}
    .size mid_pool, .-mid_pool
