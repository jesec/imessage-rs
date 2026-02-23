fn main() {
    println!("cargo:rerun-if-changed=swift/Sources");
    println!("cargo:rerun-if-changed=swift/Makefile");

    if std::env::consts::OS != "macos" {
        return;
    }

    let out_dir = std::env::var("OUT_DIR").expect("OUT_DIR not set");
    let swift_build_dir = format!("{out_dir}/swift-build");

    let status = std::process::Command::new("make")
        .arg("-C")
        .arg("swift")
        .arg(format!("BUILD_DIR={swift_build_dir}"))
        .arg(format!("OUT_DIR={swift_build_dir}"))
        .status()
        .expect("Failed to run make for imessage-helper dylib. Is Xcode installed?");

    assert!(status.success(), "Failed to build imessage-helper.dylib");

    // Tell rustc where to find the built dylib for include_bytes!()
    let dylib_path = format!("{swift_build_dir}/imessage-helper.dylib");
    println!("cargo:rustc-env=HELPER_DYLIB_PATH={dylib_path}");
}
