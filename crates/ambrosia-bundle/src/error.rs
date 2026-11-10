//! Why a bundle can't be built or used, each with a message that says what is
//! wrong and how to fix it.

use ambrosia::{boot, bundle, elf, kext, plist};
use std::fmt;
use std::io;
use std::path::PathBuf;

/// Why a bundle can't be built, or why the firmware would refuse one.
#[derive(Debug)]
pub enum Error {
    /// A file couldn't be read or written.
    Io {
        /// The file.
        path: PathBuf,
        /// What went wrong.
        error: io::Error,
    },
    /// A file's name is not UTF-8, which an archive path must be.
    NotUtf8(PathBuf),
    /// A `--config` or `--driver` that is not a regular file.
    NotAFile(PathBuf),
    /// A `--kext` that is not a directory whose name ends in `.kext`.
    NotAKext(PathBuf),
    /// A `--kext` directory without `Contents/Info.plist`.
    NoInfoPlist(PathBuf),
    /// Something in a kext's directory that is neither a regular file nor a
    /// directory, such as a symbolic link.
    NotFileOrDir(PathBuf),
    /// Two inputs that would go in at the same path in the archive.
    Duplicate(String),
    /// The bundle, or the files that would go in it, is more than
    /// [`bundle::MAX_LEN`] bytes.
    TooBig(u64),
    /// The archive doesn't start with a newc header, so the firmware would
    /// boot as if no bundle were given.
    NotABundle,
    /// The firmware's reader refuses the archive.
    Archive(bundle::Error),
    /// A property list that doesn't parse: its path in the bundle, and why.
    Plist(String, plist::Error),
    /// config.plist's top level is not a dictionary.
    NotADict,
    /// A key of config.plist or of a kext's Info.plist that is missing or of
    /// the wrong type.
    Key {
        /// The property list's path in the bundle.
        file: String,
        /// The key's path, such as `Kernel.Add[0].BundlePath`.
        key: String,
    },
    /// A patch whose `Find` is empty, or not as long as its `Replace`: its
    /// key path, such as `Kernel.Patch[0]`.
    PatchLength(String),
    /// `Graphics.DisplayNode` is empty or holds a `/`.
    DisplayNode,
    /// A kext the configuration names whose Info.plist is not in the bundle:
    /// its `BundlePath`.
    NoKext(String),
    /// A kext whose Info.plist names an executable the bundle doesn't hold:
    /// the executable's path in the bundle.
    NoExecutable(String),
    /// The graphics driver module the configuration names is not in the
    /// bundle: its path.
    NoDriver(String),
    /// A kext in the bundle that no `Kernel.Add` entry names: its path.
    Unnamed(String),
    /// A kext executable that is a universal binary: its path in the bundle.
    Universal(String),
    /// A kext executable that is not a 64-bit Mach-O: its path in the bundle.
    NotMachO(String),
    /// A kext executable built for another architecture: its path in the
    /// bundle, and the architecture.
    NotArm64e(String, String),
    /// A kext executable the firmware's kext reader refuses: its path in the
    /// bundle, and why.
    Kext(String, kext::Error),
    /// A kext executable the firmware can't lay out in the Auxiliary kernel
    /// collection: its path in the bundle, and why.
    Placement(String, boot::KextError),
    /// The kexts can't be laid out together in the Auxiliary kernel
    /// collection.
    AuxKc(boot::Error),
    /// A graphics driver module the firmware's loader refuses: its path in
    /// the bundle, and why.
    Driver(String, elf::Error),
}

/// What a key of config.plist or of an Info.plist must hold, by its name,
/// and whether the key may be left out.
fn expected(key: &str) -> (&'static str, bool) {
    let name = key.rsplit('.').next().unwrap_or(key);
    if name.ends_with(']') {
        // An element of ExportVtableDiscriminators, the one array of
        // strings.
        return ("a string", false);
    }
    match name {
        "Kernel" | "Graphics" => ("a dictionary", false),
        "Add" | "Patch" | "ExportVtableDiscriminators" => ("an array", false),
        "Enabled" => ("a boolean, <true/> or <false/>", false),
        "Find" | "Replace" => ("data, base64 in <data>", true),
        "Count" => ("an integer", false),
        "BundlePath" | "Identifier" | "CFBundleIdentifier" => ("a string", true),
        _ => ("a string", false),
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        const CONFIG_DOC: &str = "docs/configuration.md";
        match self {
            Error::Io { path, error } => write!(f, "{}: {error}", path.display()),
            Error::NotUtf8(path) => write!(
                f,
                "{}: the name is not UTF-8, which every path in a bundle must be",
                path.display()
            ),
            Error::NotAFile(path) => write!(f, "{}: not a regular file", path.display()),
            Error::NotAKext(path) => write!(
                f,
                "{}: not a kext; --kext takes a kext's bundle directory, whose name ends in .kext",
                path.display()
            ),
            Error::NoInfoPlist(path) => write!(
                f,
                "{}: no Contents/Info.plist; a kext's bundle directory holds its Info.plist there",
                path.display()
            ),
            Error::NotFileOrDir(path) => write!(
                f,
                "{}: neither a file nor a directory; a bundle holds only those, so replace it \
                 with the file it stands for",
                path.display()
            ),
            Error::Duplicate(path) => write!(
                f,
                "two inputs would go in at {path}; give each kext and driver a different name"
            ),
            Error::TooBig(len) => write!(
                f,
                "the bundle is {len} bytes, more than the {} bytes (512 MiB) the firmware reads \
                 from 0x120000000, where QEMU loads it, up to its heap; leave out what the \
                 configuration doesn't use",
                bundle::MAX_LEN
            ),
            Error::NotABundle => f.write_str(
                "not a cpio archive in the \"newc\" format: it doesn't start with 070701, so \
                 the firmware would boot without a configuration",
            ),
            Error::Archive(e) => match e {
                bundle::Error::BadHeader(at) => write!(
                    f,
                    "the archive member at byte {at} has no \"newc\" header; write the archive \
                     with ambrosia-bundle create, or cpio -o -H newc"
                ),
                bundle::Error::Truncated(at) => write!(
                    f,
                    "the archive member at byte {at} runs past the end of the file, which is \
                     cut short"
                ),
                bundle::Error::BadPath(at) => write!(
                    f,
                    "the archive member at byte {at} has a path that is not UTF-8"
                ),
                bundle::Error::NoConfig => f.write_str(
                    "the archive's first file is not config.plist; ambrosia-bundle create puts \
                     it first",
                ),
            },
            Error::Plist(file, e) => write!(
                f,
                "{file}: not a property list the firmware reads: {} at byte {}; it must be an \
                 XML property list, not a binary one",
                e.what, e.at
            ),
            Error::NotADict => write!(
                f,
                "config.plist: the top level must be a <dict> ({CONFIG_DOC} lists its keys)"
            ),
            Error::Key { file, key } => {
                let (want, required) = expected(key);
                let missing = if required { "missing or " } else { "" };
                write!(f, "{file}: {key} is {missing}not {want}")?;
                if file == "config.plist" {
                    write!(f, " ({CONFIG_DOC} gives each key's type)")?;
                }
                Ok(())
            }
            Error::PatchLength(key) => write!(
                f,
                "config.plist: {key}: Find must not be empty, and Replace must be as long as Find"
            ),
            Error::DisplayNode => f.write_str(
                "config.plist: Graphics.DisplayNode must be a node name: a string, not empty \
                 and without a /",
            ),
            Error::NoKext(path) => write!(
                f,
                "config.plist: Kernel.Add names {path}, but the bundle has no \
                 {path}/Contents/Info.plist; give the kext's directory with --kext, or fix \
                 the entry's BundlePath"
            ),
            Error::NoExecutable(path) => {
                let (dir, name) = path.split_once("/Contents/MacOS/").unwrap_or((path, ""));
                write!(
                    f,
                    "{dir}/Contents/Info.plist: CFBundleExecutable is {name}, but the bundle has \
                     no {path}; put the kext's executable there, or fix CFBundleExecutable"
                )
            }
            Error::NoDriver(path) => write!(
                f,
                "config.plist: Graphics.Driver is {path}, but the bundle has no such file; give \
                 the module with --driver, which puts it at Drivers/<its file name>, or fix \
                 Graphics.Driver"
            ),
            Error::Unnamed(path) => write!(
                f,
                "{path} is in the bundle, but config.plist doesn't name it; add an entry for \
                 it to Kernel.Add: <dict><key>BundlePath</key><string>{path}</string></dict>"
            ),
            Error::Universal(path) => write!(
                f,
                "{path}: a universal binary; the firmware reads a thin arm64e Mach-O, which \
                 lipo -thin arm64e takes out of it"
            ),
            Error::NotMachO(path) => write!(
                f,
                "{path}: not a 64-bit Mach-O; a kext's executable must be an arm64e Mach-O"
            ),
            Error::NotArm64e(path, arch) => write!(
                f,
                "{path}: built for {arch}, not arm64e; build the kext with -arch arm64e"
            ),
            Error::Kext(path, e) => {
                let why = match e {
                    kext::Error::BadMagic => "not a 64-bit Mach-O".into(),
                    kext::Error::NotKext => {
                        "not a kext executable, a Mach-O of type MH_KEXT_BUNDLE; link it with -kext"
                            .into()
                    }
                    kext::Error::Truncated => {
                        "a load command or table runs past the end of the file".into()
                    }
                    kext::Error::ShortCommand => {
                        "a load command is too short for what it holds, such as an \
                         LC_SEGMENT_64 for its sections"
                            .into()
                    }
                    kext::Error::HeaderOutside => {
                        "its header and load commands are not in a segment at file offset 0, \
                         where the static linker puts them, at the start of __TEXT"
                            .into()
                    }
                    kext::Error::BadSegment => {
                        "a segment's contents run past the end of the file, or are larger than \
                         the segment"
                            .into()
                    }
                    kext::Error::MultiStart => {
                        "a page of its chained fixups has more than one chain start, which the \
                         firmware doesn't follow"
                            .into()
                    }
                    kext::Error::Unsupported(what) => format!(
                        "its chained fixups have {what}, which the firmware doesn't read; it \
                         reads 4-byte imports with plain names, and \
                         DYLD_CHAINED_PTR_ARM64E_KERNEL pointers without addends"
                    ),
                    kext::Error::NoKmodInfo => {
                        "no _kmod_info symbol; the kernel reads a kext's kmod_info to start it"
                            .into()
                    }
                    kext::Error::NoFixups => {
                        "no chained fixups, the only fixups the firmware applies; link it with \
                         -fixup_chains"
                            .into()
                    }
                };
                write!(f, "{path}: {why}")
            }
            Error::Placement(path, e) => write!(f, "{path}: {e}"),
            Error::AuxKc(e) => write!(
                f,
                "the firmware can't lay the kexts out in an Auxiliary kernel collection: {e}"
            ),
            Error::Driver(path, e) => write!(
                f,
                "{path}: not a driver module the firmware loads: {e} \
                 (docs/graphics-drivers.md gives the format)"
            ),
        }
    }
}

impl std::error::Error for Error {}
