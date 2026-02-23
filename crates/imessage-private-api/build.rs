fn main() {
    println!("cargo:rerun-if-changed=swift/Sources");
    println!("cargo:rerun-if-changed=swift/Makefile");

    if std::env::consts::OS != "macos" {
        return;
    }

    let status = std::process::Command::new("make")
        .arg("-C")
        .arg("swift")
        .status()
        .expect("Failed to run make for imessage-helper dylib. Is Xcode installed?");

    assert!(status.success(), "Failed to build imessage-helper.dylib");

    // Tell rustc where to find the built dylib for include_bytes!()
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").unwrap();
    let dylib_path = format!("{manifest_dir}/swift/.build/imessage-helper.dylib");
    println!("cargo:rustc-env=HELPER_DYLIB_PATH={dylib_path}");
}
