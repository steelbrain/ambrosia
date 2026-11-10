//! The pointer authentication keys. XNU on vmapple never sets them, so the
//! kernel and every process run on the keys the firmware leaves, and the VMM
//! starts each secondary CPU on the keys of the CPU that starts it
//! (docs/boot-process.md, "The pointer authentication keys").

use core::arch::asm;

/// Sets all five keys on this CPU: `keys` holds IA, IB, DA, DB and GA, each
/// as its low then its high half, as `boot::pac_keys` gives them.
pub fn set(keys: &[u64; 10]) {
    // The keys by their encodings, which need no assembler feature.
    // SAFETY: the firmware uses no pointer authentication itself, so
    // changing the keys under it is harmless.
    unsafe {
        asm!(
            "msr s3_0_c2_c1_0, {0}", // APIAKEYLO_EL1
            "msr s3_0_c2_c1_1, {1}", // APIAKEYHI_EL1
            "msr s3_0_c2_c1_2, {2}", // APIBKEYLO_EL1
            "msr s3_0_c2_c1_3, {3}", // APIBKEYHI_EL1
            "msr s3_0_c2_c2_0, {4}", // APDAKEYLO_EL1
            "msr s3_0_c2_c2_1, {5}", // APDAKEYHI_EL1
            "msr s3_0_c2_c2_2, {6}", // APDBKEYLO_EL1
            "msr s3_0_c2_c2_3, {7}", // APDBKEYHI_EL1
            "msr s3_0_c2_c3_0, {8}", // APGAKEYLO_EL1
            "msr s3_0_c2_c3_1, {9}", // APGAKEYHI_EL1
            "isb",
            in(reg) keys[0],
            in(reg) keys[1],
            in(reg) keys[2],
            in(reg) keys[3],
            in(reg) keys[4],
            in(reg) keys[5],
            in(reg) keys[6],
            in(reg) keys[7],
            in(reg) keys[8],
            in(reg) keys[9],
        );
    }
}
