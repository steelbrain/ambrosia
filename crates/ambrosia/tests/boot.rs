//! The boot pipeline on a real guest's files. Set AMBROSIA_TEST_BOOT_FILES to
//! a directory holding a guest's boot objects under the names below (copied
//! from its Preboot and iSCPreboot volumes, plus 1 MiB of its aux drive from
//! the NVRAM offset as `nvram.bin`); without it the test is skipped.

use ambrosia::adt::Node;
use ambrosia::boot::{self, Files, Memory};
use ambrosia::config::{Config, Patch};
use ambrosia::macho::Header;
use ambrosia::tree::Machine;

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

#[test]
fn prepares_a_guest() {
    let Ok(dir) = std::env::var("AMBROSIA_TEST_BOOT_FILES") else {
        eprintln!("AMBROSIA_TEST_BOOT_FILES is unset; skipping");
        return;
    };
    let f = |n: &str| std::fs::read(format!("{dir}/{n}")).unwrap();
    let text = |n: &str| String::from_utf8(f(n)).unwrap().trim().to_string();
    let (vg, bd) = (text("volume-group"), text("boot-dir"));
    let (dt, kernel, manifest) = (f("devicetree.img4"), f("kernelcache"), f("apticket.der"));
    let (rh, brh) = (f("root_hash.img4"), f("base_system_root_hash.img4"));
    let (stc, btc) = (f("StaticTrustCache.img4"), f("BaseSystemTrustCache.img4"));
    let (policy, sfr, nvram) = (f("localpolicy.img4"), f("sfr-apticket.der"), f("nvram.bin"));
    assert_eq!(boot::nvram_location(&dt).unwrap(), (0xa0_0000, 0x10_0000));

    let m = Machine {
        ram_base: 0x7000_0000,
        ram_size: 0x2_0000_0000,
        pcie_window: (0x5000_0000, 0x1fff_0000),
        cpus: 4,
        ecid: 1,
        rnd: 0x6a5a_78a3,
        serial: "1234".into(),
        model: "VM0001".into(),
        region: "D/A".into(),
        soc_name: "Apple M1 (Virtual)".into(),
        display: None,
        display_node: None,
        avp_rtc: true,
    };
    let files = Files {
        volume_group: &vg,
        boot_dir: &bd,
        manifest: &manifest,
        kernel: &kernel,
        device_tree: &dt,
        root_hash: &rh,
        base_root_hash: &brh,
        static_trust_cache: &stc,
        base_trust_cache: &btc,
        policy: &policy,
        sfr_manifest: &sfr,
        nvram_region: &nvram,
    };
    let ram_end = m.ram_base + m.ram_size;
    let mut ram = Ram {
        base: m.ram_base,
        low: vec![0; 512 << 20],
        high_at: ram_end - (64 << 20),
        high: vec![0; 64 << 20],
    };
    // A configuration patch on the kernel: its first version string.
    let config = Config {
        patches: vec![Patch {
            identifier: "com.apple.kernel".into(),
            find: b"Darwin Kernel Version ".to_vec(),
            replace: b"Ambrosia-patched kern ".to_vec(),
            count: 1,
        }],
        ..Config::default()
    };
    let mut lines = Vec::new();
    let h = boot::prepare(
        &m,
        &files,
        &config,
        &mut ram,
        m.ram_base + (1 << 30),
        &[7; 48],
        &mut |a| lines.push(a.to_string()),
    )
    .unwrap();
    for l in &lines {
        eprintln!("{l}");
    }

    // The boot arguments point at a tree that parses and says what was placed.
    let args = ram
        .bytes(h.boot_args, ambrosia::bootargs::SIZE)
        .unwrap()
        .to_vec();
    let u64_at = |at: usize| u64::from_le_bytes(args[at..at + 8].try_into().unwrap());
    let (virt, phys) = (u64_at(8), u64_at(16));
    assert_eq!(virt.wrapping_sub(phys) % (32 << 20), 0);
    let dt_pa = u64_at(0x60) - virt + phys;
    let dt_len = u32::from_le_bytes(args[0x68..0x6c].try_into().unwrap()) as usize;
    let (tree, _) = Node::parse(ram.bytes(dt_pa, dt_len).unwrap()).unwrap();
    let chosen = tree.find("chosen").unwrap();
    // The longer layout's memSizeActual is /chosen/dram-size, which the
    // kernel checks it against; the shorter layout's is 0.
    let dram_size = u64::from_le_bytes(chosen.get("dram-size").unwrap().try_into().unwrap());
    assert_eq!((u64_at(1144), u64_at(728)), (dram_size, 0));
    assert_eq!(chosen.get_u32("secure-boot"), Some(1));
    assert!(
        chosen
            .get("root-snapshot-name")
            .unwrap()
            .starts_with(b"com.apple.os.update-")
    );
    assert!(tree.find("arm-io/gfx").is_none());
    assert!(tree.find("arm-io/iosurface-mapper").is_none());
    assert!(tree.find("arm-io/avp-ctrr").is_none());
    // avp-sealed-registers is only in some builds' templates; Ambrosia keeps
    // it where it is, so assert its presence matches the input tree's.
    let template = ambrosia::img4::parse(&dt).unwrap().unpack().unwrap();
    let (orig, _) = Node::parse(&template).unwrap();
    assert_eq!(
        tree.find("arm-io/avp-sealed-registers").is_some(),
        orig.find("arm-io/avp-sealed-registers").is_some()
    );
    assert!(tree.find("arm-io/avp-rtc").is_some());
    assert!(tree.find("arm-io/pl031-rtc").is_none());
    // The kernel gets the guest's current NVRAM bank. A guest that has never
    // run with avp-rtc has no RTC offset there, and gets the one for zero; a
    // guest that has keeps its own. Either way the bank stays valid.
    let bank = ambrosia::nvram::current(&nvram, 2).unwrap().data.to_vec();
    let mut want = bank.clone();
    let added = ambrosia::nvram::add_if_absent(&mut want, b"com.apple.System.rtc-offset", b"0x1");
    let proxy = chosen.get("nvram-proxy-data").unwrap();
    assert!(proxy == &want[..]);
    let name = b"com.apple.System.rtc-offset=";
    assert!(proxy.windows(name.len()).any(|w| w == name));
    assert_eq!(
        lines.contains(&"NVRAM has no RTC offset: set it to zero".to_string()),
        added
    );
    assert!(ambrosia::nvram::current(proxy, 1).is_some());
    assert!(tree.find("filesystems").is_none());
    assert_eq!(tree.find("cpus").unwrap().children.len(), 4);

    let asmb = chosen.child("asmb").unwrap();
    assert_eq!(asmb.get("lp-nsih").unwrap().len(), 48);

    // The entry point is in the placed collection, at its unslid offset.
    let map = chosen.find("memory-map").unwrap();
    let start = |name: &str| u64::from_le_bytes(map.get(name).unwrap()[..8].try_into().unwrap());
    let kc_at = start("BootKC-ro");
    // The trust caches are below the collection, in its 32 MiB block, and
    // the device tree and boot arguments above it.
    assert!(start("TrustCache") < kc_at);
    assert_eq!(start("TrustCache") >> 25, kc_at >> 25);
    assert!(start("DeviceTree") > kc_at);
    assert!(start("BootArgs") > kc_at);
    let header = Header::parse(ram.bytes(kc_at, 0x10000).unwrap()).unwrap();
    assert_eq!(h.entry - kc_at, header.entry.unwrap() - header.vm_range().0);
    assert!(lines.iter().any(|l| l.ends_with("PAC spins made nops")));
    assert!(lines.contains(&"patch for com.apple.kernel: 1 replaced".to_string()));
    let kc = ram.bytes(kc_at, 0x4000000).unwrap();
    assert!(kc.windows(22).any(|w| w == b"Ambrosia-patched kern "));

    // On a machine without avp-rtc, the guest keeps the PL031, and its NVRAM
    // is left as it is.
    let m = Machine {
        avp_rtc: false,
        ..m.clone()
    };
    let mut ram = Ram {
        base: m.ram_base,
        low: vec![0; 512 << 20],
        high_at: ram_end - (64 << 20),
        high: vec![0; 64 << 20],
    };
    let h = boot::prepare(
        &m,
        &files,
        &Config::default(),
        &mut ram,
        m.ram_base + (1 << 30),
        &[7; 48],
        &mut |_| {},
    )
    .unwrap();
    let args = ram.bytes(h.boot_args, 0x70).unwrap().to_vec();
    let u64_at = |at: usize| u64::from_le_bytes(args[at..at + 8].try_into().unwrap());
    let dt_pa = u64_at(0x60) - u64_at(8) + u64_at(16);
    let dt_len = u32::from_le_bytes(args[0x68..0x6c].try_into().unwrap()) as usize;
    let (tree, _) = Node::parse(ram.bytes(dt_pa, dt_len).unwrap()).unwrap();
    assert!(tree.find("arm-io/avp-rtc").is_none());
    assert!(tree.find("arm-io/pl031-rtc").is_some());
    let proxy = tree
        .find("chosen")
        .unwrap()
        .get("nvram-proxy-data")
        .unwrap();
    assert!(proxy == &bank[..]);
}
