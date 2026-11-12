//! The restore input on a real IPSW's restore objects. Set
//! `AMBROSIA_TEST_RESTORE` to a directory holding `restore.cpio` (the restore
//! bundle, every member but the ramdisk) and `restore-ramdisk.im4p` (the
//! ramdisk, loaded on its own); without it the test is skipped. The bytes are
//! the user's own IPSW, so they live outside the tree.

use ambrosia::adt::Node;
use ambrosia::boot::{self, Memory};
use ambrosia::bundle::Bundle;
use ambrosia::img4::Packing;
use ambrosia::restore::{self, Input};
use ambrosia::tree::Machine;

/// Guest RAM as two sparse regions: the kernel's static memory from the base,
/// and the carveouts at the top.
struct Ram {
    base: u64,
    low: Vec<u8>,
    high_at: u64,
    high: Vec<u8>,
}

impl Memory for Ram {
    fn bytes(&mut self, pa: u64, len: usize) -> Option<&mut [u8]> {
        let end = pa.checked_add(len as u64)?;
        for (at, mem) in [(self.base, &mut self.low), (self.high_at, &mut self.high)] {
            if pa >= at && end <= at + mem.len() as u64 {
                let o = (pa - at) as usize;
                return Some(&mut mem[o..o + len]);
            }
        }
        None
    }
}

/// The vmapple machine the restore boot runs on: RAM at 4 GiB, as the firmware
/// gives it.
fn machine() -> Machine {
    Machine {
        ram_base: 0x1_0000_0000,
        ram_size: 0x2_0000_0000,
        pcie_window: (0x5000_0000, 0x1fff_0000),
        cpus: 4,
        ecid: 0x1234_5678_9abc_def0,
        rnd: 0x6a5a_78a3,
        serial: "1234".into(),
        model: "VM0001".into(),
        region: "D/A".into(),
        soc_name: "Apple M1 (Virtual)".into(),
        display: None,
        display_node: None,
        avp_rtc: true,
    }
}

/// A valid NVRAM bank of `size` bytes with the given generation: the header
/// partition's signature and magic, the generation at +0x14, and an Adler-32
/// over the bytes from +0x14 on at +0x10, as the aux drive holds a bank.
fn nvram_bank(size: usize, generation: u32) -> Vec<u8> {
    fn adler32(data: &[u8]) -> u32 {
        let (mut a, mut b) = (1u32, 0u32);
        for chunk in data.chunks(5552) {
            for &x in chunk {
                a += u32::from(x);
                b += a;
            }
            a %= 65521;
            b %= 65521;
        }
        (b << 16) | a
    }
    let mut v = vec![0u8; size];
    v[0] = 0x5a;
    v[4..9].copy_from_slice(b"nvram");
    v[0x14..0x18].copy_from_slice(&generation.to_le_bytes());
    let sum = adler32(&v[0x14..]);
    v[0x10..0x14].copy_from_slice(&sum.to_le_bytes());
    v
}

/// A 16-byte memory-map range as `(address, size)`.
fn mm_range(mm: &Node, name: &str) -> (u64, u64) {
    let r = mm
        .get(name)
        .unwrap_or_else(|| panic!("memory-map has no {name}"));
    let n = |i: usize| u64::from_le_bytes(r[i..i + 8].try_into().unwrap());
    (n(0), n(8))
}

#[test]
fn reads_a_real_restore_bundle() {
    let Ok(dir) = std::env::var("AMBROSIA_TEST_RESTORE") else {
        eprintln!("AMBROSIA_TEST_RESTORE is unset; skipping");
        return;
    };
    let archive = std::fs::read(format!("{dir}/restore.cpio")).unwrap();
    let ramdisk = std::fs::read(format!("{dir}/restore-ramdisk.im4p")).unwrap();

    let bundle = Bundle::find(&archive).unwrap().unwrap();
    assert!(
        restore::present(&bundle),
        "a restore bundle holds the kernelcache"
    );
    let input = Input::from(&bundle, &ramdisk).unwrap();

    // The four images carry the tags the IPSW gives them.
    assert_eq!(&input.kernelcache.kind, b"krnl");
    assert_eq!(&input.device_tree.kind, b"dtre");
    assert_eq!(&input.trust_cache.kind, b"rtsc");
    assert_eq!(&input.ramdisk.kind, b"rdsk");

    // The kernelcache and device tree are lzfse; the ramdisk is a raw APFS
    // container, so its payload is handed over as stored.
    assert_eq!(input.kernelcache.packing, Packing::Lzfse);
    assert_eq!(input.device_tree.packing, Packing::Lzfse);
    assert_eq!(input.ramdisk.packing, Packing::Raw);

    // The device tree inflates to a real tree: the root node names the
    // properties iBoot would place for a device tree.
    let dt = input.device_tree.unpack().unwrap();
    assert!(dt.len() > input.device_tree.data.len());
    assert!(
        dt.windows(5).any(|w| w == b"model"),
        "the inflated device tree names its model"
    );

    // The handover members come through whole.
    assert_eq!(input.nonce.len(), 32);
    assert!(input.boot_args.contains("-restore"));
    assert!(input.boot_args.contains("rd=md0"));
    assert!(!input.apticket.is_empty());
    assert!(!input.policy.is_empty());
}

#[test]
fn prepares_a_real_restore_boot() {
    let Ok(dir) = std::env::var("AMBROSIA_TEST_RESTORE") else {
        eprintln!("AMBROSIA_TEST_RESTORE is unset; skipping");
        return;
    };
    let archive = std::fs::read(format!("{dir}/restore.cpio")).unwrap();
    let ramdisk = std::fs::read(format!("{dir}/restore-ramdisk.im4p")).unwrap();
    let bundle = Bundle::find(&archive).unwrap().unwrap();
    let input = Input::from(&bundle, &ramdisk).unwrap();

    let m = machine();
    // The firmware loads the bundle at RAM_BASE + 512 MiB; the kernel's static
    // memory must end below it. The carveouts sit at the top of RAM.
    let limit = m.ram_base + (512 << 20);
    let mut ram = Ram {
        base: m.ram_base,
        low: vec![0u8; (limit - m.ram_base) as usize],
        high_at: m.ram_base + m.ram_size - (8 << 20),
        high: vec![0u8; 8 << 20],
    };
    let seed = boot::seed(m.rnd, 1);
    // The guest's NVRAM: 64 KiB of two 32 KiB banks, the second current. The
    // restore boot reads the current bank and hands it to the kernel, as the
    // installed boot does.
    let mut nvram_region = nvram_bank(0x8000, 3);
    nvram_region.extend(nvram_bank(0x8000, 7));
    let mut log: Vec<String> = Vec::new();
    let handoff = boot::prepare_restore(
        &m,
        &input,
        &nvram_region,
        &mut ram,
        limit,
        &seed,
        &mut |a| {
            log.push(format!("{a}"));
        },
    )
    .unwrap();
    for line in &log {
        eprintln!("{line}");
    }

    // The entry is in the kernel's static memory, below the carveouts.
    let kc_size = input.kernelcache.size.unwrap() as u64;
    assert!(handoff.entry >= m.ram_base);

    // Read the boot arguments back from where they were placed.
    let ba = handoff.boot_args;
    let read_u64 = |ram: &mut Ram, pa: u64| -> u64 {
        u64::from_le_bytes(ram.bytes(pa, 8).unwrap()[..8].try_into().unwrap())
    };
    let read_u32 = |ram: &mut Ram, pa: u64| -> u32 {
        u32::from_le_bytes(ram.bytes(pa, 4).unwrap()[..4].try_into().unwrap())
    };
    let virt_base = read_u64(&mut ram, ba + 8);
    let phys_base = read_u64(&mut ram, ba + 16);
    let top = read_u64(&mut ram, ba + 32);
    let dt_virt = read_u64(&mut ram, ba + 96);
    let dt_len = u64::from(read_u32(&mut ram, ba + 104));
    assert_eq!(phys_base, m.ram_base);
    let dt_phys = dt_virt - virt_base + phys_base;

    // The command line is the restore boot-args.
    let cl = {
        let b = ram.bytes(ba + 108, 256).unwrap();
        let end = b.iter().position(|&c| c == 0).unwrap();
        String::from_utf8(b[..end].to_vec()).unwrap()
    };
    assert_eq!(cl, input.boot_args);
    assert!(cl.contains("rd=md0"));

    // Parse the placed device tree and check the restore layout invariants.
    let dt_bytes = ram.bytes(dt_phys, dt_len as usize).unwrap().to_vec();
    let (dt, _) = Node::parse(&dt_bytes).unwrap();
    let mm = dt
        .find("chosen/memory-map")
        .expect("the tree has a memory map");
    let (dt_mm_at, _dt_region) = mm_range(mm, "DeviceTree");
    let (tc_at, tc_region) = mm_range(mm, "TrustCache");
    let (kc_at, _) = mm_range(mm, "BootKC-ro");
    let (rd_at, rd_len) = mm_range(mm, "RAMDisk");

    // The memory map places the device tree where the boot arguments point.
    assert_eq!(
        dt_mm_at, dt_phys,
        "device tree memory-map matches boot-args"
    );
    // The restore kernel requires the trust cache directly below the
    // collection, as the installed system's is; it panics on one above it.
    assert!(tc_at < kc_at, "trust cache below the collection");
    assert_eq!(
        tc_at + tc_region,
        kc_at,
        "trust cache directly below the collection"
    );
    // The ramdisk ends below the end of the kernel's data, which the kernel
    // frees above.
    assert!(rd_at + rd_len <= top, "ramdisk below topOfKernelData");
    // The ramdisk's bytes are the raw APFS container: its superblock magic
    // `NXSB` sits at offset 0x20.
    assert_eq!(&ram.bytes(rd_at + 0x20, 4).unwrap()[..4], b"NXSB");

    // The restore boot keeps the CTRR node the installed system removes.
    assert!(
        dt.find("arm-io/avp-ctrr").is_some(),
        "restore keeps avp-ctrr"
    );

    // The entry is inside the placed collection.
    assert!(handoff.entry >= kc_at && handoff.entry < kc_at + kc_size);

    // The current bank of the aux's NVRAM is handed to the kernel: the second,
    // with the higher generation, whole.
    let chosen = dt.find("chosen").expect("the tree has a chosen node");
    assert_eq!(chosen.get_u32("nvram-bank-count"), Some(2));
    assert_eq!(chosen.get_u32("nvram-bank-size"), Some(0x8000));
    assert_eq!(chosen.get_u32("nvram-current-bank"), Some(1));
    assert_eq!(
        chosen.get("nvram-proxy-data").map(|d| d.len()),
        Some(0x8000)
    );

    // The restore boot marks itself a remote boot, so the kernel's boot policy
    // takes its restore path rather than a normal OS boot.
    assert_eq!(chosen.get_u32("boot-command"), Some(7));
    assert_eq!(chosen.get("boot-type"), Some(&b"remote\0"[..]));
    assert_eq!(
        dt.find("chosen/handoff")
            .and_then(|h| h.get_u32("boot_type")),
        Some(7)
    );
}
