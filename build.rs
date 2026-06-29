fn main() {
    // screencapturekit depends on Swift runtime at /usr/lib/swift/
    // Must be set here (not just .cargo/config.toml) because cargo install
    // from crates.io does not carry project-level .cargo/ config.
    let target = std::env::var("TARGET").unwrap();
    if target.contains("-apple-") {
        println!("cargo:rustc-link-arg=-Wl,-rpath,/usr/lib/swift");
    }
}
