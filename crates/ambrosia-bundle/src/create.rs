//! Builds a bundle from files on disk: config.plist first, then each driver
//! module at `Drivers/<file name>` and each kext's bundle directory at
//! `Kexts/<name>.kext`, in sorted order, then the trailer.

use crate::Error;
use crate::check::{self, Summary};
use crate::newc::{self, Entry};
use ambrosia::bundle;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

/// What goes into a bundle.
#[derive(Debug, Clone, Default)]
pub struct Inputs {
    /// The configuration, which goes in as `config.plist` whatever its name.
    pub config: PathBuf,
    /// Kext bundle directories, each of which goes in at `Kexts/<name>.kext`.
    pub kexts: Vec<PathBuf>,
    /// Graphics driver modules, each of which goes in at
    /// `Drivers/<file name>`.
    pub drivers: Vec<PathBuf>,
}

/// The bundle `inputs` make, checked as [`check::check`] checks one, and
/// what the firmware will find in it.
pub fn create(inputs: &Inputs) -> Result<(Vec<u8>, Summary), Error> {
    let mut files = Files::default();
    let config = files.read(&inputs.config)?;

    let mut entries = Vec::new();
    if !inputs.drivers.is_empty() {
        entries.push(Entry::dir("Drivers"));
    }
    for d in &inputs.drivers {
        let at = format!("Drivers/{}", file_name(d)?);
        let executable = is_executable(d)?;
        entries.push(Entry::file(at, executable, files.read(d)?));
    }
    if !inputs.kexts.is_empty() {
        entries.push(Entry::dir("Kexts"));
    }
    for k in &inputs.kexts {
        let name = file_name(k)?;
        if !name.ends_with(".kext") || !k.is_dir() {
            return Err(Error::NotAKext(k.clone()));
        }
        if !k.join("Contents/Info.plist").is_file() {
            return Err(Error::NoInfoPlist(k.clone()));
        }
        files.walk(k, &format!("Kexts/{name}"), &mut entries)?;
    }

    // Sorted by path, part by part, so that a directory comes just before
    // what it holds, and the order is the same on every machine.
    entries.sort_by(|a, b| a.path.split('/').cmp(b.path.split('/')));
    if let Some(w) = entries.windows(2).find(|w| w[0].path == w[1].path) {
        return Err(Error::Duplicate(w[0].path.clone()));
    }
    entries.insert(0, Entry::file(bundle::CONFIG, false, config));

    let archive = newc::write(&entries);
    let summary = check::check(&archive)?;
    Ok((archive, summary))
}

/// A file or directory's name, which must be UTF-8 to be an archive path.
fn file_name(path: &Path) -> Result<&str, Error> {
    // A path such as `x.kext/` or `./x.kext` names `x.kext`.
    let name = path
        .components()
        .next_back()
        .map(|c| c.as_os_str())
        .ok_or_else(|| Error::NotAFile(path.into()))?;
    name.to_str().ok_or_else(|| Error::NotUtf8(path.into()))
}

/// Reads the inputs, keeping count of their bytes so that a bundle over the
/// size limit is refused before it is read.
#[derive(Default)]
struct Files {
    total: u64,
}

impl Files {
    /// The contents of `path`, a regular file or a link to one.
    fn read(&mut self, path: &Path) -> Result<Vec<u8>, Error> {
        let meta = fs::metadata(path).map_err(|error| io(path, error))?;
        if !meta.is_file() {
            return Err(Error::NotAFile(path.into()));
        }
        self.total += meta.len();
        if self.total > bundle::MAX_LEN as u64 {
            return Err(Error::TooBig(self.total));
        }
        fs::read(path).map_err(|error| io(path, error))
    }

    /// Adds the directory `dir`, at `at` in the archive, and everything in
    /// it, to `out`. Links and other special files are refused: the firmware
    /// reads only regular files.
    fn walk(&mut self, dir: &Path, at: &str, out: &mut Vec<Entry>) -> Result<(), Error> {
        out.push(Entry::dir(at));
        let names = fs::read_dir(dir)
            .and_then(|d| {
                d.map(|e| e.map(|e| e.file_name()))
                    .collect::<Result<Vec<_>, _>>()
            })
            .map_err(|error| io(dir, error))?;
        for name in names {
            let path = dir.join(&name);
            let name = name.to_str().ok_or_else(|| Error::NotUtf8(path.clone()))?;
            let inner = format!("{at}/{name}");
            let meta = fs::symlink_metadata(&path).map_err(|error| io(&path, error))?;
            if meta.is_dir() {
                self.walk(&path, &inner, out)?;
            } else if meta.is_file() {
                let executable = meta.permissions().mode() & 0o100 != 0;
                out.push(Entry::file(inner, executable, self.read(&path)?));
            } else {
                return Err(Error::NotFileOrDir(path));
            }
        }
        Ok(())
    }
}

/// Whether `path` has its user-execute bit set, which gives it mode 0755 in
/// the archive rather than 0644.
fn is_executable(path: &Path) -> Result<bool, Error> {
    let meta = fs::metadata(path).map_err(|error| io(path, error))?;
    Ok(meta.permissions().mode() & 0o100 != 0)
}

fn io(path: &Path, error: std::io::Error) -> Error {
    Error::Io {
        path: path.into(),
        error,
    }
}
