//! Embeds the Windows manifest (current common controls, DPI awareness) and
//! version details. Skipped, with a warning, where no resource compiler is
//! installed, as when only checking the code on a Mac.

fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    println!("cargo:rerun-if-changed=res/portputer.manifest");
    let version = std::env::var("CARGO_PKG_VERSION").unwrap();
    let numeric = version.split(['.', '-']).take(3).chain(["0"]).collect::<Vec<_>>().join(",");
    let manifest = std::path::Path::new(&std::env::var("CARGO_MANIFEST_DIR").unwrap()).join("res/portputer.manifest");
    let rc = format!(
        r#"#include <winuser.h>
#include <winver.h>

CREATEPROCESS_MANIFEST_RESOURCE_ID RT_MANIFEST "{manifest}"

VS_VERSION_INFO VERSIONINFO
FILEVERSION     {numeric}
PRODUCTVERSION  {numeric}
FILEOS          VOS_NT_WINDOWS32
FILETYPE        VFT_APP
BEGIN
  BLOCK "StringFileInfo"
  BEGIN
    BLOCK "040904b0"
    BEGIN
      VALUE "FileDescription", "Portputer"
      VALUE "FileVersion", "{version}"
      VALUE "ProductName", "Portputer"
      VALUE "ProductVersion", "{version}"
      VALUE "OriginalFilename", "Portputer.exe"
      VALUE "LegalCopyright", "Apache License 2.0"
    END
  END
  BLOCK "VarFileInfo"
  BEGIN
    VALUE "Translation", 0x409, 1200
  END
END
"#,
        manifest = manifest.display().to_string().replace('\\', "/"),
    );
    let out = std::path::Path::new(&std::env::var("OUT_DIR").unwrap()).join("portputer.rc");
    std::fs::write(&out, rc).unwrap();
    if let Err(e) = embed_resource::compile(&out, embed_resource::NONE).manifest_optional() {
        println!("cargo:warning=no Windows resources embedded: {e}");
    }
}
