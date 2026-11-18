//! Windows targets only: embed the app icon and version info into
//! MdNotePad.exe (`windows/mdnotepad.rc`, icon `windows/mdnotepad.ico`
//! from `tools/build-icon.sh`).  Without a resource compiler the
//! exe is built without them (cargo prints a warning); other targets skip
//! this entirely.

use std::{env, path::PathBuf};

fn main() {
    println!("cargo:rerun-if-changed=windows/mdnotepad.rc");
    println!("cargo:rerun-if-changed=windows/mdnotepad.ico");
    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }

    let version = env::var("CARGO_PKG_VERSION").unwrap_or_default();
    let mut numbers = version
        .split(['.', '-', '+'])
        .map(|part| part.parse::<u16>().unwrap_or(0));
    let mut next = || numbers.next().unwrap_or(0);
    let (major, minor, patch) = (next(), next(), next());
    let macros = [
        format!("RUSTDOWN_VERSION_MAJOR={major}"),
        format!("RUSTDOWN_VERSION_MINOR={minor}"),
        format!("RUSTDOWN_VERSION_PATCH={patch}"),
        format!("RUSTDOWN_VERSION={version}"),
    ];
    // The icon is looked up through the include path, which works the
    // same for windres, llvm-rc and rc.exe.
    let dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap_or_default()).join("windows");
    let result = embed_resource::compile(
        dir.join("mdnotepad.rc"),
        embed_resource::ParamsMacrosAndIncludeDirs(&macros, [&dir]),
    );
    if let Err(err) = result.manifest_required() {
        println!("cargo:warning=MdNotePad.exe built without icon and version info: {err}");
    }
}
