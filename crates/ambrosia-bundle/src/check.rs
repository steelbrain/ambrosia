//! Checks a bundle as the firmware reads it, with the firmware's own readers:
//! the archive (`ambrosia::bundle`), config.plist and each kext's Info.plist
//! (`ambrosia::config`), each kext's executable (`ambrosia::kext`), the
//! kexts' layout in the Auxiliary kernel collection (`ambrosia::auxkc`), and
//! the graphics driver module (`ambrosia::elf`). On top of what the firmware
//! checks, a kext's executable must be built for arm64e, and every kext under
//! `Kexts/` must be one that config.plist names. What depends on the guest's
//! kernel collection, such as the symbols a kext imports, can't be checked
//! here.

use crate::Error;
use ambrosia::boot::{self, KextError};
use ambrosia::bundle::{self, Bundle};
use ambrosia::config::{self, Config};
use ambrosia::plist::{self, Value};
use ambrosia::{auxkc, elf, kext};
use std::collections::BTreeSet;

const MH_MAGIC_64: u32 = 0xfeed_facf;
/// The magics of a universal binary, 32- and 64-bit, as its first 4 bytes.
const FAT_MAGICS: [[u8; 4]; 2] = [[0xca, 0xfe, 0xba, 0xbe], [0xca, 0xfe, 0xba, 0xbf]];
const CPU_TYPE_ARM64: u32 = 0x0100_000c;
const CPU_TYPE_X86_64: u32 = 0x0100_0007;
const CPU_SUBTYPE_ARM64E: u32 = 2;
/// The subtype's capability bits, such as arm64e's pointer authentication
/// ABI version, which say nothing of the architecture.
const CPU_SUBTYPE_MASK: u32 = 0xff00_0000;

/// What a bundle holds, as the firmware reads it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Summary {
    /// The archive's length in bytes, through its trailer.
    pub len: usize,
    /// How many regular files it holds, config.plist among them.
    pub files: usize,
    /// The kexts the firmware places: the enabled entries of `Kernel.Add`.
    pub kexts: Vec<Kext>,
    /// How many patches the firmware applies: the enabled entries of
    /// `Kernel.Patch`.
    pub patches: usize,
    /// The graphics driver module the firmware starts, if any: its path.
    pub driver: Option<String>,
}

/// A kext the firmware places.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Kext {
    /// Its `BundlePath`.
    pub path: String,
    /// Its `CFBundleIdentifier`.
    pub id: String,
}

/// Reads `archive` with the firmware's reader, as the firmware finds it in
/// RAM.
pub fn read(archive: &[u8]) -> Result<Bundle<'_>, Error> {
    if archive.len() > bundle::MAX_LEN {
        return Err(Error::TooBig(archive.len() as u64));
    }
    Bundle::find(archive)
        .ok_or(Error::NotABundle)?
        .map_err(Error::Archive)
}

/// Checks that the firmware would boot with `archive`, and what it would
/// find there.
pub fn check(archive: &[u8]) -> Result<Summary, Error> {
    let b = read(archive)?;
    let config = Config::read(&b).map_err(config_error)?;
    // Config::read parsed config.plist, so this parse succeeds.
    let root = plist::parse(b.config()).map_err(|e| Error::Plist(bundle::CONFIG.into(), e))?;

    let named = bundle_paths(&root);
    for dir in kext_dirs(&b) {
        let covers = |n: &&str| {
            *n == dir
                || n.strip_prefix(dir)
                    .is_some_and(|rest| rest.starts_with('/'))
        };
        if !named.iter().any(covers) {
            return Err(Error::Unnamed(dir.into()));
        }
    }

    for k in &config.kexts {
        if let Some(exe) = k.executable {
            check_executable(&executable_path(k), exe)?;
        }
    }
    // The firmware lays out every kext with an executable in the Auxiliary
    // KC, and sizes that layout before it reads the guest's kernel.
    let aux: Vec<auxkc::KextInput<'_>> = config
        .kexts
        .iter()
        .filter_map(|k| {
            Some(auxkc::KextInput {
                id: &k.id,
                info: k.info,
                exe: k.executable?,
            })
        })
        .collect();
    if !aux.is_empty() {
        auxkc::size(&aux).map_err(|e| layout_error(e, &config.kexts))?;
    }
    if let Some(d) = &config.driver {
        elf::Module::parse(d.elf).map_err(|e| Error::Driver(d.path.clone(), e))?;
    }

    Ok(Summary {
        len: b.len,
        files: b.files.len(),
        kexts: config
            .kexts
            .iter()
            .map(|k| Kext {
                path: k.path.clone(),
                id: k.id.clone(),
            })
            .collect(),
        patches: config.patches.len(),
        driver: config.driver.map(|d| d.path),
    })
}

/// The `BundlePath` of every entry of `Kernel.Add` that has one, enabled or
/// not, without a trailing `/`.
fn bundle_paths(root: &Value) -> Vec<&str> {
    let add = root
        .get("Kernel")
        .and_then(|k| k.get("Add"))
        .and_then(Value::as_array)
        .unwrap_or_default();
    add.iter()
        .filter_map(|e| Some(e.get("BundlePath")?.as_str()?.trim_end_matches('/')))
        .collect()
}

/// The kext bundles directly under `Kexts/`, where `create` puts them: each
/// `Kexts/<name>.kext` that holds a file.
fn kext_dirs<'a>(b: &Bundle<'a>) -> BTreeSet<&'a str> {
    b.files
        .iter()
        .filter_map(|f| {
            let rest = f.path.strip_prefix("Kexts/")?;
            let (name, _) = rest.split_once('/')?;
            name.ends_with(".kext")
                .then_some(&f.path[.."Kexts/".len() + name.len()])
        })
        .collect()
}

/// The path in the bundle of a kext's executable, which [`Config::read`]
/// has found there.
fn executable_path(k: &config::Kext<'_>) -> String {
    let name = plist::parse(k.info)
        .ok()
        .and_then(|p| p.get("CFBundleExecutable")?.as_str().map(String::from))
        .unwrap_or_default();
    format!("{}/Contents/MacOS/{name}", k.path)
}

/// The tool's error for [`Config::read`]'s `e`.
fn config_error(e: config::Error) -> Error {
    match e {
        config::Error::Plist(file, e) => Error::Plist(file, e),
        config::Error::NotADict => Error::NotADict,
        config::Error::Key { file, key } => Error::Key { file, key },
        config::Error::PatchLength(i) => Error::PatchLength(format!("Kernel.Patch[{i}]")),
        config::Error::DisplayNode => Error::DisplayNode,
        config::Error::NoKext(path) => Error::NoKext(path),
        config::Error::NoExecutable(path) => Error::NoExecutable(path),
        config::Error::NoDriver(path) => Error::NoDriver(path),
    }
}

/// The tool's error for `e`, from laying out `kexts` in the Auxiliary KC,
/// naming the executable of the kext it blames.
fn layout_error(e: boot::Error, kexts: &[config::Kext<'_>]) -> Error {
    match e {
        boot::Error::Kext(id, why) => match kexts.iter().find(|k| k.id == id) {
            Some(k) => match why {
                KextError::Exe(e) => Error::Kext(executable_path(k), e),
                why => Error::Placement(executable_path(k), why),
            },
            None => Error::AuxKc(boot::Error::Kext(id, why)),
        },
        e => Error::AuxKc(e),
    }
}

/// Checks a kext's executable, at `path` in the bundle: a thin arm64e Mach-O
/// that the firmware's kext reader takes.
fn check_executable(path: &str, exe: &[u8]) -> Result<(), Error> {
    if FAT_MAGICS.iter().any(|m| exe.starts_with(m)) {
        return Err(Error::Universal(path.into()));
    }
    let word = |at: usize| {
        exe.get(at..at + 4)
            .map(|b| u32::from_le_bytes(b.try_into().unwrap()))
    };
    if word(0) != Some(MH_MAGIC_64) {
        return Err(Error::NotMachO(path.into()));
    }
    let (cputype, subtype) = (word(4).unwrap_or(0), word(8).unwrap_or(0));
    if cputype != CPU_TYPE_ARM64 || subtype & !CPU_SUBTYPE_MASK != CPU_SUBTYPE_ARM64E {
        let arch = match (cputype, subtype & !CPU_SUBTYPE_MASK) {
            (CPU_TYPE_ARM64, 0) => "arm64".into(),
            (CPU_TYPE_X86_64, _) => "x86_64".into(),
            _ => format!("CPU type {cputype:#x}, subtype {subtype:#x}"),
        };
        return Err(Error::NotArm64e(path.into(), arch));
    }
    kext::Kext::parse(exe).map_err(|e| Error::Kext(path.into(), e))?;
    Ok(())
}
