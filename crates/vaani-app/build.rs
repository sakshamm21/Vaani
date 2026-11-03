//! Windows: put the app icon (and the recording icon the tray switches to) and version
//! details into vaani.exe. Nothing to do elsewhere.

fn main() {
    #[cfg(windows)]
    {
        let mut res = winresource::WindowsResource::new();
        res.set_icon_with_id("../../assets/windows/vaani.ico", "1")
            .set_icon_with_id("../../assets/windows/vaani-recording.ico", "2")
            .set("ProductName", "Vaani")
            .set("FileDescription", "Vaani voice dictation")
            .set("CompanyName", "sakshamm21")
            .set("LegalCopyright", "MIT License");
        if let Err(e) = res.compile() {
            panic!("could not embed the Windows resources: {e}");
        }
    }
    println!("cargo:rerun-if-changed=../../assets/windows");
}
