fn main() {
    println!("cargo:rerun-if-changed=assets/bandwidth-proxy.ico");
    #[cfg(windows)]
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        winresource::WindowsResource::new()
            .set_icon("assets/bandwidth-proxy.ico")
            .set("ProductName", "Bandwidth Proxy")
            .set("FileDescription", "Bandwidth Proxy")
            .compile()
            .expect("Failed to compile the Windows application icon resource");
    }
}
