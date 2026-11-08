fn main() {
    slint_build::compile("ui/app.slint").expect("Slint UI compilation failed");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        winresource::WindowsResource::new()
            .set("ProductName", "Application Pre-Installer")
            .set("FileDescription", "Windows setup assistant")
            .set_manifest_file("assets/app.manifest")
            .compile()
            .expect("Windows resource compilation failed");
    }
}
