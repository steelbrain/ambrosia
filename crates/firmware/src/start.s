// The image's entry, and the two places it leaves the Rust code: an
// exception, and the jump into the kernel.

    .section .text.start, "ax"
    .global _start
_start:
    // vmapple starts the boot CPU here at EL1, with the MMU and caches off.
    // The other CPUs stay off until the kernel starts them through PSCI.
    msr     daifset, #0xf
    // FP and SIMD on: the compiler uses them for copies.
    mov     x0, #(3 << 20)
    msr     cpacr_el1, x0
    isb
    adrp    x0, vectors
    add     x0, x0, :lo12:vectors
    msr     vbar_el1, x0
    adrp    x0, __stack_top
    add     x0, x0, :lo12:__stack_top
    mov     sp, x0
    adrp    x0, __bss_start
    add     x0, x0, :lo12:__bss_start
    adrp    x1, __bss_end
    add     x1, x1, :lo12:__bss_end
1:  cmp     x0, x1
    b.hs    2f
    stp     xzr, xzr, [x0], #16
    b       1b
2:  bl      main
3:  wfi
    b       3b

// Each vector saves x0 to x30 on the stack and passes its index and their
// address to the handler, which reports the exception and never returns.
    .macro vector index
    .balign 0x80
    sub     sp, sp, #(32 * 8)
    stp     x0, x1, [sp, #(0 * 8)]
    mov     x0, #\index
    mov     x1, sp
    b       exception_entry
    .endm

    .balign 0x800
vectors:
    vector 0
    vector 1
    vector 2
    vector 3
    vector 4
    vector 5
    vector 6
    vector 7
    vector 8
    vector 9
    vector 10
    vector 11
    vector 12
    vector 13
    vector 14
    vector 15

exception_entry:
    stp     x2, x3, [x1, #(2 * 8)]
    stp     x4, x5, [x1, #(4 * 8)]
    stp     x6, x7, [x1, #(6 * 8)]
    stp     x8, x9, [x1, #(8 * 8)]
    stp     x10, x11, [x1, #(10 * 8)]
    stp     x12, x13, [x1, #(12 * 8)]
    stp     x14, x15, [x1, #(14 * 8)]
    stp     x16, x17, [x1, #(16 * 8)]
    stp     x18, x19, [x1, #(18 * 8)]
    stp     x20, x21, [x1, #(20 * 8)]
    stp     x22, x23, [x1, #(22 * 8)]
    stp     x24, x25, [x1, #(24 * 8)]
    stp     x26, x27, [x1, #(26 * 8)]
    stp     x28, x29, [x1, #(28 * 8)]
    str     x30, [x1, #(30 * 8)]
    bl      exception
4:  wfi
    b       4b

// enter_kernel(entry, boot_args): turn the MMU and caches off, as the kernel
// expects them at entry, and jump to it with x0 at its boot arguments. The
// caller has cleaned to the point of coherency everything the kernel reads.
    .global enter_kernel
enter_kernel:
    mov     x19, x0
    mov     x20, x1
    mrs     x2, sctlr_el1
    bic     x2, x2, #(1 << 0)       // M
    bic     x2, x2, #(1 << 2)       // C
    bic     x2, x2, #(1 << 12)      // I
    msr     sctlr_el1, x2
    isb
    ic      iallu
    tlbi    vmalle1
    dsb     sy
    isb
    msr     vbar_el1, xzr
    msr     ttbr0_el1, xzr
    msr     tcr_el1, xzr
    isb
    // As iBoot does, enter with x30 at the entry point and every other
    // general register zero.
    mov     x0, x20
    mov     x30, x19
    .irp    r, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24, 25, 26, 27, 28, 29
    mov     x\r, xzr
    .endr
    br      x30
