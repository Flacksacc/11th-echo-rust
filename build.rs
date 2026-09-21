fn main() {
    println!("cargo:rerun-if-env-changed=ECHO_UPDATE_FEED_URL");
    println!("cargo:rerun-if-env-changed=ECHO_UPDATE_PUBLIC_KEY");
    slint_build::compile("ui/appwindow.slint").unwrap();

    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let mut resource = winresource::WindowsResource::new();
        resource.set_icon("eleventhecho.ico");
        resource.set("ProductName", "Echo");
        resource.set("FileDescription", "Echo speech-to-text assistant");
        resource.set("ProductVersion", env!("CARGO_PKG_VERSION"));
        resource.set("FileVersion", env!("CARGO_PKG_VERSION"));
        resource.set("LegalCopyright", "Copyright (c) Echo contributors");
        resource.compile().unwrap();
    }
}
