// Embeds the icon and version info into the Windows .exe (does nothing on other systems).
fn main() {
    println!("cargo:rerun-if-changed=assets/analizator.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("assets/analizator.ico");
        res.set("ProductName", "Analizator");
        res.set("FileDescription", "Analizator - current & power analyzer");
        if let Err(e) = res.compile() {
            println!("cargo:warning=Windows icon not embedded: {e}");
        }
    }
}
