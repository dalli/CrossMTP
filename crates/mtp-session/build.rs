use std::env;
use std::path::PathBuf;

fn main() {
    let lib = pkg_config::Config::new()
        .atleast_version("1.1.0")
        .probe("libmtp")
        .expect("libmtp not found via pkg-config. On macOS: `brew install libmtp`.");

    let mut clang_args: Vec<String> = lib
        .include_paths
        .iter()
        .map(|p| format!("-I{}", p.display()))
        .collect();

    // Help bindgen on macOS find system headers.
    if cfg!(target_os = "macos") {
        if let Ok(sdk) = std::process::Command::new("xcrun")
            .args(["--show-sdk-path"])
            .output()
        {
            if sdk.status.success() {
                let path = String::from_utf8_lossy(&sdk.stdout).trim().to_string();
                if !path.is_empty() {
                    clang_args.push(format!("-isysroot{path}"));
                }
            }
        }
    }

    let bindings = bindgen::Builder::default()
        .header_contents("wrapper.h", "#include <stdlib.h>\n#include <libmtp.h>\n")
        .clang_args(&clang_args)
        .allowlist_function("LIBMTP_.*")
        .allowlist_type("LIBMTP_.*")
        .allowlist_var("LIBMTP_.*")
        .allowlist_function("free")
        .derive_default(true)
        .layout_tests(false)
        .generate()
        .expect("bindgen failed to generate libmtp bindings");

    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());
    bindings
        .write_to_file(out_dir.join("libmtp_sys.rs"))
        .expect("failed to write libmtp_sys.rs");

    println!("cargo:rerun-if-changed=build.rs");
}
