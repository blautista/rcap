use std::path::PathBuf;
use std::{env, fs};

fn main() {
    // Windows resources: the app icon (ID 1, shown by Explorer and loaded for
    // the window class) and version info (product name and version, required
    // for code signing), generated from Cargo.toml.
    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let icon = manifest_dir.join("assets").join("rcap.ico");
    let version = env::var("CARGO_PKG_VERSION").unwrap();
    let numeric = format!(
        "{},{},{},0",
        env::var("CARGO_PKG_VERSION_MAJOR").unwrap(),
        env::var("CARGO_PKG_VERSION_MINOR").unwrap(),
        env::var("CARGO_PKG_VERSION_PATCH").unwrap()
    );
    let description = env::var("CARGO_PKG_DESCRIPTION").unwrap();
    let rc = format!(
        r#"1 ICON "{icon}"

1 VERSIONINFO
FILEVERSION {numeric}
PRODUCTVERSION {numeric}
FILEOS 0x40004
FILETYPE 0x1
BEGIN
  BLOCK "StringFileInfo"
  BEGIN
    BLOCK "040904B0"
    BEGIN
      VALUE "FileDescription", "{description}"
      VALUE "FileVersion", "{version}"
      VALUE "InternalName", "rcap"
      VALUE "LegalCopyright", "MIT License"
      VALUE "OriginalFilename", "rcap.exe"
      VALUE "ProductName", "rcap"
      VALUE "ProductVersion", "{version}"
    END
  END
  BLOCK "VarFileInfo"
  BEGIN
    VALUE "Translation", 0x409, 1200
  END
END
"#,
        icon = icon.display().to_string().replace('\\', "\\\\"),
    );
    let rc_path = PathBuf::from(env::var("OUT_DIR").unwrap()).join("rcap.rc");
    fs::write(&rc_path, rc).unwrap();
    embed_resource::compile(&rc_path, embed_resource::NONE)
        .manifest_optional()
        .unwrap();
    println!("cargo:rerun-if-changed=assets/rcap.ico");

    // Common Controls v6 so buttons/edits get the modern look.
    println!("cargo:rustc-link-arg-bins=/MANIFEST:EMBED");
    println!(
        "cargo:rustc-link-arg-bins=/MANIFESTDEPENDENCY:type='win32' name='Microsoft.Windows.Common-Controls' version='6.0.0.0' processorArchitecture='*' publicKeyToken='6595b64144ccf1df' language='*'"
    );
}
