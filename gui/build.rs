fn main() {
    // asInvoker: the GUI runs unelevated (drag & drop, no UAC at launch);
    // device writes go through buildcast-cli spawned with the runas verb
    if std::env::var_os("CARGO_CFG_WINDOWS").is_some() {
        use embed_manifest::{embed_manifest, new_manifest};
        embed_manifest(new_manifest("BuildCast.App")).expect("failed to embed Windows manifest");

        winresource::WindowsResource::new()
            .set_icon("assets/buildcast.ico")
            .set("ProductName", "BuildCast")
            .set("FileDescription", "BuildCast disk image writer")
            .compile()
            .expect("failed to embed icon resource");
    }
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=assets/buildcast.ico");
}
