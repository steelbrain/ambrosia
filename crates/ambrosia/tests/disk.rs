//! Reads a real guest disk and checks what it reads against what macOS
//! reads from the same disk.
//!
//! Set `AMBROSIA_TEST_DISK` to a raw disk image of a guest, and
//! `AMBROSIA_TEST_AUX` to its aux image for the test that reads every boot
//! object. Without the variables, the tests say so and pass.
//!
//! The expected partitions, volumes and boot files are in a fixture,
//! `tests/fixtures/<image name without extension>.txt`, recorded with macOS
//! from a read-only attach of a clone of the disk: the partitions from IOKit
//! and `diskutil info`, the containers and volumes from `diskutil apfs list
//! -plist`, and the boot files' sizes and SHA-256 from a read-only mount of
//! the Preboot volume, under `/<group>/boot/<boot>/`, and of the iSCPreboot
//! volume for the LocalPolicy and SFR manifest. Blank lines and lines that
//! start with `#` are skipped; the others are:
//!
//! ```text
//! partition <first LBA> <last LBA> <type GUID> <unique GUID> <name>
//! container <first LBA> <UUID>
//! volume <container's first LBA> <role> <UUID> <name>
//! group <the System volume's volume group UUID>
//! boot <the boot directory's name>
//! file <size> <SHA-256> <path in the boot directory>
//! isc-file <size> <SHA-256> <path in iSCPreboot>
//! ```

use ambrosia::apfs::{Container, Kind, Role, Volume};
use ambrosia::block::{BlockDevice, SECTOR};
use ambrosia::gpt::{APFS_TYPE, Gpt};
use ambrosia::{files, img4, nvram};
use sha2::{Digest, Sha256};
use std::cell::Cell;
use std::fs::File;
use std::os::unix::fs::FileExt;
use std::path::Path;
use std::rc::Rc;
use std::time::Instant;

/// A disk image file that counts the requests made of it and their bytes.
struct ImageDisk {
    file: File,
    /// What every handle on the image has read so far.
    stats: Rc<Cell<Stats>>,
}

/// The requests made of a disk, and the bytes they read.
#[derive(Clone, Copy, Default)]
struct Stats {
    requests: u64,
    bytes: u64,
}

impl BlockDevice for ImageDisk {
    type Error = std::io::Error;

    fn read(&mut self, sector: u64, buf: &mut [u8]) -> Result<(), std::io::Error> {
        let Stats { requests, bytes } = self.stats.get();
        self.stats.set(Stats {
            requests: requests + 1,
            bytes: bytes + buf.len() as u64,
        });
        let at = sector
            .checked_mul(SECTOR as u64)
            .ok_or(std::io::ErrorKind::InvalidInput)?;
        self.file.read_exact_at(buf, at)
    }
}

/// What macOS reads from a disk, as its fixture records it. The module's
/// documentation gives each kind of line's fields.
#[derive(Default)]
struct Fixture {
    /// The `partition` lines, each without its first word.
    partitions: Vec<String>,
    /// The `container` lines, each without its first word.
    containers: Vec<String>,
    /// The `volume` lines, each without its first word.
    volumes: Vec<String>,
    /// The System volume's volume group UUID.
    group: String,
    /// The boot directory's name.
    boot: String,
    /// The `file` lines: files in the boot directory.
    files: Vec<FixtureFile>,
    /// The `isc-file` lines: files in iSCPreboot.
    isc_files: Vec<FixtureFile>,
}

/// A file a fixture records.
struct FixtureFile {
    /// Its size in bytes.
    size: u64,
    /// Its SHA-256, in lowercase hex.
    sha256: String,
    /// Its path in the boot directory, or in iSCPreboot.
    path: String,
}

impl FixtureFile {
    /// Checks that `data` has the file's size and SHA-256.
    fn check(&self, data: &[u8]) {
        assert_eq!(
            (data.len() as u64, hex(&Sha256::digest(data))),
            (self.size, self.sha256.clone()),
            "{}",
            self.path
        );
    }
}

impl Fixture {
    /// Parses a fixture. A line of a kind it doesn't know fails the test.
    fn parse(text: &str) -> Fixture {
        let mut f = Fixture::default();
        for line in text
            .lines()
            .filter(|l| !l.is_empty() && !l.starts_with('#'))
        {
            let (kind, rest) = line.split_once(' ').unwrap();
            match kind {
                "partition" => f.partitions.push(rest.into()),
                "container" => f.containers.push(rest.into()),
                "volume" => f.volumes.push(rest.into()),
                "group" => f.group = rest.into(),
                "boot" => f.boot = rest.into(),
                "file" | "isc-file" => {
                    let mut it = rest.splitn(3, ' ');
                    let file = FixtureFile {
                        size: it.next().unwrap().parse().unwrap(),
                        sha256: it.next().unwrap().into(),
                        path: it.next().unwrap().into(),
                    };
                    let list = if kind == "file" {
                        &mut f.files
                    } else {
                        &mut f.isc_files
                    };
                    list.push(file);
                }
                _ => panic!("fixture line {line:?}"),
            }
        }
        f
    }
}

/// The fixture for the disk image `image`.
fn fixture(image: &Path) -> Fixture {
    let stem = image.file_stem().unwrap().to_str().unwrap();
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(format!("{stem}.txt"));
    let text = std::fs::read_to_string(&fixture)
        .unwrap_or_else(|e| panic!("{}: {e}; record one for this disk", fixture.display()));
    Fixture::parse(&text)
}

/// The fixture's record of the file at `path`.
fn expect<'a>(list: &'a [FixtureFile], path: &str) -> &'a FixtureFile {
    list.iter()
        .find(|f| f.path == path)
        .unwrap_or_else(|| panic!("the fixture has no {path}"))
}

/// Reads every boot object as the firmware does, with `files::read_all`,
/// and checks each against the fixture.
#[test]
fn boot_files() {
    let (Some(image), Some(aux)) = (
        std::env::var_os("AMBROSIA_TEST_DISK"),
        std::env::var_os("AMBROSIA_TEST_AUX"),
    ) else {
        eprintln!(
            "skipping: set AMBROSIA_TEST_DISK and AMBROSIA_TEST_AUX to read the boot objects"
        );
        return;
    };
    let want = fixture(Path::new(&image));
    let stats = Rc::new(Cell::new(Stats::default()));
    let open = |p: &std::ffi::OsStr| ImageDisk {
        file: File::open(p).unwrap(),
        stats: stats.clone(),
    };
    let (mut root, mut aux) = (open(&image), open(&aux));
    let mut lines = Vec::new();
    let start = Instant::now();
    let found = files::read_all(&mut root, &mut aux, &mut |a| lines.push(a.to_string())).unwrap();
    let Stats { requests, bytes } = stats.get();
    println!(
        "read the boot objects in {:.1} ms, {requests} disk requests of {bytes} bytes",
        start.elapsed().as_secs_f64() * 1e3
    );
    assert_eq!(found.volume_group, want.group);
    assert_eq!(found.boot_dir, want.boot);
    assert_eq!(
        lines,
        [format!("macOS {}, boot objects {}", want.group, want.boot)]
    );

    let firmware = "usr/standalone/firmware";
    for (path, data) in [
        ("System/Library/Caches/apticket.der", &found.manifest),
        (
            "System/Library/Caches/com.apple.kernelcaches/kernelcache",
            &found.kernel,
        ),
        (&format!("{firmware}/devicetree.img4"), &found.device_tree),
        (&format!("{firmware}/root_hash.img4"), &found.root_hash),
        (
            &format!("{firmware}/base_system_root_hash.img4"),
            &found.base_root_hash,
        ),
        (
            &format!("{firmware}/FUD/StaticTrustCache.img4"),
            &found.static_trust_cache,
        ),
        (
            &format!("{firmware}/FUD/BaseSystemTrustCache.img4"),
            &found.base_trust_cache,
        ),
    ] {
        expect(&want.files, path).check(data);
    }
    // The policy is named for a hash, so the fixture's is found by its
    // directory.
    let policy = want
        .isc_files
        .iter()
        .find(|f| f.path.contains("/LocalPolicy/"))
        .unwrap();
    policy.check(&found.policy);
    assert!(img4::parse_img4(&found.policy).unwrap().manifest.is_some());
    expect(&want.isc_files, "SFR/current/apticket.der").check(&found.sfr_manifest);
    // NVRAM changes as the guest runs, so it is checked for a valid bank.
    assert_eq!(found.nvram_region.len(), 1 << 20);
    assert!(nvram::current(&found.nvram_region, 2).is_some());
}

/// Reads the partition table and every container and volume, lists the boot
/// directory, and reads the kernel collection, its manifest, and every image
/// and trust cache under `usr/standalone/firmware`, checking each against the
/// fixture. It reads the kernel collection again in ranges off sector and
/// block boundaries, and prints what it reads, with each file's time and
/// disk requests.
#[test]
fn guest_disk() {
    let Some(image) = std::env::var_os("AMBROSIA_TEST_DISK") else {
        eprintln!("skipping: set AMBROSIA_TEST_DISK to a raw guest disk image to read one");
        return;
    };
    let image = Path::new(&image);
    let want = fixture(image);

    let file = File::open(image).unwrap();
    let stats = Rc::new(Cell::new(Stats::default()));
    let disk = || ImageDisk {
        file: file.try_clone().unwrap(),
        stats: stats.clone(),
    };

    let gpt = Gpt::read(&mut disk()).unwrap();
    println!("disk {}", gpt.disk_guid);
    let partitions: Vec<String> = gpt
        .partitions
        .iter()
        .map(|p| {
            let line = format!(
                "{} {} {} {} {}",
                p.first_lba, p.last_lba, p.type_guid, p.unique_guid, p.name
            );
            println!("partition {line}");
            line
        })
        .collect();
    assert_eq!(partitions, want.partitions);

    let (mut containers, mut volumes) = (Vec::new(), Vec::new());
    for p in &gpt.partitions {
        let mut c = Container::open(disk(), p.first_lba).unwrap();
        println!(
            "container {} {}: block size {}, xid {:#x}",
            p.name,
            c.uuid(),
            c.block_size(),
            c.xid()
        );
        containers.push(format!("{} {}", p.first_lba, c.uuid()));
        for (i, v) in c.volumes().to_vec().iter().enumerate() {
            let opens = match c.volume(i) {
                Ok(_) => "opens".into(),
                Err(e) => e.to_string(),
            };
            let group = v.group.map(|g| format!(", group {g}")).unwrap_or_default();
            println!(
                "  volume {:?} {} {}{group}: {opens}",
                v.name, v.role, v.uuid
            );
            volumes.push((p.first_lba, v.role, v.uuid.to_string(), v.name.clone()));
        }
    }
    assert_eq!(containers, want.containers);
    // diskutil spells roles its own way (xART, VM), so roles compare
    // without case.
    assert_eq!(volumes.len(), want.volumes.len());
    for ((lba, role, uuid, name), line) in volumes.iter().zip(&want.volumes) {
        let fields: Vec<&str> = line.splitn(4, ' ').collect();
        assert_eq!(fields[0], lba.to_string(), "{line}");
        assert!(
            fields[1].eq_ignore_ascii_case(&format!("{role:?}")),
            "{line}: {role:?}"
        );
        assert_eq!((fields[2], fields[3]), (uuid.as_str(), name.as_str()));
    }

    // The boot files are in the main container's Preboot volume, under the
    // System volume's group.
    let main = gpt
        .partitions
        .iter()
        .find(|p| p.type_guid.to_string() == APFS_TYPE)
        .unwrap();
    let before = stats.get();
    let mut c = Container::open(disk(), main.first_lba).unwrap();
    let after = stats.get();
    println!(
        "opened {} in {} disk requests of {} bytes",
        main.name,
        after.requests - before.requests,
        after.bytes - before.bytes
    );
    let group = c
        .volumes()
        .iter()
        .find(|v| v.role == Role::System)
        .and_then(|v| v.group)
        .unwrap()
        .to_string();
    assert_eq!(group, want.group);
    let preboot = c
        .volumes()
        .iter()
        .position(|v| v.role == Role::Preboot)
        .unwrap();
    let mut vol = c.volume(preboot).unwrap();

    let boot = format!("/{group}/boot");
    list(&mut vol, &boot);
    let active = vol.lookup(&format!("{boot}/active")).unwrap();
    let name = String::from_utf8(vol.read(&active).unwrap()).unwrap();
    let name = name.trim_end().to_string();
    assert_eq!(name, want.boot);
    let dir = format!("{boot}/{name}");
    for entry in list(&mut vol, &dir) {
        if entry.1 == Kind::Dir {
            list(&mut vol, &format!("{dir}/{}", entry.0));
        }
    }

    // The kernel collection, its manifest, the device tree, and every image
    // and trust cache under usr/standalone/firmware.
    let mut files = vec![
        "System/Library/Caches/com.apple.kernelcaches/kernelcache".to_string(),
        "System/Library/Caches/apticket.der".to_string(),
        "usr/standalone/firmware/devicetree.img4".to_string(),
    ];
    let mut dirs = vec!["usr/standalone/firmware".to_string()];
    while let Some(sub) = dirs.pop() {
        let inode = vol.lookup(&format!("{dir}/{sub}")).unwrap();
        for e in vol.read_dir(&inode).unwrap() {
            let path = format!("{sub}/{}", e.name);
            let lower = e.name.to_ascii_lowercase();
            match e.kind {
                Kind::Dir => dirs.push(path),
                _ if lower.ends_with(".img4")
                    || lower.ends_with(".im4p")
                    || lower.contains("trustcache") =>
                {
                    files.push(path);
                }
                _ => {}
            }
        }
    }
    files.sort();
    files.dedup();
    let mut expected: Vec<&String> = want.files.iter().map(|f| &f.path).collect();
    expected.sort();
    assert_eq!(files.iter().collect::<Vec<_>>(), expected);

    for FixtureFile { size, sha256, path } in &want.files {
        let inode = vol.lookup(&format!("{dir}/{path}")).unwrap();
        assert_eq!(inode.kind, Kind::File, "{path}");
        assert!(!inode.is_compressed(), "{path} is compressed");
        assert_eq!(inode.size, *size, "{path}");
        let mut data = vec![0u8; inode.size as usize];
        let before = stats.get();
        let start = Instant::now();
        assert_eq!(vol.read_at(&inode, 0, &mut data).unwrap(), data.len());
        let took = start.elapsed();
        let after = stats.get();
        assert_eq!(hex(&Sha256::digest(&data)), *sha256, "{path}");
        println!(
            "read {path}: {size} bytes in {:.1} ms, {} disk requests of {} bytes",
            took.as_secs_f64() * 1e3,
            after.requests - before.requests,
            after.bytes - before.bytes
        );
        if path.ends_with("/kernelcache") {
            check_ranges(&mut vol, &inode, &data);
        }
    }
}

/// Lists a directory, prints it, and returns each entry's name and kind.
fn list(vol: &mut Volume<'_, ImageDisk>, path: &str) -> Vec<(String, Kind)> {
    let inode = vol.lookup(path).unwrap();
    assert_eq!(inode.kind, Kind::Dir, "{path}");
    println!("{path}/");
    vol.read_dir(&inode)
        .unwrap()
        .into_iter()
        .map(|e| {
            println!("  {} {:?}", e.name, e.kind);
            (e.name, e.kind)
        })
        .collect()
}

/// Reads ranges of `file` that start and end off sector and block
/// boundaries, and past its end, and checks them against its whole data.
fn check_ranges(vol: &mut Volume<'_, ImageDisk>, file: &ambrosia::apfs::Inode, data: &[u8]) {
    let size = data.len() as u64;
    let offsets = [
        0,
        1,
        511,
        512,
        4095,
        4096,
        4097,
        (1 << 20) + 7,
        size - 5000,
        size - 1,
    ];
    for &offset in &offsets {
        for len in [1usize, 7, 512, 513, 3 * 4096 + 5, 1 << 20] {
            let mut buf = vec![0xa5u8; len];
            let n = vol.read_at(file, offset, &mut buf).unwrap();
            let end = (offset as usize + len).min(data.len());
            assert_eq!(n, end - offset as usize, "read at {offset} for {len}");
            assert!(
                buf[..n] == data[offset as usize..end],
                "read at {offset} for {len}"
            );
        }
    }
    let mut buf = [0u8; 16];
    assert_eq!(vol.read_at(file, size, &mut buf).unwrap(), 0);
    assert_eq!(vol.read_at(file, size + 100, &mut buf).unwrap(), 0);
}

/// `bytes` in lowercase hex.
fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
