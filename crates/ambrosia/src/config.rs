//! Ambrosia's configuration: the bundle's `config.plist`, which names the
//! kexts to add and the patches to apply, as OpenCore's `config.plist`
//! does on x86-64, and the graphics driver module to start.
//!
//! ```text
//! <dict>
//!   <key>BootArgs</key>          <string>-v</string>  (optional, default none)
//!   <key>Kernel</key>
//!   <dict>
//!     <key>Add</key>
//!     <array>
//!       <dict>
//!         <key>BundlePath</key>  <string>Kexts/Example.kext</string>
//!         <key>Enabled</key>     <true/>           (optional, default true)
//!       </dict>
//!     </array>
//!     <key>Patch</key>
//!     <array>
//!       <dict>
//!         <key>Identifier</key>  <string>com.apple.kernel</string>
//!         <key>Find</key>        <data>...</data>
//!         <key>Replace</key>     <data>...</data> (as long as Find)
//!         <key>Count</key>       <integer>1</integer> (optional; 0, the
//!                                                     default, is every match)
//!         <key>Enabled</key>     <true/>           (optional, default true)
//!       </dict>
//!     </array>
//!     <key>ExportVtableDiscriminators</key>       (optional)
//!     <array>
//!       <string>__ZTV9IOService</string>
//!     </array>
//!   </dict>
//!   <key>Graphics</key>                           (optional)
//!   <dict>
//!     <key>Driver</key>  <string>Drivers/display.elf</string> (optional,
//!                                                     default none)
//!     <key>DisplayNode</key> <string>display0</string> (optional,
//!                                                     default none)
//!   </dict>
//! </dict>
//! ```
//!
//! `BootArgs` is the kernel's command line. A guest needs none, but they help
//! in debugging: `serial=3 -v` puts the kernel's log on the UART.
//!
//! A kext's `BundlePath` is its bundle's directory in the configuration
//! bundle. Its Info.plist is at `Contents/Info.plist` there, and its
//! executable, if it has one, is the `CFBundleExecutable` the Info.plist
//! names, under `Contents/MacOS/`. A patch's `Identifier` names the kernel
//! collection entry it applies to, by the entry's bundle identifier
//! (`com.apple.kernel` for the kernel itself).
//!
//! `Kernel` > `ExportVtableDiscriminators` lists vtable symbols of the
//! kernel collection whose per-slot pointer authentication discriminators
//! Ambrosia hands over to injected code under `/chosen/ambrosia`
//! (`vtdisc.rs`; docs/internals/handover.md, "Vtable discriminators").
//!
//! `Graphics` > `Driver` is the path in the bundle of the graphics driver
//! module to start: an ELF file in the format `elf.rs` loads, with the
//! interface `graphics.rs` gives (docs/graphics-drivers.md). It drives a
//! display device, on which Ambrosia's log then shows as an early console,
//! and which the kernel is handed. With no driver, the log goes only to the
//! UART.
//!
//! `Graphics` > `DisplayNode` names a device tree node that Ambrosia adds
//! under `/arm-io` for the display the driver sets up, with `device_type`
//! `display-subsystem`, for a kernel display driver that attaches to it
//! (`tree.rs`). It is written only when the driver drives a display.

use crate::bundle::{self, Bundle};
use crate::plist::{self, Value};
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::fmt;

/// A kext to add, read from the bundle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Kext<'a> {
    /// Its `BundlePath`.
    pub path: String,
    /// Its `CFBundleIdentifier`.
    pub id: String,
    /// Its Info.plist, as the bundle holds it.
    pub info: &'a [u8],
    /// Its executable, if its Info.plist names one.
    pub executable: Option<&'a [u8]>,
}

/// A patch to apply to one kernel collection entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Patch {
    /// The bundle identifier of the entry it applies to.
    pub identifier: String,
    /// The bytes to find.
    pub find: Vec<u8>,
    /// The bytes to put in their place, as many as `find`.
    pub replace: Vec<u8>,
    /// How many matches to replace, from the entry's start: 0 is all.
    pub count: u64,
}

/// The graphics driver module to start, read from the bundle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Driver<'a> {
    /// Its path in the bundle.
    pub path: String,
    /// Its ELF file.
    pub elf: &'a [u8],
}

/// What the configuration asks for. With no bundle, it is the default: no
/// command line, kexts, patches or graphics driver.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Config<'a> {
    /// The kernel's command line, from `BootArgs`.
    pub command_line: String,
    /// The enabled kexts of `Kernel` > `Add`, in order.
    pub kexts: Vec<Kext<'a>>,
    /// The enabled patches of `Kernel` > `Patch`, in order.
    pub patches: Vec<Patch>,
    /// The graphics driver module `Graphics` > `Driver` names, if any.
    pub driver: Option<Driver<'a>>,
    /// The display's device tree node, from `Graphics` > `DisplayNode`: a
    /// name, not empty and without a `/`.
    pub display_node: Option<String>,
    /// The donor vtable symbols of `Kernel` > `ExportVtableDiscriminators`,
    /// whose per-slot PAC discriminators Ambrosia hands over for injected code.
    pub export_vtable_disc: Vec<String>,
}

/// Why the configuration can't be used, with the file, key or path that is
/// wrong.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// A file that doesn't parse as a property list: its path in the bundle,
    /// `config.plist` or a kext's Info.plist, and why.
    Plist(String, plist::Error),
    /// `config.plist`'s top level is not a dictionary.
    NotADict,
    /// A key holds the wrong type, or a required key is missing.
    Key {
        /// The property list's path in the bundle: `config.plist`, or a
        /// kext's Info.plist.
        file: String,
        /// The key's path in it, such as `Kernel.Add[0].BundlePath`.
        key: String,
    },
    /// A patch whose `Find` is empty, or not as long as its `Replace`: its
    /// index in `Kernel` > `Patch`.
    PatchLength(usize),
    /// `Graphics` > `DisplayNode` is empty or holds a `/`.
    DisplayNode,
    /// A kext whose Info.plist is not in the bundle: its `BundlePath`.
    NoKext(String),
    /// A kext's executable, which its Info.plist names, is not in the bundle:
    /// the executable's path.
    NoExecutable(String),
    /// The graphics driver module `Graphics` > `Driver` names is not in the
    /// bundle: its path.
    NoDriver(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let config = bundle::CONFIG;
        match self {
            Error::Plist(file, e) => write!(f, "{file} doesn't parse as a property list: {e}"),
            Error::NotADict => write!(f, "{config}: its top level is not a dictionary"),
            Error::Key { file, key } => {
                write!(f, "{file}: {key} is missing or has the wrong type")
            }
            Error::PatchLength(i) => write!(
                f,
                "{config}: Kernel.Patch[{i}]: Find is empty or not as long as Replace"
            ),
            Error::DisplayNode => {
                write!(f, "{config}: Graphics.DisplayNode is empty or holds a /")
            }
            Error::NoKext(path) => write!(
                f,
                "{config}: Kernel.Add names {path}, but the bundle has no \
                 {path}/Contents/Info.plist"
            ),
            Error::NoExecutable(path) => write!(
                f,
                "the bundle has no {path}, which a kext's CFBundleExecutable names"
            ),
            Error::NoDriver(path) => write!(
                f,
                "{config}: Graphics.Driver names {path}, but the bundle has no such file"
            ),
        }
    }
}

/// The key path `key` in `config.plist`.
fn config_key(key: String) -> Error {
    Error::Key {
        file: bundle::CONFIG.into(),
        key,
    }
}

/// Where a key is: the property list's path in the bundle, and the key path
/// to the dictionary that holds it, with its separator, such as
/// `Kernel.Add[0].`.
struct At<'p> {
    file: &'p str,
    path: String,
}

impl At<'_> {
    /// The error for `key`, here.
    fn key(&self, key: &str) -> Error {
        Error::Key {
            file: self.file.into(),
            key: format!("{}{key}", self.path),
        }
    }
}

/// The value under `key` in `dict`, as `as_type` reads it, or `None` if
/// `dict` has no `key`. A value `as_type` can't read is an error, named by
/// `at` and `key`.
fn optional<'v, T>(
    dict: &'v Value,
    at: &At<'_>,
    key: &str,
    as_type: impl FnOnce(&'v Value) -> Option<T>,
) -> Result<Option<T>, Error> {
    match dict.get(key) {
        None => Ok(None),
        Some(v) => as_type(v).map(Some).ok_or_else(|| at.key(key)),
    }
}

/// As [`optional`], for a key `dict` must have.
fn required<'v, T>(
    dict: &'v Value,
    at: &At<'_>,
    key: &str,
    as_type: impl FnOnce(&'v Value) -> Option<T>,
) -> Result<T, Error> {
    optional(dict, at, key, as_type)?.ok_or_else(|| at.key(key))
}

/// `v`, if it is a dictionary.
fn dict(v: &Value) -> Option<&Value> {
    matches!(v, Value::Dict(_)).then_some(v)
}

impl<'a> Config<'a> {
    /// Reads the configuration in `bundle`, and each kext it names.
    pub fn read(bundle: &Bundle<'a>) -> Result<Config<'a>, Error> {
        let root =
            plist::parse(bundle.config()).map_err(|e| Error::Plist(bundle::CONFIG.into(), e))?;
        if dict(&root).is_none() {
            return Err(Error::NotADict);
        }
        let config_at = |path: &str| At {
            file: bundle::CONFIG,
            path: path.into(),
        };
        let (top, kernel_at, graphics_at) =
            (config_at(""), config_at("Kernel."), config_at("Graphics."));
        // A missing Kernel or Graphics reads as an empty dictionary.
        let empty = Value::Dict(Vec::new());
        let kernel = optional(&root, &top, "Kernel", dict)?.unwrap_or(&empty);
        let mut config = Config {
            command_line: optional(&root, &top, "BootArgs", Value::as_str)?
                .unwrap_or_default()
                .to_string(),
            ..Config::default()
        };

        let add = optional(kernel, &kernel_at, "Add", Value::as_array)?.unwrap_or_default();
        for (i, e) in add.iter().enumerate() {
            let at = config_at(&format!("Kernel.Add[{i}]."));
            if optional(e, &at, "Enabled", Value::as_bool)? == Some(false) {
                continue;
            }
            let path = required(e, &at, "BundlePath", Value::as_str)?;
            config.kexts.push(kext(bundle, path.trim_end_matches('/'))?);
        }

        let patches = optional(kernel, &kernel_at, "Patch", Value::as_array)?.unwrap_or_default();
        for (i, e) in patches.iter().enumerate() {
            let at = config_at(&format!("Kernel.Patch[{i}]."));
            if optional(e, &at, "Enabled", Value::as_bool)? == Some(false) {
                continue;
            }
            let identifier = required(e, &at, "Identifier", Value::as_str)?.to_string();
            let find = required(e, &at, "Find", Value::as_data)?.to_vec();
            let replace = required(e, &at, "Replace", Value::as_data)?.to_vec();
            if find.is_empty() || find.len() != replace.len() {
                return Err(Error::PatchLength(i));
            }
            let count = optional(e, &at, "Count", Value::as_u64)?.unwrap_or(0);
            config.patches.push(Patch {
                identifier,
                find,
                replace,
                count,
            });
        }

        let graphics = optional(&root, &top, "Graphics", dict)?.unwrap_or(&empty);
        if let Some(path) = optional(graphics, &graphics_at, "Driver", Value::as_str)? {
            let elf = bundle
                .file(path)
                .ok_or_else(|| Error::NoDriver(path.to_string()))?;
            config.driver = Some(Driver {
                path: path.to_string(),
                elf,
            });
        }
        if let Some(name) = optional(graphics, &graphics_at, "DisplayNode", Value::as_str)? {
            if name.is_empty() || name.contains('/') {
                return Err(Error::DisplayNode);
            }
            config.display_node = Some(name.to_string());
        }

        let exports = optional(
            kernel,
            &kernel_at,
            "ExportVtableDiscriminators",
            Value::as_array,
        )?
        .unwrap_or_default();
        for (i, e) in exports.iter().enumerate() {
            let sym = e
                .as_str()
                .ok_or_else(|| config_key(format!("Kernel.ExportVtableDiscriminators[{i}]")))?;
            config.export_vtable_disc.push(sym.to_string());
        }
        Ok(config)
    }
}

/// The kext whose bundle directory is `path` in `bundle`.
fn kext<'a>(bundle: &Bundle<'a>, path: &str) -> Result<Kext<'a>, Error> {
    let info_path = format!("{path}/Contents/Info.plist");
    let info = bundle
        .file(&info_path)
        .ok_or_else(|| Error::NoKext(path.to_string()))?;
    let plist = plist::parse(info).map_err(|e| Error::Plist(info_path.clone(), e))?;
    let at = At {
        file: &info_path,
        path: String::new(),
    };
    let id = required(&plist, &at, "CFBundleIdentifier", Value::as_str)?.to_string();
    let executable = match optional(&plist, &at, "CFBundleExecutable", Value::as_str)? {
        None => None,
        Some(name) => {
            let exe = format!("{path}/Contents/MacOS/{name}");
            Some(bundle.file(&exe).ok_or(Error::NoExecutable(exe))?)
        }
    };
    Ok(Kext {
        path: path.to_string(),
        id,
        info,
        executable,
    })
}

/// Applies `patch` to `bytes`, part of the entry it names, replacing at most
/// `limit` matches, or all of them if `limit` is 0, and returns how many it
/// replaced. A patch whose `find` is empty, or not as long as its `replace`,
/// which [`Config::read`] never gives, replaces nothing.
pub fn apply(patch: &Patch, bytes: &mut [u8], limit: u64) -> u64 {
    let n = patch.find.len();
    // An empty `find` would match at every byte without moving past it.
    if n == 0 || patch.replace.len() != n {
        return 0;
    }
    let mut done = 0;
    let mut at = 0;
    while at + n <= bytes.len() && (limit == 0 || done < limit) {
        if bytes[at..at + n] == patch.find[..] {
            bytes[at..at + n].copy_from_slice(&patch.replace);
            done += 1;
            at += n;
        } else {
            at += 1;
        }
    }
    done
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bundle::tests::archive;

    const KEXT_INFO: &[u8] = br#"<plist version="1.0"><dict>
        <key>CFBundleIdentifier</key><string>org.example.T</string>
        <key>CFBundleExecutable</key><string>T</string>
    </dict></plist>"#;

    fn config(text: &str, extra: &[(&str, u32, &[u8])]) -> Vec<u8> {
        let mut m: Vec<(&str, u32, &[u8])> = vec![("config.plist", 0o100644, text.as_bytes())];
        m.extend_from_slice(extra);
        archive(&m)
    }

    #[test]
    fn reads_kexts_and_patches() {
        let a = config(
            r#"<dict><key>BootArgs</key><string>serial=3 -v</string><key>Kernel</key><dict>
            <key>Add</key><array>
              <dict><key>BundlePath</key><string>Kexts/T.kext/</string></dict>
              <dict><key>BundlePath</key><string>Kexts/Off.kext</string><key>Enabled</key><false/></dict>
            </array>
            <key>Patch</key><array>
              <dict><key>Identifier</key><string>com.apple.kernel</string>
                <key>Find</key><data>AQID</data><key>Replace</key><data>BAUG</data>
                <key>Count</key><integer>1</integer></dict>
            </array></dict></dict>"#,
            &[
                ("Kexts/T.kext/Contents/Info.plist", 0o100644, KEXT_INFO),
                ("Kexts/T.kext/Contents/MacOS/T", 0o100755, b"MACHO"),
            ],
        );
        let b = Bundle::parse(&a).unwrap();
        let c = Config::read(&b).unwrap();
        assert_eq!(c.command_line, "serial=3 -v");
        assert_eq!(c.kexts.len(), 1);
        assert_eq!(c.kexts[0].path, "Kexts/T.kext");
        assert_eq!(c.kexts[0].id, "org.example.T");
        assert_eq!(c.kexts[0].executable, Some(&b"MACHO"[..]));
        assert_eq!(
            c.patches,
            [Patch {
                identifier: "com.apple.kernel".into(),
                find: vec![1, 2, 3],
                replace: vec![4, 5, 6],
                count: 1
            }]
        );
    }

    #[test]
    fn reads_export_vtable_discriminators() {
        let a = config(
            r#"<dict><key>Kernel</key><dict>
            <key>ExportVtableDiscriminators</key><array>
              <string>__ZTV25IOMobileFramebufferLegacy</string>
              <string>__ZTV9IOService</string>
            </array></dict></dict>"#,
            &[],
        );
        let c = Config::read(&Bundle::parse(&a).unwrap()).unwrap();
        assert_eq!(
            c.export_vtable_disc,
            ["__ZTV25IOMobileFramebufferLegacy", "__ZTV9IOService"].map(|s| s.to_string())
        );
    }

    #[test]
    fn empty_and_wrong() {
        let a = config("<dict/>", &[]);
        assert_eq!(
            Config::read(&Bundle::parse(&a).unwrap()).unwrap(),
            Config::default()
        );
        let a = config("<array/>", &[]);
        assert_eq!(
            Config::read(&Bundle::parse(&a).unwrap()).unwrap_err(),
            Error::NotADict
        );
        for (text, want) in [
            ("<dict><key>Kernel</key><array/></dict>", "Kernel"),
            (
                "<dict><key>BootArgs</key><integer>1</integer></dict>",
                "BootArgs",
            ),
            (
                "<dict><key>Kernel</key><dict><key>Add</key><dict/></dict></dict>",
                "Kernel.Add",
            ),
            (
                "<dict><key>Kernel</key><dict><key>Add</key><array><dict><key>Enabled</key><string>no</string></dict></array></dict></dict>",
                "Kernel.Add[0].Enabled",
            ),
            (
                "<dict><key>Kernel</key><dict><key>Patch</key><array><dict><key>Identifier</key><string>x</string></dict></array></dict></dict>",
                "Kernel.Patch[0].Find",
            ),
            (
                "<dict><key>Kernel</key><dict><key>Patch</key><array><dict><key>Identifier</key><string>x</string><key>Find</key><data>AQ==</data><key>Replace</key><data>Ag==</data><key>Count</key><true/></dict></array></dict></dict>",
                "Kernel.Patch[0].Count",
            ),
            (
                "<dict><key>Kernel</key><dict><key>Add</key><array><dict/></array></dict></dict>",
                "Kernel.Add[0].BundlePath",
            ),
        ] {
            let a = config(text, &[]);
            assert_eq!(
                Config::read(&Bundle::parse(&a).unwrap()).unwrap_err(),
                config_key(want.into())
            );
        }
        let a = config(
            "<dict><key>Kernel</key><dict><key>Patch</key><array><dict><key>Identifier</key><string>x</string><key>Find</key><data>AQ==</data><key>Replace</key><data>AQI=</data></dict></array></dict></dict>",
            &[],
        );
        assert_eq!(
            Config::read(&Bundle::parse(&a).unwrap()).unwrap_err(),
            Error::PatchLength(0)
        );
        let a = config(
            "<dict><key>Kernel</key><dict><key>Add</key><array><dict><key>BundlePath</key><string>K.kext</string></dict></array></dict></dict>",
            &[],
        );
        assert_eq!(
            Config::read(&Bundle::parse(&a).unwrap()).unwrap_err(),
            Error::NoKext("K.kext".into())
        );
        let info = b"<dict><key>CFBundleIdentifier</key><string>T</string>\
                     <key>CFBundleExecutable</key><true/></dict>";
        let a = config(
            "<dict><key>Kernel</key><dict><key>Add</key><array><dict><key>BundlePath</key><string>K.kext</string></dict></array></dict></dict>",
            &[("K.kext/Contents/Info.plist", 0o100644, info)],
        );
        assert_eq!(
            Config::read(&Bundle::parse(&a).unwrap()).unwrap_err(),
            Error::Key {
                file: "K.kext/Contents/Info.plist".into(),
                key: "CFBundleExecutable".into()
            }
        );
        let info = b"<dict><key>CFBundleIdentifier</key><string>T</string>\
                     <key>CFBundleExecutable</key><string>T</string></dict>";
        let a = config(
            "<dict><key>Kernel</key><dict><key>Add</key><array><dict><key>BundlePath</key><string>K.kext</string></dict></array></dict></dict>",
            &[("K.kext/Contents/Info.plist", 0o100644, info)],
        );
        assert_eq!(
            Config::read(&Bundle::parse(&a).unwrap()).unwrap_err(),
            Error::NoExecutable("K.kext/Contents/MacOS/T".into())
        );
        let a = config(
            "<dict><key>Kernel</key><dict><key>Add</key><array><dict><key>BundlePath</key><string>K.kext</string></dict></array></dict></dict>",
            &[("K.kext/Contents/Info.plist", 0o100644, b"<dict>")],
        );
        assert!(matches!(
            Config::read(&Bundle::parse(&a).unwrap()).unwrap_err(),
            Error::Plist(file, _) if file == "K.kext/Contents/Info.plist"
        ));
    }

    #[test]
    fn reads_the_display_node() {
        let read = |text: &str| {
            Config::read(&Bundle::parse(&config(text, &[])).unwrap()).map(|c| c.display_node)
        };
        assert_eq!(
            read(
                "<dict><key>Graphics</key><dict><key>DisplayNode</key><string>display0</string></dict></dict>"
            ),
            Ok(Some("display0".into()))
        );
        assert_eq!(read("<dict/>"), Ok(None));
        // Not a string, empty, or with a `/`, which would make it a path.
        for (value, want) in [
            ("<true/>", config_key("Graphics.DisplayNode".into())),
            ("<string></string>", Error::DisplayNode),
            ("<string>a/b</string>", Error::DisplayNode),
        ] {
            let text = format!(
                "<dict><key>Graphics</key><dict><key>DisplayNode</key>{value}</dict></dict>"
            );
            assert_eq!(read(&text).unwrap_err(), want, "{value}");
        }
    }

    #[test]
    fn reads_the_driver() {
        let text = "<dict><key>Graphics</key><dict><key>Driver</key><string>Drivers/d.elf</string></dict></dict>";
        let a = config(text, &[("Drivers/d.elf", 0o100644, b"\x7fELF")]);
        let b = Bundle::parse(&a).unwrap();
        assert_eq!(
            Config::read(&b).unwrap().driver,
            Some(Driver {
                path: "Drivers/d.elf".into(),
                elf: b"\x7fELF"
            })
        );
        let a = config(text, &[]);
        assert_eq!(
            Config::read(&Bundle::parse(&a).unwrap()).unwrap_err(),
            Error::NoDriver("Drivers/d.elf".into())
        );
        let a = config("<dict><key>Graphics</key><dict/></dict>", &[]);
        assert_eq!(
            Config::read(&Bundle::parse(&a).unwrap()).unwrap().driver,
            None
        );
        for (text, want) in [
            (
                "<dict><key>Graphics</key><string>x</string></dict>",
                "Graphics",
            ),
            (
                "<dict><key>Graphics</key><dict><key>Driver</key><true/></dict></dict>",
                "Graphics.Driver",
            ),
        ] {
            let a = config(text, &[]);
            assert_eq!(
                Config::read(&Bundle::parse(&a).unwrap()).unwrap_err(),
                config_key(want.into())
            );
        }
    }

    #[test]
    fn patches_count() {
        let p = Patch {
            identifier: String::new(),
            find: vec![1, 1],
            replace: vec![2, 2],
            count: 0,
        };
        let mut b = [1, 1, 1, 0, 1, 1];
        assert_eq!(apply(&p, &mut b, 0), 2);
        assert_eq!(b, [2, 2, 1, 0, 2, 2]);
        let mut b = [1, 1, 1, 0, 1, 1];
        assert_eq!(apply(&p, &mut b, 1), 1);
        assert_eq!(b, [2, 2, 1, 0, 1, 1]);
        for (find, replace) in [(vec![], vec![]), (vec![1], vec![2, 2])] {
            let p = Patch {
                find,
                replace,
                ..p.clone()
            };
            let mut b = [1, 1];
            assert_eq!(apply(&p, &mut b, 0), 0);
            assert_eq!(b, [1, 1]);
        }
    }
}
