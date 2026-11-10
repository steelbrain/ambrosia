//! The kernel's PAC spins.
//!
//! A vmapple kernel asks the hypervisor to manage its pointer authentication
//! keys with `hvc #0`. Hypervisor.framework's public API fails those calls,
//! and from macOS 14 on the kernel then spins: `hvc #0; cbnz x0, .`.
//! Replacing each spin with a `nop` lets the kernel run on the keys the
//! firmware set, as macOS 13's kernel does (docs/boot-process.md, "The PAC
//! nop pass").

const HVC_0: u32 = 0xd400_0002;
const CBNZ_X0_SELF: u32 = 0xb500_0000;
const NOP: u32 = 0xd503_201f;

/// Turns every `cbnz x0, .` that directly follows an `hvc #0` in `code` into
/// a `nop`, and gives how many it changed. `code` starts on an instruction;
/// bytes past its last whole word are left as they are.
pub fn skip_spins(code: &mut [u8]) -> usize {
    let mut n = 0;
    let mut prev = 0;
    for word in code.as_chunks_mut::<4>().0 {
        let w = u32::from_le_bytes(*word);
        if w == CBNZ_X0_SELF && prev == HVC_0 {
            *word = NOP.to_le_bytes();
            n += 1;
        }
        prev = w;
    }
    n
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_spins_after_hvc() {
        let words = [HVC_0, CBNZ_X0_SELF, CBNZ_X0_SELF, NOP, HVC_0, CBNZ_X0_SELF];
        let mut code: Vec<u8> = words.iter().flat_map(|w| w.to_le_bytes()).collect();
        assert_eq!(skip_spins(&mut code), 2);
        let back: Vec<u32> = code
            .chunks(4)
            .map(|c| u32::from_le_bytes(c.try_into().unwrap()))
            .collect();
        assert_eq!(back, [HVC_0, NOP, CBNZ_X0_SELF, NOP, HVC_0, NOP]);
    }
}
