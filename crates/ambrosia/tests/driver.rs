//! A real driver module through the loader. Set AMBROSIA_TEST_DRIVER to a
//! module's ELF file, such as the example's, which `make bochs-display`
//! builds as target/bochs-display.elf; without it the test is skipped.

use ambrosia::elf::Module;

#[test]
fn loads_a_module() {
    let Ok(path) = std::env::var("AMBROSIA_TEST_DRIVER") else {
        eprintln!("AMBROSIA_TEST_DRIVER is unset; skipping");
        return;
    };
    let elf = std::fs::read(path).unwrap();
    let m = Module::parse(&elf).unwrap();
    assert!(m.align.is_power_of_two());
    let base = 0x1_4000_0000;
    let mut image = vec![0; m.size];
    let entry = m.load(&mut image, base).unwrap();
    assert!((base..base + m.size as u64).contains(&entry));
}
