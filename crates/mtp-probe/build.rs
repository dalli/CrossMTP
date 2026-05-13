fn main() {
    pkg_config::Config::new()
        .atleast_version("1.1.0")
        .probe("libmtp")
        .expect("libmtp not found via pkg-config. On macOS: `brew install libmtp`.");
    println!("cargo:rerun-if-changed=build.rs");
}
