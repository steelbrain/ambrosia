# hello-kext

A minimal kext that Ambrosia adds to a macOS guest's kernel, and the
example for [adding a kext](../../docs/how-to/add-a-kext.md). Its bundle
identifier is `ai.aneesiqbal.ambrosia.example.hello`.

When the kernel loads it, its start routine logs
`ai.aneesiqbal.ambrosia.example.hello: loaded`. Its one IOKit personality
matches `IOResources`, so the kernel then creates an
`ai_aneesiqbal_ambrosia_example_Hello` service, whose `start` logs
`ai.aneesiqbal.ambrosia.example.hello: Hello, world` and sets the service's
`Greeting` property to `Hello, world`.

## Files

- `Hello.cpp`: the kext. It holds the `ai_aneesiqbal_ambrosia_example_Hello`
  class, a subclass of `IOService`, and the kext's `kmod_info`, which Xcode
  would otherwise generate.
- `Info.plist`: the kext's Info.plist, with its personality and the two
  KPIs it links against, `com.apple.kpi.iokit` and `com.apple.kpi.libkern`.
- `build.sh`: builds `Hello.kext` with the macOS SDK.
- `config.plist`: an Ambrosia configuration that adds the kext from
  `Kexts/Hello.kext` in the bundle.

## Building

On a Mac with Xcode or its Command Line Tools:

```sh
examples/hello-kext/build.sh
```

This writes `examples/hello-kext/build/Hello.kext`. Give a directory as the
first argument to write `Hello.kext` there instead. The bundle holds two
files:

```text
Hello.kext/Contents/Info.plist
Hello.kext/Contents/MacOS/Hello
```

`Hello` is an arm64e kext bundle with chained fixups. Its calls into the
kernel are left as imports by name, which Ambrosia binds when it boots the
guest, so the build needs no Kernel Debug Kit.

## Bundling

Put the configuration and the kext in one archive:

```sh
ambrosia-bundle create --config examples/hello-kext/config.plist \
  --kext examples/hello-kext/build/Hello.kext --output hello.cpio
```

`ambrosia-bundle` is built from
[`crates/ambrosia-bundle`](../../crates/ambrosia-bundle/README.md), with
`cargo build --release -p ambrosia-bundle`. It writes `config.plist` first
and the kext's files under `Kexts/Hello.kext/`, and checks the bundle as
the firmware will read it.

From the repository's root, `make hello-kext-bundle` builds the kext into
`target/hello-kext/Hello.kext` and the bundle as `target/hello-kext.cpio`.

## Running

Boot a guest with the bundle ([running a guest](../../docs/running.md)):

```sh
scripts/run-guest.sh -e "$ECID" -b hello.cpio path/to/vm
```

Then, in the guest:

```sh
kmutil showloaded --bundle-identifier ai.aneesiqbal.ambrosia.example.hello
ioreg -r -c ai_aneesiqbal_ambrosia_example_Hello
/usr/bin/log show --last boot --style syslog --predicate 'process == "kernel" AND eventMessage CONTAINS "ai.aneesiqbal.ambrosia.example.hello"'
csrutil status
```

`kmutil` lists `ai.aneesiqbal.ambrosia.example.hello (1.0.0)`, `ioreg` shows the
service with `"Greeting" = "Hello, world"`, the log has the kext's two
lines, and `csrutil` reports System Integrity Protection enabled.
[Adding a kext](../../docs/how-to/add-a-kext.md#checking-the-guest) shows
the whole output of each.

## Documentation

- [Adding a kext](../../docs/how-to/add-a-kext.md): what a kext needs to
  be placed, and how to build, bundle and boot one.
- [Configuration](../../docs/configuration.md#kernel--add): the `Kernel` >
  `Add` key.

Ambrosia is licensed under GPL-2.0-or-later OR LGPL-3.0-or-later; see
[LICENSE](../../LICENSE).
