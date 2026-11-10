//! The device tree the kernel gets: the guest's template, `devicetree.img4`,
//! filled in as iBoot fills it.
//!
//! docs/internals/replacing-iboot.md, "The device tree", says what iBoot adds
//! to the template. Ambrosia writes the same, from the same inputs: the machine's
//! config block, the boot objects and manifests on the guest's disks, NVRAM,
//! and the memory layout. What it leaves out on purpose is said where it
//! happens.

use crate::adt::Node;
use crate::graphics::Framebuffer;
use crate::im4m::{self, Entry, Manifest, Value};
use crate::sha384::{Sha384, sha384};
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::fmt;

/// The machine: its fixed layout, its config block, and the display a
/// graphics driver module set up.
#[derive(Debug, Clone)]
pub struct Machine {
    /// Where RAM starts.
    pub ram_base: u64,
    /// How much RAM there is, in bytes.
    pub ram_size: u64,
    /// The PCIe MMIO window, as its base and size.
    pub pcie_window: (u64, u64),
    /// How many CPUs the machine has.
    pub cpus: u32,
    /// The VM's ECID, its unique chip ID.
    pub ecid: u64,
    /// The random value the VMM draws for the VM.
    pub rnd: u32,
    /// The serial number, `SrNm` in the system configuration.
    pub serial: String,
    /// The model number, `Mod#` in the system configuration.
    pub model: String,
    /// The region, `Regn` in the system configuration.
    pub region: String,
    /// The SoC's name, for `/product/product-soc-name`.
    pub soc_name: String,
    /// The display a graphics driver module set up, if any (`graphics.rs`),
    /// which the kernel's console takes over.
    pub display: Option<Framebuffer>,
    /// The name of the display's node under `/arm-io`, from the
    /// configuration (`Graphics` > `DisplayNode`), written only with a
    /// display.
    pub display_node: Option<String>,
    /// Whether the machine has avp-rtc, the clock macOS keeps its offset to
    /// calendar time with: its ID register reads 0xf001, which is how iBoot
    /// tells.
    pub avp_rtc: bool,
}

/// What Ambrosia read from the guest's disks, besides the kernel.
pub struct Objects<'a> {
    /// The volume group's UUID, as text.
    pub volume_group: &'a str,
    /// The boot directory's name: the boot manifest's SHA-384, in hex.
    pub boot_dir: &'a str,
    /// `apticket.der`, the manifest of every boot object.
    pub manifest: &'a [u8],
    /// The LocalPolicy's manifest.
    pub policy: &'a [u8],
    /// The system firmware's manifest, `SFR/current/apticket.der`.
    pub sfr_manifest: &'a [u8],
    /// The `isys` payload: the root hash of the OS.
    pub root_hash: &'a [u8],
    /// The `csys` payload: the root hash of BaseSystem.
    pub base_root_hash: &'a [u8],
    /// The current NVRAM bank's contents.
    pub nvram: &'a [u8],
    /// The current NVRAM bank's index.
    pub nvram_bank: u32,
    /// How many banks NVRAM has.
    pub nvram_banks: u32,
    /// The size of an NVRAM bank, in bytes.
    pub nvram_bank_size: u32,
}

/// Where things are in memory, as the tree records them. Each range is a
/// physical address and a size in bytes.
#[derive(Debug, Clone, Copy, Default)]
pub struct Placement {
    /// The device tree.
    pub device_tree: (u64, u64),
    /// The trust caches.
    pub trust_cache: (u64, u64),
    /// The boot arguments.
    pub boot_args: (u64, u64),
    /// SEPFW, which is empty.
    pub sepfw: (u64, u64),
    /// The kernel collection's read-only part.
    pub kc_ro: (u64, u64),
    /// The kernel collection's writable part.
    pub kc_rw: (u64, u64),
    /// The kernel collection's `__LINKEDIT`.
    pub kc_le: (u64, u64),
    /// The kernel collection's entry point, as a slid virtual address.
    pub kc_entry: u64,
    /// The kernel collection's base, as a slid virtual address.
    pub kc_virt: u64,
    /// The boot manifest, for `/chosen/boot-object-manifests`.
    pub manifest: (u64, u64),
    /// The LocalPolicy's manifest, for `/chosen/boot-object-manifests`.
    pub policy: (u64, u64),
    /// The system firmware's manifest, for `/chosen/boot-object-manifests`.
    pub sfr_manifest: (u64, u64),
    /// The carveout at the top of RAM that holds the manifests.
    pub manifests_region: (u64, u64),
    /// The panic log's carveout.
    pub pram: (u64, u64),
    /// The framebuffer's carveout.
    pub vram: (u64, u64),
    /// The kernel's memory: from its physical base to the carveouts.
    pub kernel_memory: (u64, u64),
    /// A boot-placed Auxiliary KC, below the Boot KC, advertised as
    /// `/chosen/memory-map/AuxKC-mach_header`. Zero size when none.
    pub aux_kc: (u64, u64),
}

/// Why the template couldn't be filled in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    /// A node the template must have, by its path.
    Missing(&'static str),
    /// The boot manifest or the LocalPolicy's doesn't parse.
    Manifest(im4m::Error),
    /// A root hash payload that isn't the 208-byte form.
    BadRootHash,
    /// arm-io's `ranges` aren't the one range every template has, don't
    /// start below the PCIe window, or run past the end of the address space.
    BadArmIoRanges,
    /// A template directive asks for a value longer than 4 KiB, which no
    /// template's directives do.
    BadDirective,
    /// `/arm-io` already has a child named as the display's node.
    DisplayNodeTaken,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Missing(path) => write!(f, "the device tree template has no {path} node"),
            Error::Manifest(e) => {
                write!(
                    f,
                    "the boot manifest or the LocalPolicy's doesn't parse: {e}"
                )
            }
            Error::BadRootHash => f.write_str("a root hash is not 208 bytes long"),
            Error::BadArmIoRanges => f.write_str(
                "/arm-io's ranges are not one range that starts at or below the PCIe window \
                 and ends in the address space",
            ),
            Error::BadDirective => {
                f.write_str("a directive in the device tree template asks for more than 4 KiB")
            }
            Error::DisplayNodeTaken => {
                f.write_str("/arm-io already has a node named as Graphics.DisplayNode")
            }
        }
    }
}

impl From<im4m::Error> for Error {
    fn from(e: im4m::Error) -> Self {
        Error::Manifest(e)
    }
}

/// The panic log carveout's size, `/chosen/embedded-panic-log-size`.
pub const PRAM_SIZE: u64 = 0x1fc000;

/// The firmware's name, where iBoot gives its version.
pub fn firmware_version() -> String {
    format!("Ambrosia-{}", env!("CARGO_PKG_VERSION"))
}

/// `b` in upper-case hex.
fn hex_upper(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02X}")).collect()
}

/// A range as the tree holds it: its address, then its size, each 64 bits.
fn range(r: (u64, u64)) -> [u8; 16] {
    let mut v = [0u8; 16];
    v[..8].copy_from_slice(&r.0.to_le_bytes());
    v[8..].copy_from_slice(&r.1.to_le_bytes());
    v
}

/// A field of `len` bytes holding `s` and zeroes, `s` cut to leave at least
/// one zero.
fn padded(s: &str, len: usize) -> Vec<u8> {
    let mut v = alloc::vec![0u8; len];
    let n = s.len().min(len.saturating_sub(1));
    v[..n].copy_from_slice(&s.as_bytes()[..n]);
    v
}

/// `n` bytes of the VM's random value, repeated, as iBoot fills its random
/// fields.
fn repeated(rnd: u32, n: usize) -> Vec<u8> {
    rnd.to_le_bytes().iter().copied().cycle().take(n).collect()
}

/// The system configuration keys the config block gives, each with its
/// value.
fn syscfg(m: &Machine) -> [(&'static [u8; 4], &str); 3] {
    [
        (b"SrNm", &m.serial),
        (b"Mod#", &m.model),
        (b"Regn", &m.region),
    ]
}

/// The system configuration iBoot builds from the config block, as
/// `/chosen/syscfg-v2-data`: an `SCfg` header, one `CNTB` record per key,
/// then each value in a 32-byte slot.
fn syscfg_v2(m: &Machine) -> Vec<u8> {
    let keys = syscfg(m);
    let data_at = 0xa4u32;
    let size = data_at + 32 * keys.len() as u32;
    let mut v = Vec::with_capacity(size as usize);
    v.extend(b"gfCS");
    v.extend(size.to_le_bytes());
    v.extend(size.to_le_bytes());
    v.extend([2, 0, 2, 0, 0, 0, 0, 0]);
    v.extend((keys.len() as u32).to_le_bytes());
    for (i, (key, value)) in keys.iter().enumerate() {
        v.extend(b"BTNC");
        let mut k = **key;
        k.reverse();
        v.extend(k);
        v.extend((value.len() as u32).to_le_bytes());
        v.extend((data_at + 32 * i as u32).to_le_bytes());
        v.extend([0; 4]);
    }
    v.resize(data_at as usize, 0);
    for (_, value) in keys {
        v.extend(padded(value, 32));
    }
    v
}

/// The longest value a template directive may ask for. The templates'
/// directives ask for 64 bytes at most, so a longer length is a corrupt
/// template, which Ambrosia would otherwise allocate for.
const DIRECTIVE_MAX: usize = 0x1000;

/// A directive's length, in hex, if it is one.
fn directive_len(s: &str) -> Result<Option<usize>, Error> {
    match usize::from_str_radix(s.trim_start_matches("0x"), 16) {
        Ok(len) if len > DIRECTIVE_MAX => Err(Error::BadDirective),
        Ok(len) => Ok(Some(len)),
        Err(_) => Ok(None),
    }
}

/// A template directive: `syscfg/<key>[/<len>][,zeroes/<len>]`, or
/// `macaddr/<name>,syscfg/<key>/6`. Gives the value to set, or `None` to
/// drop the property, as iBoot drops one it has no value for.
fn directive(spec: &[u8], m: &Machine) -> Result<Option<Vec<u8>>, Error> {
    let spec = spec.split(|&b| b == 0).next().unwrap_or_default();
    let Ok(spec) = core::str::from_utf8(spec) else {
        return Ok(None);
    };
    for part in spec.split(',') {
        let mut f = part.split('/');
        match (f.next(), f.next(), f.next()) {
            (Some("syscfg"), Some(key), len) => {
                let Some((_, value)) = syscfg(m)
                    .into_iter()
                    .find(|(k, _)| k.as_slice() == key.as_bytes())
                else {
                    continue;
                };
                let len = match len {
                    Some(len) => directive_len(len)?,
                    None => None,
                };
                return Ok(Some(padded(value, len.unwrap_or(value.len() + 1))));
            }
            (Some("zeroes"), Some(len), None) => {
                return Ok(directive_len(len)?.map(|len| alloc::vec![0; len]));
            }
            _ => {}
        }
    }
    Ok(None)
}

/// The manifest's own property `code`, or `default` where the manifest has
/// no 32-bit integer for it.
fn manifest_u32(manifest: &Manifest<'_>, code: &[u8; 4], default: u32) -> u32 {
    match manifest.get(code) {
        Some(Value::Int(v)) => u32::try_from(v).unwrap_or(default),
        _ => default,
    }
}

/// A manifest property as iBoot writes it into the tree: a boolean as 32
/// bits, an integer as 64, data and strings as their bytes.
fn manifest_value(v: Value<'_>) -> Vec<u8> {
    match v {
        Value::Bool(b) => u32::from(b).to_le_bytes().to_vec(),
        Value::Int(i) => i.to_le_bytes().to_vec(),
        Value::Data(d) | Value::String(d) => d.to_vec(),
    }
}

/// A LocalPolicy property as iBoot writes it into `/chosen/asmb`: a boolean
/// as one byte, an integer as 64 bits, data as its bytes.
fn policy_value(v: Value<'_>) -> Vec<u8> {
    match v {
        Value::Bool(b) => alloc::vec![u8::from(b)],
        Value::Int(i) => i.to_le_bytes().to_vec(),
        Value::Data(d) | Value::String(d) => d.to_vec(),
    }
}

/// The LocalPolicy properties iBoot passes on as `lp-<code>`. The `sip*`
/// and `smb*` ones carry a policy below Full Security, and are passed on when
/// a policy has them.
const POLICY_KEYS: &[&[u8; 4]] = &[
    b"nsih", b"spih", b"stng", b"lpnh", b"rpnh", b"hrlp", b"sip0", b"sip1", b"sip2", b"sip3",
    b"smb0", b"smb1", b"smb2", b"smb3", b"smb4", b"auxp", b"auxi", b"auxr", b"coih",
];

/// Values iBoot writes into `/chosen` that depend on nothing it reads: this
/// machine's security state with Full Security, and its fixed facts.
const CHOSEN_U32: &[(&str, u32)] = &[
    ("mix-n-match-prevention-status", 1),
    ("secure-boot", 1),
    ("gid-aes-key", 1),
    ("uid-aes-key", 1),
    ("system-trusted", 1),
    ("sepfw-load-at-boot", 0),
    ("certificate-security-mode", 1),
    ("production-cert", 1),
    ("effective-production-status-ap", 1),
    ("effective-security-mode-ap", 1),
    ("certificate-production-status", 1),
    ("recovery-snag-key-pressed", 1),
    ("display-scale", 2),
    ("override-platform-only-code", 0),
    ("boot-command", 1),
    ("internal-use-only-unit", 0),
    ("engineering-use-only-unit", 0),
    ("factory-prerelease-global-trust", 0),
    ("sfr-internal-use-only-sw", 0),
    ("kernel-ctrr-to-be-enabled", 1),
    ("security-domain", 1),
    ("chip-epoch", 1),
    ("use-ddi-secure-boot", 1),
    ("allow-ecid-mismatch", 1),
    ("external-boot", 0),
    ("enable-avp-fairplay", 0),
    ("avp-features", 0),
    ("guest_amx_version", 0),
    ("sepfw-never-boot", 1),
    ("syscfg-late-kbgs-allow-load", 0),
    ("syscfg-late-kbgs-allow-unsealed", 0),
    ("syscfg-late-kbgs-format", 7),
    ("syscfg-late-kbgs-storage", 4),
    ("syscfg-late-kbgs-load-status", 4),
];

/// The system keybag description iBoot gives, as pairs of 32-bit values.
const CHOSEN_U32_PAIRS: &[(&str, [u32; 2])] = &[
    ("syscfg-keybag-ids-providers", [7, 1]),
    ("syscfg-erly-kbgs-allow-load", [1, 1]),
    ("syscfg-erly-kbgs-allow-unsealed", [1, 1]),
    ("syscfg-erly-kbgs-format", [2, 1]),
    ("syscfg-erly-kbgs-storage", [1, 1]),
    ("syscfg-erly-kbgs-load-status", [0, 4]),
];

/// The keybag strings iBoot gives, without a NUL.
const CHOSEN_STR: &[(&str, &str)] = &[
    ("syscfg-keybag-ids", "raVgADDA"),
    ("syscfg-erly-kbgs-data-class", "vpmvADDA"),
    ("syscfg-late-kbgs-data-class", "raVg"),
];

/// The range iBoot gives a memory map entry for something it didn't place:
/// all ones.
const NO_RANGE: [u8; 16] = [0xff; 16];

/// The objects this machine doesn't have, which the memory map lists with
/// [`NO_RANGE`] under their names and each of their parts'.
const ABSENT: &[&str] = &["AuxKC", "TXM", "CL4", "SPTM"];
/// The parts the memory map lists for an object, as `<object>-<part>`.
const PARTS: &[&str] = &[
    "rx",
    "ro",
    "rm",
    "rs",
    "bx",
    "rw",
    "le",
    "entry",
    "virt",
    "exception",
];

/// The boot objects iBoot loads, in its order. `logo` and `lpol` come with
/// their own manifests; the rest come with the boot manifest.
const OBJECTS: &[&str] = &["ibot", "dtre", "isys", "csys", "bstc", "trst", "krnl"];

/// The name of the sealed snapshot the system volume boots from. The root
/// hash payload is u32 {2, 0, 1, 0x20}, then three 64-byte slots, and slot
/// 0's first 32 bytes name the snapshot.
fn root_snapshot(root_hash: &[u8]) -> Result<String, Error> {
    if root_hash.len() != 208 {
        return Err(Error::BadRootHash);
    }
    Ok(format!(
        "com.apple.os.update-{}",
        hex_upper(&root_hash[16..48])
    ))
}

/// The node at `path` from the root of `tree`, which the template must have.
fn find<'a>(tree: &'a mut Node, path: &'static str) -> Result<&'a mut Node, Error> {
    tree.find_mut(path).ok_or(Error::Missing(path))
}

/// A kext Ambrosia placed, for the `/chosen/ambrosia` handover: the identifier
/// injected code looks it up by, and where it landed, as slid virtual
/// addresses that agree with what the kernel's OSKext reports.
pub struct PlacedKext<'a> {
    /// The kext's `CFBundleIdentifier`.
    pub id: &'a str,
    /// The slid virtual address of the kext's sub-header (its `mach_header`).
    pub base: u64,
    /// The kext's vmsize: the span of its placed segments.
    pub size: u64,
}

/// Fills in `tree`, the template, for this machine and these objects. `placed`
/// is the kexts Ambrosia placed, empty for a plain guest, which it records in
/// `/chosen/ambrosia` for injected code to read back from the device tree.
pub fn populate(
    tree: &mut Node,
    m: &Machine,
    o: &Objects<'_>,
    at: &Placement,
    placed: &[PlacedKext<'_>],
    vtables: &[crate::vtdisc::Table],
    seed: &[u8; 48],
) -> Result<(), Error> {
    let manifest = Manifest::parse(o.manifest)?;
    let policy = Manifest::parse(o.policy)?;
    let boot_hash = sha384(o.manifest);

    fill_directives(tree, m)?;
    tree.set("platform-name", &padded("vmapple2", 32));

    chosen(find(tree, "/chosen")?, m, o, &manifest, &boot_hash, seed)?;
    copy_properties(
        find(tree, "/chosen/manifest-properties")?,
        manifest.entry(b"MANP"),
    );
    copy_properties(
        find(tree, "/chosen/manifest-object-properties")?,
        manifest.entry(b"krnl"),
    );
    boot_object_manifests(find(tree, "/chosen/boot-object-manifests")?, at);
    // Only when Ambrosia placed kexts or exported discriminators, so a plain
    // guest's tree is untouched.
    if !placed.is_empty() || !vtables.is_empty() {
        ambrosia(find(tree, "/chosen")?, at.kc_virt, placed, vtables);
    }
    secure_boot_hashes(find(tree, "/chosen/secure-boot-hashes")?, o, &boot_hash);
    local_policy(find(tree, "/chosen/asmb")?, &policy);
    memory_map(find(tree, "/chosen/memory-map")?, at);
    carveouts(find(tree, "/chosen/carveout-memory-map")?, at);

    // Only the CPUs the machine has.
    let cpus = find(tree, "/cpus")?;
    cpus.set_u32("max_cpus", cpus.children.len() as u32);
    cpus.children
        .retain(|c| c.get_u32("cpu-id").is_some_and(|id| id < m.cpus));

    arm_io(find(tree, "/arm-io")?, m)?;
    find(tree, "/pram")?.set("reg", &range(at.pram));
    find(tree, "/vram")?.set("reg", &range(at.vram));
    let product = find(tree, "/product")?;
    product.set_u32("allow-hactivation", 1);
    product.set_str("product-soc-name", &m.soc_name);
    // The recovery overlays apply to a recovery boot, which this isn't.
    tree.remove_child("osenvironments");
    Ok(())
}

/// Fills in or drops each flagged property anywhere in the tree, as its
/// directive says.
fn fill_directives(tree: &mut Node, m: &Machine) -> Result<(), Error> {
    tree.walk_mut(&mut |n| -> Result<(), Error> {
        let flagged = n
            .properties
            .iter()
            .filter(|p| p.flag)
            .map(|p| Ok((p.name.clone(), directive(&p.value, m)?)))
            .collect::<Result<Vec<_>, Error>>()?;
        for (name, value) in flagged {
            match value {
                Some(v) => n.set(&name, &v),
                None => n.remove(&name),
            }
        }
        Ok(())
    })
}

/// `n` bytes from the boot's seed: the SHA-384 of the seed and a counter
/// byte, for the counter at 0, 1 and on, end to end.
fn seed_bytes(seed: &[u8; 48], n: usize) -> Vec<u8> {
    let mut v = Vec::with_capacity(n.next_multiple_of(48));
    let mut counter = 0u8;
    while v.len() < n {
        let mut h = Sha384::new();
        h.update(seed);
        h.update(&[counter]);
        v.extend(h.finalize());
        counter += 1;
    }
    v.truncate(n);
    v
}

/// The properties of `/chosen` itself: the security state, the machine's
/// identity, the random fields, where the boot objects are, NVRAM and the
/// memory, and the `handoff` node.
fn chosen(
    c: &mut Node,
    m: &Machine,
    o: &Objects<'_>,
    manifest: &Manifest<'_>,
    boot_hash: &[u8; 48],
    seed: &[u8; 48],
) -> Result<(), Error> {
    let chip = manifest_u32(manifest, b"CHIP", 0xfe00);
    let board = manifest_u32(manifest, b"BORD", 0x20);
    let version = firmware_version();

    for &(name, v) in CHOSEN_U32 {
        c.set_u32(name, v);
    }
    for &(name, [a, b]) in CHOSEN_U32_PAIRS {
        let mut v = [0u8; 8];
        v[..4].copy_from_slice(&a.to_le_bytes());
        v[4..].copy_from_slice(&b.to_le_bytes());
        c.set(name, &v);
    }
    for &(name, s) in CHOSEN_STR {
        c.set(name, s.as_bytes());
    }
    c.set_str("boot-nonce-hash-method", "sha2-384");
    for i in 0..4 {
        c.set_u64(&format!("restore-security-overrides{i}"), 0);
    }
    c.set_u64("unique-chip-id", m.ecid);
    let mut udid = [0u8; 16];
    udid[..8].copy_from_slice(&m.ecid.to_le_bytes());
    udid[8..12].copy_from_slice(&chip.to_le_bytes());
    c.set("unique-device-id", &udid);
    c.set_u32("chip-id", chip);
    c.set_u32("board-id", board);
    c.set("dram-vendor", &padded("Unknown", 16));
    // bootp-response: iBoot's network boot reply, its first byte a BOOTP
    // reply opcode, the rest empty for a local boot.
    let mut bootp = [0u8; 300];
    bootp[0] = 2;
    c.set("bootp-response", &bootp);
    c.set("firmware-version", &padded(&version, 256));
    c.set_str("system-firmware-version", &version);
    c.set_str("iboot-stage-one-tag", &version);
    c.set_str("iboot-stage-two-tag", &version);

    // The random fields. iBoot fills them with the VM's 32-bit random
    // value repeated; Ambrosia gives the seeds from its own entropy and
    // keeps the boot nonce iBoot's way, since update tickets name it.
    let random = seed_bytes(seed, 448);
    c.set("random-seed", &random[..256]);
    c.set("cl4-entropy", &random[256..]);
    c.set("boot-nonce", &repeated(m.rnd, 8));

    c.set_str("apfs-preboot-uuid", o.volume_group);
    c.set_str("boot-uuid", o.volume_group);
    c.set_str(
        "boot-objects-path",
        &format!("/{}/boot/{}", o.volume_group, o.boot_dir),
    );
    c.set("boot-manifest-hash", boot_hash);
    c.set("system-volume-auth-blob", o.root_hash);
    c.set_str("root-snapshot-name", &root_snapshot(o.root_hash)?);

    c.set_u32("nvram-bank-size", o.nvram_bank_size);
    c.set_u32("nvram-bank-count", o.nvram_banks);
    c.set_u32("nvram-current-bank", o.nvram_bank);
    c.set("nvram-proxy-data", o.nvram);

    c.set_u64("dram-base", m.ram_base);
    c.set_u64("dram-size", m.ram_size);
    c.set_u32("embedded-panic-log-size", PRAM_SIZE as u32);
    c.set("syscfg-v2-data", &syscfg_v2(m));
    c.set("syscfg-keybag-fallback-allowed", &[0, 0]);
    c.set("syscfg-keybag-fallback-used", &[0, 0]);

    if c.child("handoff").is_none() {
        let mut h = Node::default();
        h.set_str("name", "handoff");
        h.set_u32("boot_type", 1);
        c.children.push(h);
    }
    Ok(())
}

/// Copies a manifest entry's properties into `node`, each named by its code.
fn copy_properties(node: &mut Node, entry: Option<&Entry<'_>>) {
    for p in entry.into_iter().flat_map(|e| &e.properties) {
        node.set(
            core::str::from_utf8(&p.code).unwrap_or("????"),
            &manifest_value(p.value),
        );
    }
}

/// `/chosen/boot-object-manifests`: where the manifest of each boot object
/// is in memory.
fn boot_object_manifests(bom: &mut Node, at: &Placement) {
    bom.set("logo", &range(at.sfr_manifest));
    bom.set("lpol", &range(at.policy));
    for name in OBJECTS {
        bom.set(name, &range(at.manifest));
    }
}

/// `/chosen/ambrosia`: Ambrosia's documented handover. `/chosen` survives into
/// the IODeviceTree plane, so injected code reads this at runtime to find each
/// kext Ambrosia placed, by the same slid addresses the kernel's OSKext
/// reports. `boot-kc-base` is the Boot KC's `mach_header`; each child is a
/// placed kext, with its `bundle-id`, `base` and `size`, and `vtable-disc`
/// holds the exported vtable discriminators. Any framework can read it; it
/// names none. Written only when kexts were placed or discriminators
/// exported (docs/internals/handover.md).
fn ambrosia(
    chosen: &mut Node,
    kc_base: u64,
    placed: &[PlacedKext<'_>],
    vtables: &[crate::vtdisc::Table],
) {
    let mut node = Node::default();
    node.set_str("name", "ambrosia");
    node.set_u64("boot-kc-base", kc_base);
    for k in placed {
        let mut child = Node::default();
        child.set_str("name", k.id);
        child.set_str("bundle-id", k.id);
        child.set_u64("base", k.base);
        child.set_u64("size", k.size);
        node.children.push(child);
    }
    // One `vtable-disc` child per donor vtable the configuration asked to
    // export, each holding its slid base and packed per-slot discriminators.
    if !vtables.is_empty() {
        let mut vd = Node::default();
        vd.set_str("name", "vtable-disc");
        for t in vtables {
            let mut child = Node::default();
            child.set_str("name", &t.symbol);
            child.set_str("symbol", &t.symbol);
            child.set_u64("vtable-base", t.base);
            child.set_u32("first-slot", u32::from(t.first_slot));
            child.set("slots", &crate::vtdisc::blob(t));
            vd.children.push(child);
        }
        node.children.push(vd);
    }
    chosen.children.push(node);
}

/// `/chosen/secure-boot-hashes`: the hash of each boot object's manifest,
/// and the root hashes.
fn secure_boot_hashes(sbh: &mut Node, o: &Objects<'_>, boot_hash: &[u8; 48]) {
    let sfr_hash = sha384(o.sfr_manifest);
    sbh.set("sfr-manifest-hash", &sfr_hash);
    sbh.set("logo", &sfr_hash);
    sbh.set("lpol", &sha384(o.policy));
    for name in OBJECTS {
        sbh.set(name, boot_hash);
    }
    sbh.set("system-volume-auth-blob", o.root_hash);
    sbh.set("base-system-volume-auth-blob", o.base_root_hash);
}

/// `/chosen/asmb`: the LocalPolicy properties iBoot passes on.
fn local_policy(asmb: &mut Node, policy: &Manifest<'_>) {
    let Some(p) = policy.entry(b"MANP") else {
        return;
    };
    for key in POLICY_KEYS {
        if let Some(v) = p.get(key) {
            let name = format!("lp-{}", core::str::from_utf8(*key).unwrap());
            asmb.set(&name, &policy_value(v));
        }
    }
}

/// `/chosen/memory-map`: where each boot object is in memory.
fn memory_map(mm: &mut Node, at: &Placement) {
    mm.set("SEPFW", &range(at.sepfw));
    mm.set("TrustCache", &range(at.trust_cache));
    mm.set("DeviceTree", &range(at.device_tree));
    mm.set("BootArgs", &range(at.boot_args));
    // The kernel collection's parts. iBoot gives the parts this layout
    // lacks as empty ranges where the writable part starts.
    let empty = (at.kc_rw.0, 0);
    mm.set("BootKC-rx", &range(empty));
    mm.set("BootKC-ro", &range(at.kc_ro));
    mm.set("BootKC-rm", &NO_RANGE);
    mm.set("BootKC-rs", &range(empty));
    mm.set("BootKC-bx", &range(empty));
    mm.set("BootKC-rw", &range(at.kc_rw));
    mm.set("BootKC-le", &range(at.kc_le));
    mm.set("BootKC-entry", &range((at.kc_entry, 0)));
    mm.set("BootKC-virt", &range((at.kc_virt, 0)));
    mm.set("BootKC-exception", &NO_RANGE);
    let no_parts = |mm: &mut Node, object: &str| {
        for part in PARTS {
            mm.set(&format!("{object}-{part}"), &NO_RANGE);
        }
    };
    for object in ABSENT {
        mm.set(object, &NO_RANGE);
        no_parts(mm, object);
    }
    // The trust caches and the device tree have ranges, set above, but no
    // parts.
    no_parts(mm, "TrustCache");
    no_parts(mm, "DeviceTree");
    mm.set("BootKC", &NO_RANGE);
    mm.set("RTBuddySeg", &NO_RANGE);
    mm.set("CL4-dummypage", &NO_RANGE);
    // The boot-placed AuxKC, when the configuration added a kext. The kernel's
    // early VM bootstrap reads both "AuxKC" (the whole collection, whose size
    // it uses to map the region) and "AuxKC-mach_header" (the header, at the
    // collection's base), translating each base through the static physmap, so
    // each must name a real physical range. The collection's base sits below
    // the Boot KC's lowest segment, as the kernel's guard requires; its split
    // parts stay absent (the kernel walks the segments from the collection
    // itself). A plain guest places no kext, so AuxKC is left absent, exactly
    // as the ABSENT loop set it.
    if at.aux_kc.1 != 0 {
        mm.set("AuxKC", &range(at.aux_kc));
        mm.set("AuxKC-mach_header", &range(at.aux_kc));
    }
}

/// `/chosen/carveout-memory-map`: the carveouts Ambrosia keeps from the
/// kernel, by iBoot's region numbers: its memory, the panic log, the
/// framebuffer and the manifests.
fn carveouts(cmm: &mut Node, at: &Placement) {
    for (id, r) in [
        (24, at.kernel_memory),
        (98, at.pram),
        (14, at.vram),
        (76, at.manifests_region),
    ] {
        cmm.set(&format!("region-id-{id}"), &range(r));
        cmm.set(&format!("region-name-id-{id}"), &[0]);
    }
}

/// `/arm-io`: the devices the machine has, and its PCIe window.
fn arm_io(io: &mut Node, m: &Machine) -> Result<(), Error> {
    // The tree lists the devices the machine has. Ambrosia supports none of
    // Apple's paravirtual devices, so the paravirtual display and its
    // IOSurface mapper go (docs/machine.md). The tree keeps one clock, as
    // iBoot does: avp-rtc, the one macOS keeps its offset to calendar time
    // with, if the machine has it, and otherwise the PL031. Templates before
    // 14's have only the PL031, named rtc, and keep it. The machine has no
    // CTRR device, which Virtualization.framework's has; the kernel then
    // skips the lockdown. avp-sealed-registers stays, though the machine has
    // none: without the node, 27.0.1 doesn't reach userspace's network.
    let other_clock = if m.avp_rtc { "pl031-rtc" } else { "avp-rtc" };
    for name in ["gfx", "iosurface-mapper", other_clock, "avp-ctrr"] {
        io.remove_child(name);
    }
    for name in ["fuse-revision", "tunable-revision", "chip-config-rack"] {
        io.set_u32(name, 0);
    }
    if let (Some(_), Some(name)) = (&m.display, &m.display_node) {
        display_node(io, name)?;
    }
    pcie_window(io, m.pcie_window)
}

/// Adds the node a kernel display driver attaches to, for the display a
/// graphics driver module set up, named `name`. Its `device_type`,
/// `display-subsystem`, is what macOS's IOMobileFramebuffer framework counts
/// among arm-io's children when a process first asks for a display. A process
/// that counts none never matches a framebuffer service, so the node must be
/// in the tree before WindowServer starts, which is before any placed kext
/// could add one (docs/configuration.md, "Graphics > DisplayNode").
fn display_node(io: &mut Node, name: &str) -> Result<(), Error> {
    if io.child(name).is_some() {
        return Err(Error::DisplayNodeTaken);
    }
    let mut node = Node::default();
    node.set_str("name", name);
    node.set_str("device_type", "display-subsystem");
    io.children.push(node);
    Ok(())
}

/// Gives the kernel the machine's PCIe MMIO window, `(base, size)`, in place
/// of the template's, which is Virtualization.framework's.
///
/// The kernel's PCIe driver takes each entry of the PCIe node's `ranges` as
/// a memory window at the same address on the bus as in physical memory, and
/// assigns every BAR from it (docs/machine.md, "RAM at 4 GiB, and a PCIe
/// window below it"). A device's memory is its BAR's address resolved
/// through those `ranges`, then arm-io's, so arm-io's must cover the window
/// too. The template's one arm-io range stays as it is, since the 27.0.1
/// kernel stops before its first console line with it stretched to 4 GiB;
/// where the window runs past it, a second range carries on from its end.
fn pcie_window(io: &mut Node, (base, size): (u64, u64)) -> Result<(), Error> {
    let bad = Error::BadArmIoRanges;
    let r = io.get("ranges").ok_or(bad)?;
    let [child, parent, len] = match r.as_chunks::<8>() {
        (&[a, b, c], []) => [a, b, c].map(u64::from_le_bytes),
        _ => return Err(bad),
    };
    let end = base.checked_add(size).ok_or(bad)?;
    let io_end = parent.checked_add(len).ok_or(bad)?;
    // The window's address under arm-io.
    let io_base = base
        .checked_sub(parent)
        .and_then(|offset| offset.checked_add(child))
        .ok_or(bad)?;
    if end > io_end {
        let mut v = r.to_vec();
        for cell in [child.checked_add(len).ok_or(bad)?, io_end, end - io_end] {
            v.extend(cell.to_le_bytes());
        }
        io.set("ranges", &v);
    }

    // A PCI address is a space code and 64 bits, here 32-bit memory space;
    // then come the window's address under arm-io, and its size.
    let mut v = Vec::with_capacity(28);
    v.extend(0x0200_0000u32.to_le_bytes());
    for cell in [base, io_base, size] {
        v.extend(cell.to_le_bytes());
    }
    io.child_mut("pcie")
        .ok_or(Error::Missing("/arm-io/pcie"))?
        .set("ranges", &v);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn machine() -> Machine {
        Machine {
            ram_base: 0x7000_0000,
            ram_size: 0x2_0000_0000,
            pcie_window: (0x5000_0000, 0x1fff_0000),
            cpus: 2,
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

    #[test]
    fn directives() {
        let m = machine();
        assert_eq!(
            directive(b"syscfg/SrNm/0x20,zeroes/0x20\0", &m),
            Ok(Some(padded("1234", 32)))
        );
        assert_eq!(
            directive(b"syscfg/CFG#/0x40,zeroes/0x40", &m),
            Ok(Some(vec![0; 64]))
        );
        assert_eq!(directive(b"syscfg/RMd#/0x20", &m), Ok(None));
        assert_eq!(directive(b"macaddr/wifiaddr,syscfg/WMac/6", &m), Ok(None));
        assert_eq!(directive(b"zeroes/oops", &m), Ok(None));
        assert_eq!(
            directive(b"syscfg/Regn/oops", &m),
            Ok(Some(padded("D/A", 4)))
        );
    }

    /// A directive's length past `DIRECTIVE_MAX` is an error, not an
    /// allocation of whatever it says.
    #[test]
    fn directive_too_long() {
        let m = machine();
        assert_eq!(
            directive(b"syscfg/SrNm/0x1000", &m).unwrap().unwrap().len(),
            DIRECTIVE_MAX
        );
        for spec in [
            &b"zeroes/0xffffffffffff"[..],
            b"syscfg/SrNm/0x1001,zeroes/0x20",
            b"syscfg/CFG#/0x40,zeroes/0x7fffffff",
        ] {
            assert_eq!(directive(spec, &m), Err(Error::BadDirective));
        }
    }

    /// A CHIP or BORD that doesn't fit 32 bits is taken as absent, not cut.
    #[test]
    fn chip_and_board() {
        use crate::im4m::{Entry, Property};
        let manifest = Manifest {
            raw: &[],
            entries: vec![Entry {
                code: *b"MANP",
                properties: vec![
                    Property {
                        code: *b"CHIP",
                        value: Value::Int(0xfe01),
                    },
                    Property {
                        code: *b"BORD",
                        value: Value::Int(0x1_0000_0020),
                    },
                ],
            }],
        };
        assert_eq!(manifest_u32(&manifest, b"CHIP", 0xfe00), 0xfe01);
        assert_eq!(manifest_u32(&manifest, b"BORD", 0x20), 0x20);
        assert_eq!(manifest_u32(&manifest, b"ECID", 7), 7);
    }

    #[test]
    fn syscfg_layout() {
        let v = syscfg_v2(&machine());
        assert_eq!(v.len(), 260);
        assert_eq!(&v[..4], b"gfCS");
        assert_eq!(&v[24..32], b"BTNCmNrS");
        assert_eq!(&v[0xa4..0xa8], b"1234");
        assert_eq!(&v[0xc4..0xca], b"VM0001");
        assert_eq!(&v[0xe4..0xe7], b"D/A");
    }

    fn cells(v: &[u8]) -> Vec<u32> {
        v.chunks(4)
            .map(|c| u32::from_le_bytes(c.try_into().unwrap()))
            .collect()
    }

    fn bytes(cells: &[u32]) -> Vec<u8> {
        cells.iter().flat_map(|c| c.to_le_bytes()).collect()
    }

    /// arm-io and its PCIe node as every template has them.
    fn arm_io() -> Node {
        let mut pcie = Node::default();
        pcie.set_str("name", "pcie");
        pcie.set(
            "ranges",
            &bytes(&[0x0200_0000, 0x5000_0000, 0, 0x4000_0000, 0, 0x2fff_0000, 0]),
        );
        let mut io = Node::default();
        io.set_str("name", "arm-io");
        io.set("ranges", &bytes(&[0, 0, 0x1000_0000, 0, 0x7000_0000, 0]));
        io.children.push(pcie);
        io
    }

    #[test]
    fn pcie_window_below_ram() {
        let mut io = arm_io();
        pcie_window(&mut io, (0x5000_0000, 0x1fff_0000)).unwrap();
        assert_eq!(
            cells(io.child("pcie").unwrap().get("ranges").unwrap()),
            [0x0200_0000, 0x5000_0000, 0, 0x4000_0000, 0, 0x1fff_0000, 0]
        );
        // arm-io already reaches past it.
        assert_eq!(
            cells(io.get("ranges").unwrap()),
            [0, 0, 0x1000_0000, 0, 0x7000_0000, 0]
        );
    }

    #[test]
    fn pcie_window_up_to_4_gib() {
        let mut io = arm_io();
        pcie_window(&mut io, (0x5000_0000, 0xb000_0000)).unwrap();
        assert_eq!(
            cells(io.child("pcie").unwrap().get("ranges").unwrap()),
            [0x0200_0000, 0x5000_0000, 0, 0x4000_0000, 0, 0xb000_0000, 0]
        );
        // arm-io keeps its range and gets one from its end to 4 GiB.
        assert_eq!(
            cells(io.get("ranges").unwrap()),
            [
                0,
                0,
                0x1000_0000,
                0,
                0x7000_0000,
                0, //
                0x7000_0000,
                0,
                0x8000_0000,
                0,
                0x8000_0000,
                0
            ]
        );
    }

    #[test]
    fn pcie_window_needs_arm_io_ranges() {
        let mut io = arm_io();
        io.set("ranges", &[0; 48]);
        assert_eq!(
            pcie_window(&mut io, (0x5000_0000, 0x1fff_0000)),
            Err(Error::BadArmIoRanges)
        );
        let mut io = arm_io();
        assert_eq!(
            pcie_window(&mut io, (0x800_0000, 0x1000)),
            Err(Error::BadArmIoRanges)
        );
        // Ranges whose ends, or whose address for the window, wrap past the
        // end of the address space.
        for ranges in [
            [0, 0x4000_0000, u64::MAX],
            [u64::MAX - 0x1000, 0, 0x1000_0000],
            [u64::MAX - 0x1000, 0, 0x1_0000_0000],
            [u64::MAX - 0x5800_0000, 0, 0x6000_0000],
        ] {
            let mut io = arm_io();
            let v: Vec<u8> = ranges.iter().flat_map(|c| c.to_le_bytes()).collect();
            io.set("ranges", &v);
            assert_eq!(
                pcie_window(&mut io, (0x5000_0000, 0x1fff_0000)),
                Err(Error::BadArmIoRanges),
                "{ranges:x?}"
            );
        }
    }

    /// The display's node is added only when there is a display and the
    /// configuration names one, and never over a node arm-io has.
    #[test]
    fn display_node_with_a_display() {
        let fb = Framebuffer {
            base: 0x8040_0000,
            width: 1920,
            height: 1080,
            stride: 7680,
            reserved: 0,
        };
        let named = Machine {
            display_node: Some("display0".into()),
            ..machine()
        };
        let mut io = arm_io();
        super::arm_io(&mut io, &named).unwrap();
        assert!(io.child("display0").is_none(), "no display, no node");

        let m = Machine {
            display: Some(fb),
            ..named
        };
        let mut io = arm_io();
        super::arm_io(&mut io, &m).unwrap();
        let node = io.child("display0").unwrap();
        assert_eq!(node.get("name"), Some(&b"display0\0"[..]));
        assert_eq!(node.get("device_type"), Some(&b"display-subsystem\0"[..]));
        assert_eq!(node.properties.len(), 2);

        let mut io = arm_io();
        let unnamed = Machine {
            display_node: None,
            ..m.clone()
        };
        super::arm_io(&mut io, &unnamed).unwrap();
        assert_eq!(io.children.len(), 1, "only pcie");

        let mut io = arm_io();
        let pcie = Machine {
            display_node: Some("pcie".into()),
            ..m
        };
        assert_eq!(super::arm_io(&mut io, &pcie), Err(Error::DisplayNodeTaken));
    }

    /// The memory map's size depends on whether an AuxKC is placed, not on
    /// where anything is, so the boot can measure the tree with placeholders.
    #[test]
    fn memory_map_size() {
        let size = |at: &Placement| {
            let mut mm = Node::default();
            memory_map(&mut mm, at);
            mm.size()
        };
        let placed = Placement {
            kc_rw: (0x1_0100_0000, 0x40_0000),
            kc_entry: 0xffff_fe00_0710_0000,
            aux_kc: (0x1_0000_4000, 0x8000),
            ..Placement::default()
        };
        let placeholder = Placement {
            aux_kc: (0, 0x8000),
            ..Placement::default()
        };
        assert_eq!(size(&placeholder), size(&placed));
        assert!(size(&Placement::default()) < size(&placed));
    }

    #[test]
    fn snapshot_name() {
        let mut h = vec![0u8; 208];
        h[16] = 0x9a;
        h[47] = 0x39;
        let n = root_snapshot(&h).unwrap();
        assert!(n.starts_with("com.apple.os.update-9A00"));
        assert!(n.ends_with("0039"));
        assert_eq!(root_snapshot(&h[..100]), Err(Error::BadRootHash));
    }
}
