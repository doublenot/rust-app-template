//! Embeds the Windows icon resource into the executable.
//!
//! A desktop shortcut, and Explorer generally, takes an application's icon from
//! the icon resource inside the `.exe`. rustc embeds none by default, so without
//! this the shortcut shows the generic application icon — even while the taskbar
//! and title bar look correct, because those come from the page favicon, which
//! is an unrelated mechanism.
//!
//! Nothing here affects Linux or macOS: their icons come from the `.desktop`
//! entry and the `.app` bundle, both of which cargo-packager generates from
//! `icons/icon.png`.

fn main() {
    println!("cargo:rerun-if-changed=icons/icon.ico");
    println!("cargo:rerun-if-changed=build.rs");

    // A build script is compiled and run on the HOST, so `cfg!(target_os)` here
    // reports the host's OS rather than the one being built for — cross-compiling
    // a Windows binary from Linux would silently take the wrong branch.
    // CARGO_CFG_TARGET_OS is the target, which is the question actually being
    // asked.
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        embed_windows_icon();
    }
}

/// Host is Windows: the build-dependency exists (it is gated on the host in
/// Cargo.toml) and so does a resource compiler.
#[cfg(windows)]
fn embed_windows_icon() {
    let mut res = winresource::WindowsResource::new();
    res.set_icon("icons/icon.ico");
    // Version and description are taken from Cargo.toml's [package] on their
    // own, which is what fills in the Windows file-properties dialog.
    if let Err(e) = res.compile() {
        // Not fatal. A missing resource compiler should cost the icon, not the
        // build: the program is identical either way.
        println!("cargo:warning=could not embed the Windows icon: {e}");
    }
}

/// Host is not Windows, so `winresource` was never pulled in. Reached only when
/// cross-compiling to Windows, where the result is a binary with no icon rather
/// than a failed build.
#[cfg(not(windows))]
fn embed_windows_icon() {
    println!("cargo:warning=cross-compiling to Windows; the executable will have no icon");
}
