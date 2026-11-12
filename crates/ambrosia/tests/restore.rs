//! The restore input on a real IPSW's restore objects. Set
//! `AMBROSIA_TEST_RESTORE` to a directory holding `restore.cpio` (the restore
//! bundle, every member but the ramdisk) and `restore-ramdisk.im4p` (the
//! ramdisk, loaded on its own); without it the test is skipped. The bytes are
//! the user's own IPSW, so they live outside the tree.

use ambrosia::bundle::Bundle;
use ambrosia::img4::Packing;
use ambrosia::restore::{self, Input};

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
