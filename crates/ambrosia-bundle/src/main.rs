//! `ambrosia-bundle`: builds, checks and lists Ambrosia's configuration
//! bundles. Run `ambrosia-bundle --help` for its commands.

use ambrosia_bundle::{Error, Inputs, Summary};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

const USAGE: &str = "\
usage: ambrosia-bundle create --config FILE [--kext DIR]... [--driver FILE]... --output FILE
       ambrosia-bundle check BUNDLE
       ambrosia-bundle list BUNDLE

create  Builds a bundle: FILE as config.plist, first; each kext's bundle
        directory at Kexts/<name>.kext; each driver module at
        Drivers/<file name>. It checks the bundle as check does, and writes
        it only if it passes.
check   Checks that Ambrosia would boot with BUNDLE, and says what it would
        find there.
list    Lists the files of BUNDLE that Ambrosia reads, each with its size in
        bytes.
";

/// A command line that isn't one of the forms [`USAGE`] gives.
struct Usage(String);

fn main() -> ExitCode {
    let args: Vec<OsString> = std::env::args_os().skip(1).collect();
    match run(&args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(Failure::Usage(Usage(why))) => {
            eprintln!("ambrosia-bundle: {why}\n\n{USAGE}");
            ExitCode::from(2)
        }
        Err(Failure::Bundle(e)) => {
            eprintln!("ambrosia-bundle: {e}");
            ExitCode::FAILURE
        }
    }
}

enum Failure {
    Usage(Usage),
    Bundle(Error),
}

impl From<Usage> for Failure {
    fn from(u: Usage) -> Failure {
        Failure::Usage(u)
    }
}

impl From<Error> for Failure {
    fn from(e: Error) -> Failure {
        Failure::Bundle(e)
    }
}

fn run(args: &[OsString]) -> Result<(), Failure> {
    let Some((command, rest)) = args.split_first() else {
        return Err(Usage("no command given".into()).into());
    };
    match command.to_str() {
        Some("-h" | "--help" | "help") => {
            print!("{USAGE}");
            Ok(())
        }
        Some("create") => {
            let (inputs, output) = create_args(rest)?;
            let (archive, summary) = ambrosia_bundle::create(&inputs)?;
            std::fs::write(&output, &archive).map_err(|error| Error::Io {
                path: output.clone(),
                error,
            })?;
            print_summary(&output, &summary);
            Ok(())
        }
        Some("check") => {
            let path = one_path(rest)?;
            let summary = ambrosia_bundle::check(&read(&path)?)?;
            print_summary(&path, &summary);
            Ok(())
        }
        Some("list") => {
            let path = one_path(rest)?;
            let archive = read(&path)?;
            let bundle = ambrosia_bundle::read(&archive)?;
            let width = bundle
                .files
                .iter()
                .map(|f| f.data.len().to_string().len())
                .max()
                .unwrap_or(1);
            for f in &bundle.files {
                println!("{:>width$}  {}", f.data.len(), f.path);
            }
            println!(
                "{}, {} bytes",
                count(bundle.files.len(), "file", "files"),
                bundle.len
            );
            Ok(())
        }
        _ => Err(Usage(format!("unknown command {}", command.to_string_lossy())).into()),
    }
}

/// `create`'s options: the inputs and the output.
fn create_args(args: &[OsString]) -> Result<(Inputs, PathBuf), Usage> {
    let mut inputs = Inputs::default();
    let (mut config, mut output) = (None, None);
    let mut args = args.iter();
    while let Some(option) = args.next() {
        let mut value = || {
            args.next().map(PathBuf::from).ok_or_else(|| {
                Usage(format!(
                    "{} needs a path after it",
                    option.to_string_lossy()
                ))
            })
        };
        match option.to_str() {
            Some("--config") => config = Some(value()?),
            Some("--kext") => inputs.kexts.push(value()?),
            Some("--driver") => inputs.drivers.push(value()?),
            Some("--output") => output = Some(value()?),
            _ => {
                return Err(Usage(format!(
                    "unknown option {} for create",
                    option.to_string_lossy()
                )));
            }
        }
    }
    inputs.config = config.ok_or_else(|| Usage("create needs --config".into()))?;
    let output = output.ok_or_else(|| Usage("create needs --output".into()))?;
    Ok((inputs, output))
}

/// The one path `check` and `list` take.
fn one_path(args: &[OsString]) -> Result<PathBuf, Usage> {
    match args {
        [path] => Ok(PathBuf::from(path)),
        [] => Err(Usage("no bundle given".into())),
        _ => Err(Usage("give one bundle".into())),
    }
}

fn read(path: &Path) -> Result<Vec<u8>, Error> {
    std::fs::read(path).map_err(|error| Error::Io {
        path: path.into(),
        error,
    })
}

/// `n` and the noun for it, such as "1 kext" or "2 kexts".
fn count(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

/// What the firmware will find in the bundle at `path`, in the terms its
/// boot log uses.
fn print_summary(path: &Path, s: &Summary) {
    println!(
        "{}: {} bytes, {}, {}, {}, graphics driver {}",
        path.display(),
        s.len,
        count(s.files, "file", "files"),
        count(s.kexts.len(), "kext", "kexts"),
        count(s.patches, "patch", "patches"),
        s.driver.as_deref().unwrap_or("none")
    );
    for k in &s.kexts {
        println!("  kext {} at {}", k.id, k.path);
    }
}
