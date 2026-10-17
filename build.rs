fn main() {
    // App icon (resource ID 1), shown by Explorer and loaded for the window class.
    embed_resource::compile("assets/rcap.rc", embed_resource::NONE)
        .manifest_optional()
        .unwrap();
    println!("cargo:rerun-if-changed=assets/rcap.ico");

    // Common Controls v6 so buttons/edits get the modern look.
    println!("cargo:rustc-link-arg-bins=/MANIFEST:EMBED");
    println!(
        "cargo:rustc-link-arg-bins=/MANIFESTDEPENDENCY:type='win32' name='Microsoft.Windows.Common-Controls' version='6.0.0.0' processorArchitecture='*' publicKeyToken='6595b64144ccf1df' language='*'"
    );
}
