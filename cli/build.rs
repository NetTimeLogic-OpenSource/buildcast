fn main() {
    // asInvoker: a CLI must not trigger UAC re-spawn into a new console;
    // it checks elevation itself and prints a hint
    if std::env::var_os("CARGO_CFG_WINDOWS").is_some() {
        use embed_manifest::{embed_manifest, new_manifest};
        embed_manifest(new_manifest("BuildCast.Cli")).expect("failed to embed Windows manifest");

        winresource::WindowsResource::new()
            .set_icon("../gui/assets/buildcast.ico")
            .set("ProductName", "BuildCast")
            .set("FileDescription", "BuildCast CLI disk image writer")
            .compile()
            .expect("failed to embed icon resource");
    }
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=../gui/assets/buildcast.ico");
}
