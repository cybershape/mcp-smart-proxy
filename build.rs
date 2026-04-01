#[cfg(target_os = "macos")]
fn main() {
    use std::env;
    use std::path::PathBuf;
    use std::process::Command;

    let source_path = PathBuf::from("swift/input_popup/main.swift");
    println!("cargo:rerun-if-changed={}", source_path.display());
    println!("cargo:rerun-if-changed=build.rs");

    let out_dir = PathBuf::from(
        env::var_os("OUT_DIR").expect("OUT_DIR is not set while building the popup helper"),
    );
    let helper_path = out_dir.join("msp-popup-input-helper");

    let output = Command::new("xcrun")
        .arg("swiftc")
        .arg(&source_path)
        .arg("-o")
        .arg(&helper_path)
        .output()
        .expect("failed to run `xcrun swiftc` for the popup helper");

    if !output.status.success() {
        panic!(
            "failed to compile the macOS popup helper with `xcrun swiftc`\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    println!(
        "cargo:rustc-env=MSP_POPUP_HELPER_BINARY={}",
        helper_path.display()
    );
}

#[cfg(not(target_os = "macos"))]
fn main() {
    println!("cargo:rerun-if-changed=build.rs");
}
