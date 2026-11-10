//! Bundles built from files on disk, read back with the firmware's readers as
//! the firmware reads them, and each error `create` and `check` give.
//!
//! With AMBROSIA_TEST_DRIVER set to a driver module's ELF file, such as the
//! one `make bochs-display` builds as target/bochs-display.elf, one test
//! builds the example's bundle with it; without it that test is skipped.

use ambrosia::bundle::{self, Bundle};
use ambrosia::config::Config;
use ambrosia::{boot, elf, kext};
use ambrosia_bundle::newc::{self, Entry};
use ambrosia_bundle::{Error, Inputs, check, create};
use std::ffi::OsStr;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

/// A directory for one test's files, removed when the test ends.
struct Dir(PathBuf);

impl Dir {
    fn new(name: &str) -> Dir {
        let path = Path::new(env!("CARGO_TARGET_TMPDIR")).join(name);
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).unwrap();
        Dir(path)
    }

    /// Writes `data` at `rel`, with its parent directories, and gives its path.
    fn write(&self, rel: &str, data: &[u8]) -> PathBuf {
        let path = self.0.join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, data).unwrap();
        path
    }

    fn path(&self, rel: &str) -> PathBuf {
        self.0.join(rel)
    }
}

impl Drop for Dir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn put32(d: &mut [u8], at: usize, v: u32) {
    d[at..at + 4].copy_from_slice(&v.to_le_bytes());
}

fn put64(d: &mut [u8], at: usize, v: u64) {
    d[at..at + 8].copy_from_slice(&v.to_le_bytes());
}

/// Where [`kext_executable`]'s linkedit data starts, after its `__TEXT`.
const LINKEDIT: usize = 0x200;

/// The smallest executable the firmware's kext reader takes and lays out: an
/// arm64e `MH_KEXT_BUNDLE` with a `__TEXT` segment, a symbol table holding
/// `_kmod_info`, and chained fixups with no starts and no imports.
fn kext_executable() -> Vec<u8> {
    let le = LINKEDIT;
    let (sym, strs) = (le + 0x28, le + 0x38);
    let strtab = b"\0_kmod_info\0";
    let mut d = vec![0; strs + strtab.len()];
    put32(&mut d, 0, 0xfeed_facf); // MH_MAGIC_64
    put32(&mut d, 4, 0x0100_000c); // CPU_TYPE_ARM64
    put32(&mut d, 8, 0x8000_0002); // CPU_SUBTYPE_ARM64E, with the ABI bit
    put32(&mut d, 12, 11); // MH_KEXT_BUNDLE
    put32(&mut d, 16, 3); // ncmds
    put32(&mut d, 20, 72 + 24 + 16); // sizeofcmds
    // LC_SEGMENT_64 __TEXT: the first 0x200 bytes, read and execute.
    put32(&mut d, 32, 0x19);
    put32(&mut d, 36, 72);
    d[40..46].copy_from_slice(b"__TEXT");
    put64(&mut d, 56, 0); // vmaddr
    put64(&mut d, 64, le as u64); // vmsize
    put64(&mut d, 72, 0); // fileoff
    put64(&mut d, 80, le as u64); // filesize
    put32(&mut d, 88, 5); // maxprot
    put32(&mut d, 92, 5); // initprot
    // LC_SYMTAB: one symbol.
    put32(&mut d, 104, 0x2);
    put32(&mut d, 108, 24);
    put32(&mut d, 112, sym as u32);
    put32(&mut d, 116, 1);
    put32(&mut d, 120, strs as u32);
    put32(&mut d, 124, strtab.len() as u32);
    // LC_DYLD_CHAINED_FIXUPS.
    put32(&mut d, 128, 0x8000_0034);
    put32(&mut d, 132, 16);
    put32(&mut d, 136, le as u32);
    put32(&mut d, 140, 0x28);
    // The fixups' header: starts at +0x20, for one segment with none.
    put32(&mut d, le + 4, 0x20);
    put32(&mut d, le + 8, 0x1c);
    put32(&mut d, le + 12, 0x1c);
    put32(&mut d, le + 0x20, 1);
    // _kmod_info, in __TEXT, which holds the fields of it the firmware writes.
    put32(&mut d, sym, 1);
    d[sym + 4] = 0x0f; // N_SECT | N_EXT
    d[sym + 5] = 1;
    put64(&mut d, sym + 8, 0x80);
    d[strs..].copy_from_slice(strtab);
    d
}

/// The smallest module the firmware's loader takes: an aarch64 `ET_DYN`
/// with one executable `PT_LOAD` that holds its entry point.
fn driver_module() -> Vec<u8> {
    let mut d = vec![0; 128];
    d[..7].copy_from_slice(b"\x7fELF\x02\x01\x01");
    d[16..18].copy_from_slice(&3u16.to_le_bytes()); // ET_DYN
    d[18..20].copy_from_slice(&183u16.to_le_bytes()); // EM_AARCH64
    put32(&mut d, 20, 1);
    put64(&mut d, 24, 0x78); // e_entry
    put64(&mut d, 32, 64); // e_phoff
    d[52..54].copy_from_slice(&64u16.to_le_bytes());
    d[54..56].copy_from_slice(&56u16.to_le_bytes());
    d[56..58].copy_from_slice(&1u16.to_le_bytes());
    put32(&mut d, 64, 1); // PT_LOAD
    put32(&mut d, 68, 5); // read and execute
    put64(&mut d, 64 + 32, 128); // p_filesz
    put64(&mut d, 64 + 40, 128); // p_memsz
    put64(&mut d, 64 + 48, 0x1000); // p_align
    d
}

fn info_plist(id: &str, executable: Option<&str>) -> Vec<u8> {
    let exe = executable.map_or(String::new(), |e| {
        format!("<key>CFBundleExecutable</key><string>{e}</string>")
    });
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<plist version=\"1.0\"><dict>\
         <key>CFBundleIdentifier</key><string>{id}</string>{exe}</dict></plist>\n"
    )
    .into_bytes()
}

/// A config.plist whose `Kernel.Add` names `kexts` and whose driver is
/// `driver`.
fn config_plist(kexts: &[&str], driver: Option<&str>) -> Vec<u8> {
    let add: String = kexts
        .iter()
        .map(|k| format!("<dict><key>BundlePath</key><string>{k}</string></dict>"))
        .collect();
    let graphics = driver.map_or(String::new(), |d| {
        format!("<key>Graphics</key><dict><key>Driver</key><string>{d}</string></dict>")
    });
    format!(
        "<plist version=\"1.0\"><dict><key>Kernel</key><dict><key>Add</key><array>{add}\
         </array></dict>{graphics}</dict></plist>"
    )
    .into_bytes()
}

/// Writes a kext bundle at `rel`: its Info.plist and its executable.
fn write_kext(dir: &Dir, rel: &str, id: &str, exe: &[u8]) -> PathBuf {
    dir.write(
        &format!("{rel}/Contents/Info.plist"),
        &info_plist(id, Some("K")),
    );
    let path = dir.write(&format!("{rel}/Contents/MacOS/K"), exe);
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    dir.path(rel)
}

/// The inputs of a bundle with one kext, `Kexts/A.kext`, and one driver,
/// `Drivers/d.elf`, written into `dir`.
fn inputs(dir: &Dir) -> Inputs {
    Inputs {
        config: dir.write(
            "config.plist",
            &config_plist(&["Kexts/A.kext"], Some("Drivers/d.elf")),
        ),
        kexts: vec![write_kext(
            dir,
            "in/A.kext",
            "ai.aneesiqbal.ambrosia.test.a",
            &kext_executable(),
        )],
        drivers: vec![dir.write("in/d.elf", &driver_module())],
    }
}

#[test]
fn the_firmware_reads_what_create_writes() {
    let dir = Dir::new("reads");
    let inputs = inputs(&dir);
    let (archive, summary) = create(&inputs).unwrap();

    // The firmware finds the bundle at the start of RAM it doesn't know the
    // end of, so give it what follows too.
    let mut ram = archive.clone();
    ram.extend([0xaa; 4096]);
    let b = Bundle::find(&ram).unwrap().unwrap();
    assert_eq!(b.len, archive.len());
    let paths: Vec<&str> = b.files.iter().map(|f| f.path).collect();
    assert_eq!(
        paths,
        [
            "config.plist",
            "Drivers/d.elf",
            "Kexts/A.kext/Contents/Info.plist",
            "Kexts/A.kext/Contents/MacOS/K",
        ]
    );
    let c = Config::read(&b).unwrap();
    assert_eq!(c.kexts.len(), 1);
    assert_eq!(c.kexts[0].path, "Kexts/A.kext");
    assert_eq!(c.kexts[0].id, "ai.aneesiqbal.ambrosia.test.a");
    let exe = c.kexts[0].executable.unwrap();
    assert_eq!(exe, kext_executable());
    kext::Kext::parse(exe).unwrap();
    let d = c.driver.unwrap();
    assert_eq!(d.path, "Drivers/d.elf");
    elf::Module::parse(d.elf).unwrap();

    assert_eq!(summary.len, archive.len());
    assert_eq!(summary.files, 4);
    assert_eq!(summary.kexts[0].id, "ai.aneesiqbal.ambrosia.test.a");
    assert_eq!(summary.driver.as_deref(), Some("Drivers/d.elf"));
    assert_eq!(check(&archive).unwrap(), summary);
}

#[test]
fn the_archive_holds_directories_with_fixed_metadata() {
    let dir = Dir::new("metadata");
    let (archive, _) = create(&inputs(&dir)).unwrap();
    // Each member: its path, mode, uid, gid and mtime, from the header.
    let mut members = Vec::new();
    let mut at = 0;
    loop {
        let h = std::str::from_utf8(&archive[at..at + 110]).unwrap();
        let field = |i: usize| u32::from_str_radix(&h[6 + 8 * i..14 + 8 * i], 16).unwrap();
        let (size, name) = (field(6) as usize, field(11) as usize);
        let path = std::str::from_utf8(&archive[at + 110..at + 109 + name]).unwrap();
        if path == "TRAILER!!!" {
            break;
        }
        assert_eq!((field(2), field(3), field(5)), (0, 0, 0), "{path}");
        members.push((path.to_owned(), field(1)));
        at = (at + 110 + name).next_multiple_of(4);
        at = (at + size).next_multiple_of(4);
    }
    let want = [
        ("config.plist", 0o100644),
        ("Drivers", 0o40755),
        ("Drivers/d.elf", 0o100644),
        ("Kexts", 0o40755),
        ("Kexts/A.kext", 0o40755),
        ("Kexts/A.kext/Contents", 0o40755),
        ("Kexts/A.kext/Contents/Info.plist", 0o100644),
        ("Kexts/A.kext/Contents/MacOS", 0o40755),
        ("Kexts/A.kext/Contents/MacOS/K", 0o100755),
    ];
    let want: Vec<(String, u32)> = want.iter().map(|(p, m)| (p.to_string(), *m)).collect();
    assert_eq!(members, want);
}

#[test]
fn the_same_inputs_give_the_same_bytes() {
    let one = Dir::new("same-1");
    let (a, _) = create(&inputs(&one)).unwrap();
    // The same files, written later, in another order, with other
    // permissions: the same bundle.
    let two = Dir::new("same-2");
    let drivers = vec![two.write("elsewhere/d.elf", &driver_module())];
    fs::set_permissions(&drivers[0], fs::Permissions::from_mode(0o600)).unwrap();
    two.write("k/A.kext/Contents/MacOS/K", &kext_executable());
    fs::set_permissions(
        two.path("k/A.kext/Contents/MacOS/K"),
        fs::Permissions::from_mode(0o700),
    )
    .unwrap();
    two.write(
        "k/A.kext/Contents/Info.plist",
        &info_plist("ai.aneesiqbal.ambrosia.test.a", Some("K")),
    );
    let config = two.write(
        "renamed.plist",
        &config_plist(&["Kexts/A.kext"], Some("Drivers/d.elf")),
    );
    let (b, _) = create(&Inputs {
        config,
        kexts: vec![two.path("k/A.kext/")],
        drivers,
    })
    .unwrap();
    assert!(a == b, "the two bundles differ");
}

#[test]
fn the_example_bundle() {
    let Ok(driver) = std::env::var("AMBROSIA_TEST_DRIVER") else {
        eprintln!("AMBROSIA_TEST_DRIVER is unset; skipping");
        return;
    };
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let (archive, summary) = create(&Inputs {
        config: root.join("drivers/bochs-display/config.plist"),
        kexts: Vec::new(),
        drivers: vec![PathBuf::from(&driver)],
    })
    .unwrap();
    assert_eq!(summary.driver.as_deref(), Some("Drivers/bochs-display.elf"));
    let b = Bundle::parse(&archive).unwrap();
    let d = Config::read(&b).unwrap().driver.unwrap();
    assert_eq!(d.elf, fs::read(driver).unwrap());
}

#[test]
fn a_disabled_kext_may_be_left_out() {
    let dir = Dir::new("disabled");
    let config = dir.write(
        "config.plist",
        b"<dict><key>Kernel</key><dict><key>Add</key><array><dict>\
          <key>BundlePath</key><string>Kexts/Off.kext</string>\
          <key>Enabled</key><false/></dict></array></dict></dict>",
    );
    let (_, s) = create(&Inputs {
        config: config.clone(),
        ..Inputs::default()
    })
    .unwrap();
    assert!(s.kexts.is_empty());
    // And given, it is named, so it may go in too.
    let kexts = vec![write_kext(
        &dir,
        "Off.kext",
        "ai.aneesiqbal.ambrosia.test.off",
        b"",
    )];
    let (_, s) = create(&Inputs {
        config,
        kexts,
        drivers: Vec::new(),
    })
    .unwrap();
    assert!(s.kexts.is_empty());
}

#[test]
fn a_plugin_of_a_named_kext_is_named() {
    let dir = Dir::new("plugin");
    let exe = kext_executable();
    let a = write_kext(&dir, "A.kext", "ai.aneesiqbal.ambrosia.test.a", &exe);
    write_kext(
        &dir,
        "A.kext/Contents/PlugIns/B.kext",
        "ai.aneesiqbal.ambrosia.test.b",
        &exe,
    );
    let config = dir.write(
        "config.plist",
        &config_plist(&["Kexts/A.kext/Contents/PlugIns/B.kext"], None),
    );
    let (_, s) = create(&Inputs {
        config,
        kexts: vec![a],
        drivers: Vec::new(),
    })
    .unwrap();
    assert_eq!(s.kexts[0].id, "ai.aneesiqbal.ambrosia.test.b");
}

/// The error `create` gives for a bundle whose config.plist is `config`, with
/// the kext and driver of [`inputs`], after `change` has its way with them.
fn create_error(name: &str, config: &[u8], change: impl FnOnce(&Dir, &mut Inputs)) -> Error {
    let dir = Dir::new(name);
    let mut i = inputs(&dir);
    fs::write(&i.config, config).unwrap();
    change(&dir, &mut i);
    create(&i).unwrap_err()
}

/// A config.plist that names [`inputs`]' kext and driver and has `extra`
/// keys at its top level.
fn config_with(extra: &str) -> Vec<u8> {
    format!(
        "<dict><key>Kernel</key><dict><key>Add</key><array><dict><key>BundlePath</key>\
         <string>Kexts/A.kext</string></dict></array></dict><key>Graphics</key><dict>\
         <key>Driver</key><string>Drivers/d.elf</string>{extra}</dict></dict>"
    )
    .into_bytes()
}

#[test]
fn input_errors() {
    let good = config_with("");
    let e = create_error("no-config", &good, |d, i| i.config = d.path("nothing"));
    assert!(matches!(e, Error::Io { .. }), "{e}");

    let e = create_error("config-dir", &good, |d, i| i.config = d.path("in"));
    assert!(matches!(e, Error::NotAFile(_)), "{e}");

    let e = create_error("not-kext", &good, |d, i| {
        i.kexts
            .push(d.write("in/B.bundle/Contents/Info.plist", b"<dict/>"));
    });
    assert!(matches!(e, Error::NotAKext(_)), "{e}");

    let e = create_error("kext-file", &good, |d, i| {
        i.kexts.push(d.write("file.kext", b""));
    });
    assert!(matches!(e, Error::NotAKext(_)), "{e}");

    let e = create_error("no-info", &good, |d, i| {
        d.write("in/B.kext/Contents/MacOS/B", b"");
        i.kexts.push(d.path("in/B.kext"));
    });
    assert!(matches!(e, Error::NoInfoPlist(_)), "{e}");
    assert!(e.to_string().contains("Contents/Info.plist"), "{e}");

    let e = create_error("link", &good, |d, _| {
        std::os::unix::fs::symlink("K", d.path("in/A.kext/Contents/MacOS/L")).unwrap();
    });
    assert!(matches!(e, Error::NotFileOrDir(_)), "{e}");

    let e = create_error("two-kexts", &good, |d, i| {
        i.kexts.push(write_kext(
            d,
            "other/A.kext",
            "ai.aneesiqbal.ambrosia.test.x",
            &kext_executable(),
        ));
    });
    assert!(
        matches!(&e, Error::Duplicate(p) if p == "Kexts/A.kext"),
        "{e}"
    );

    let e = create_error("two-drivers", &good, |d, i| {
        i.drivers.push(d.write("other/d.elf", b""));
    });
    assert!(
        matches!(&e, Error::Duplicate(p) if p == "Drivers/d.elf"),
        "{e}"
    );

    let e = create_error("too-big", &good, |d, i| {
        let big = d.path("in/big.elf");
        fs::File::create(&big)
            .unwrap()
            .set_len(bundle::MAX_LEN as u64)
            .unwrap();
        i.drivers.push(big);
    });
    assert!(
        matches!(e, Error::TooBig(n) if n > bundle::MAX_LEN as u64),
        "{e}"
    );
    assert!(e.to_string().contains("512 MiB"), "{e}");
}

#[test]
fn an_unnamed_kext_is_refused() {
    let e = create_error(
        "unnamed",
        &config_plist(&[], Some("Drivers/d.elf")),
        |_, _| {},
    );
    assert!(
        matches!(&e, Error::Unnamed(p) if p == "Kexts/A.kext"),
        "{e}"
    );
    assert_eq!(
        e.to_string(),
        "Kexts/A.kext is in the bundle, but config.plist doesn't name it; add an entry for it \
         to Kernel.Add: <dict><key>BundlePath</key><string>Kexts/A.kext</string></dict>"
    );
}

/// Checks that `create` refuses a bundle whose config.plist is `config`, with
/// an error `is` accepts, whose message starts with `message`.
fn refused(name: &str, config: &[u8], is: fn(&Error) -> bool, message: &str) {
    let e = create_error(name, config, |_, _| {});
    assert!(is(&e), "{name}: {e:?}");
    assert!(e.to_string().starts_with(message), "{name}: {e}");
}

#[test]
fn configuration_errors() {
    refused(
        "not-plist",
        b"<dict>",
        |e| matches!(e, Error::Plist(f, _) if f == "config.plist"),
        "config.plist: not a property list the firmware reads",
    );
    refused(
        "not-dict",
        b"<array/>",
        |e| matches!(e, Error::NotADict),
        "config.plist: the top level must be a <dict>",
    );
    refused(
        "boot-args",
        b"<dict><key>BootArgs</key><integer>1</integer></dict>",
        |e| matches!(e, Error::Key { key, .. } if key == "BootArgs"),
        "config.plist: BootArgs is not a string",
    );
    refused(
        "bundle-path",
        b"<dict><key>Kernel</key><dict><key>Add</key><array><dict/></array></dict></dict>",
        |e| matches!(e, Error::Key { key, .. } if key == "Kernel.Add[0].BundlePath"),
        "config.plist: Kernel.Add[0].BundlePath is missing or not a string",
    );
    refused(
        "enabled",
        b"<dict><key>Kernel</key><dict><key>Add</key><array><dict><key>Enabled</key>\
              <string>no</string></dict></array></dict></dict>",
        |e| matches!(e, Error::Key { key, .. } if key == "Kernel.Add[0].Enabled"),
        "config.plist: Kernel.Add[0].Enabled is not a boolean",
    );
    refused(
        "patch",
        b"<dict><key>Kernel</key><dict><key>Patch</key><array><dict><key>Identifier</key>\
              <string>x</string><key>Find</key><data>AQ==</data><key>Replace</key>\
              <data>AQI=</data></dict></array></dict></dict>",
        |e| matches!(e, Error::PatchLength(k) if k == "Kernel.Patch[0]"),
        "config.plist: Kernel.Patch[0]: Find must not be empty",
    );
    refused(
        "display-node",
        b"<dict><key>Graphics</key><dict><key>DisplayNode</key><string>a/b</string>\
              </dict></dict>",
        |e| matches!(e, Error::DisplayNode),
        "config.plist: Graphics.DisplayNode must be a node name",
    );
    refused(
        "no-kext",
        b"<dict><key>Kernel</key><dict><key>Add</key><array><dict><key>BundlePath</key>\
              <string>Kexts/A.kext</string></dict><dict><key>BundlePath</key>\
              <string>Kexts/Gone.kext</string></dict></array></dict></dict>",
        |e| matches!(e, Error::NoKext(p) if p == "Kexts/Gone.kext"),
        "config.plist: Kernel.Add names Kexts/Gone.kext, but the bundle has no \
             Kexts/Gone.kext/Contents/Info.plist; give the kext's directory with --kext",
    );
    refused(
        "no-driver",
        &config_plist(&["Kexts/A.kext"], Some("Drivers/gone.elf")),
        |e| matches!(e, Error::NoDriver(p) if p == "Drivers/gone.elf"),
        "config.plist: Graphics.Driver is Drivers/gone.elf, but the bundle has no such file",
    );
}

#[test]
fn kext_errors() {
    let good = config_with("");
    let info = "in/A.kext/Contents/Info.plist";
    let exe = "in/A.kext/Contents/MacOS/K";
    let e = create_error("bad-info", &good, |d, _| {
        d.write(info, b"<dict><key>CFBundleIdentifier</key>");
    });
    assert!(
        matches!(&e, Error::Plist(f, _) if f == "Kexts/A.kext/Contents/Info.plist"),
        "{e:?}"
    );

    let e = create_error("no-id", &good, |d, _| {
        d.write(
            info,
            b"<dict><key>CFBundleExecutable</key><string>K</string></dict>",
        );
    });
    assert!(
        matches!(&e, Error::Key { file, key }
            if file == "Kexts/A.kext/Contents/Info.plist" && key == "CFBundleIdentifier"),
        "{e:?}"
    );
    assert_eq!(
        e.to_string(),
        "Kexts/A.kext/Contents/Info.plist: CFBundleIdentifier is missing or not a string"
    );

    let e = create_error("no-exe", &good, |d, _| {
        d.write(
            info,
            &info_plist("ai.aneesiqbal.ambrosia.test.a", Some("Gone")),
        );
    });
    assert!(
        matches!(&e, Error::NoExecutable(p) if p == "Kexts/A.kext/Contents/MacOS/Gone"),
        "{e:?}"
    );
    assert!(
        e.to_string()
            .starts_with("Kexts/A.kext/Contents/Info.plist: CFBundleExecutable is Gone"),
        "{e}"
    );

    let at = "Kexts/A.kext/Contents/MacOS/K";
    let change = |name: &str, f: fn(&mut Vec<u8>)| {
        create_error(name, &good, |d, _| {
            let mut data = kext_executable();
            f(&mut data);
            d.write(exe, &data);
        })
    };
    let e = change("fat", |d| d[..4].copy_from_slice(&[0xca, 0xfe, 0xba, 0xbe]));
    assert!(matches!(&e, Error::Universal(p) if p == at), "{e:?}");
    assert!(e.to_string().contains("lipo -thin arm64e"), "{e}");

    let e = change("not-macho", |d| d[0] = 0);
    assert!(matches!(&e, Error::NotMachO(p) if p == at), "{e:?}");

    let e = change("arm64", |d| put32(d, 8, 0));
    assert!(
        matches!(&e, Error::NotArm64e(p, a) if p == at && a == "arm64"),
        "{e:?}"
    );
    assert_eq!(
        e.to_string(),
        "Kexts/A.kext/Contents/MacOS/K: built for arm64, not arm64e; build the kext with \
         -arch arm64e"
    );

    let e = change("x86", |d| put32(d, 4, 0x0100_0007));
    assert!(
        matches!(&e, Error::NotArm64e(_, a) if a == "x86_64"),
        "{e:?}"
    );

    let e = change("not-kext", |d| put32(d, 12, 6)); // MH_DYLIB
    assert!(
        matches!(&e, Error::Kext(p, kext::Error::NotKext) if p == at),
        "{e:?}"
    );

    let e = change("no-fixups", |d| put32(d, 128, 0x8000_0001));
    assert!(matches!(&e, Error::Kext(_, kext::Error::NoFixups)), "{e:?}");
    assert!(e.to_string().contains("-fixup_chains"), "{e}");

    let e = change("bad-segment", |d| put64(d, 80, 0x400)); // filesize > vmsize
    assert!(
        matches!(&e, Error::Kext(_, kext::Error::BadSegment)),
        "{e:?}"
    );

    // One import, in DYLD_CHAINED_IMPORT_ADDEND's format.
    let e = change("import-addend", |d| {
        put32(d, LINKEDIT + 0x10, 1);
        put32(d, LINKEDIT + 0x14, 2);
    });
    assert!(
        matches!(&e, Error::Kext(_, kext::Error::Unsupported(_))),
        "{e:?}"
    );
    assert!(
        e.to_string()
            .contains("chained fixups have imports other than DYLD_CHAINED_IMPORT"),
        "{e}"
    );

    let e = change("no-kmod-info", |d| {
        let n = d.len();
        d[n - 11..n - 1].copy_from_slice(b"_kmod_nope");
    });
    assert!(
        matches!(&e, Error::Kext(_, kext::Error::NoKmodInfo)),
        "{e:?}"
    );
}

/// Kexts the firmware reads but can't lay out in the Auxiliary kernel
/// collection are refused too, as the firmware refuses them before it reads
/// the guest's disk.
#[test]
fn layout_errors() {
    let good = config_with("");
    let at = "Kexts/A.kext/Contents/MacOS/K";
    let change = |name: &str, f: fn(&mut Vec<u8>)| {
        create_error(name, &good, |d, _| {
            let mut data = kext_executable();
            f(&mut data);
            d.write("in/A.kext/Contents/MacOS/K", &data);
        })
    };

    // __TEXT renamed to a segment the collection doesn't hold.
    let e = change("segment", |d| d[40..46].copy_from_slice(b"__KLD\0"));
    assert!(
        matches!(&e, Error::Placement(p, boot::KextError::Segment(s)) if p == at && s == "__KLD"),
        "{e:?}"
    );
    assert_eq!(
        e.to_string(),
        "Kexts/A.kext/Contents/MacOS/K: it has a segment named __KLD; Ambrosia places only \
         __TEXT, __TEXT_EXEC, __DATA_CONST, __OBJC_CONST and __DATA"
    );

    // _kmod_info, whose value is at +8 of the one symbol, 16 bytes before
    // the end of __TEXT, where the fields the firmware writes don't fit.
    let e = change("kmod-info", |d| {
        put64(d, LINKEDIT + 0x30, LINKEDIT as u64 - 0x10);
    });
    assert!(
        matches!(&e, Error::Placement(p, boot::KextError::Place(_)) if p == at),
        "{e:?}"
    );
    assert!(e.to_string().contains("_kmod_info"), "{e}");
}

#[test]
fn driver_errors() {
    let e = create_error("not-elf", &config_with(""), |d, _| {
        d.write("in/d.elf", b"not an ELF file");
    });
    assert!(
        matches!(&e, Error::Driver(p, elf::Error::NotElf) if p == "Drivers/d.elf"),
        "{e:?}"
    );
    assert!(
        e.to_string()
            .starts_with("Drivers/d.elf: not a driver module the firmware loads: not an ELF file"),
        "{e}"
    );
}

#[test]
fn archive_errors() {
    assert!(matches!(check(b"not an archive"), Err(Error::NotABundle)));
    let a = newc::write(&[
        Entry::file("other", false, Vec::new()),
        Entry::file("config.plist", false, b"<dict/>".to_vec()),
    ]);
    let e = check(&a).unwrap_err();
    assert!(
        matches!(e, Error::Archive(bundle::Error::NoConfig)),
        "{e:?}"
    );
    assert!(
        e.to_string().contains("first file is not config.plist"),
        "{e}"
    );

    // config.plist's member: 110 + 13 bytes of header and path, padded to
    // 124, and 7 of data, padded to 132, where the trailer starts.
    let a = newc::write(&[Entry::file("config.plist", false, b"<dict/>".to_vec())]);
    check(&a).unwrap();
    let e = check(&a[..200]).unwrap_err();
    assert!(
        matches!(e, Error::Archive(bundle::Error::Truncated(132))),
        "{e:?}"
    );

    let mut bad = a.clone();
    bad[132] = b'1'; // the trailer's magic
    let e = check(&bad).unwrap_err();
    assert!(
        matches!(e, Error::Archive(bundle::Error::BadHeader(132))),
        "{e:?}"
    );

    // QEMU loads the whole file, and the firmware reads at most MAX_LEN.
    let mut big = vec![0; bundle::MAX_LEN + 1];
    big[..a.len()].copy_from_slice(&a);
    assert!(matches!(check(&big), Err(Error::TooBig(_))));
}

#[test]
fn the_command_line() {
    let dir = Dir::new("cli");
    let i = inputs(&dir);
    let out = dir.path("bundle.cpio");
    let run = |args: &[&OsStr]| {
        Command::new(env!("CARGO_BIN_EXE_ambrosia-bundle"))
            .args(args)
            .output()
            .unwrap()
    };
    let s = OsStr::new;
    let o = run(&[
        s("create"),
        s("--config"),
        i.config.as_os_str(),
        s("--kext"),
        i.kexts[0].as_os_str(),
        s("--driver"),
        i.drivers[0].as_os_str(),
        s("--output"),
        out.as_os_str(),
    ]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let len = fs::metadata(&out).unwrap().len();
    let summary = format!(
        "{}: {len} bytes, 4 files, 1 kext, 0 patches, graphics driver Drivers/d.elf\n  \
         kext ai.aneesiqbal.ambrosia.test.a at Kexts/A.kext\n",
        out.display()
    );
    assert_eq!(String::from_utf8_lossy(&o.stdout), summary);

    let o = run(&[s("check"), out.as_os_str()]);
    assert!(o.status.success());
    assert_eq!(String::from_utf8_lossy(&o.stdout), summary);

    let o = run(&[s("list"), out.as_os_str()]);
    assert!(o.status.success());
    let exe = kext_executable().len();
    let info = info_plist("ai.aneesiqbal.ambrosia.test.a", Some("K")).len();
    let config = fs::metadata(&i.config).unwrap().len();
    let w = [exe, info, config as usize, 128]
        .iter()
        .map(|n| n.to_string().len())
        .max()
        .unwrap();
    assert_eq!(
        String::from_utf8_lossy(&o.stdout),
        format!(
            "{config:>w$}  config.plist\n{:>w$}  Drivers/d.elf\n\
             {info:>w$}  Kexts/A.kext/Contents/Info.plist\n\
             {exe:>w$}  Kexts/A.kext/Contents/MacOS/K\n4 files, {len} bytes\n",
            128
        )
    );

    // A bundle that fails its check is not written.
    let refused = dir.path("refused.cpio");
    fs::write(&i.config, config_plist(&[], Some("Drivers/d.elf"))).unwrap();
    let o = run(&[
        s("create"),
        s("--config"),
        i.config.as_os_str(),
        s("--kext"),
        i.kexts[0].as_os_str(),
        s("--driver"),
        i.drivers[0].as_os_str(),
        s("--output"),
        refused.as_os_str(),
    ]);
    assert_eq!(o.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&o.stderr).starts_with("ambrosia-bundle: Kexts/A.kext"));
    assert!(!refused.exists());

    let usage: [&[&OsStr]; 3] = [&[], &[s("list")], &[s("create"), s("--kext")]];
    for args in usage {
        let o = run(args);
        assert_eq!(o.status.code(), Some(2), "{args:?}");
        assert!(String::from_utf8_lossy(&o.stderr).contains("usage:"));
    }
    let o = run(&[s("--help")]);
    assert!(o.status.success());
    assert!(String::from_utf8_lossy(&o.stdout).starts_with("usage:"));
}
