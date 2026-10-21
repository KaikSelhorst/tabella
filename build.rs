//! Windows builds get version info and a manifest embedded in the .exe: executables without any
//! metadata are what antivirus heuristics flag most (Defender's `!ml` detections).

/// `{version}` becomes the package version as Windows wants it (four numbers).
const MANIFEST: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<assembly xmlns="urn:schemas-microsoft-com:asm.v1" manifestVersion="1.0">
  <assemblyIdentity type="win32" name="tabella" version="{version}"/>
  <trustInfo xmlns="urn:schemas-microsoft-com:asm.v3">
    <security>
      <requestedPrivileges>
        <requestedExecutionLevel level="asInvoker" uiAccess="false"/>
      </requestedPrivileges>
    </security>
  </trustInfo>
  <compatibility xmlns="urn:schemas-microsoft-com:compatibility.v1">
    <application>
      <!-- Windows 10 and 11 -->
      <supportedOS Id="{8e0f7a12-bfb3-4fe8-b9a5-48fd50a15a9a}"/>
    </application>
  </compatibility>
</assembly>
"#;

fn main() {
    println!("cargo::rerun-if-changed=assets/icon.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let version = format!(
        "{}.{}.{}.0",
        env!("CARGO_PKG_VERSION_MAJOR"),
        env!("CARGO_PKG_VERSION_MINOR"),
        env!("CARGO_PKG_VERSION_PATCH")
    );
    let manifest = MANIFEST.replace("{version}", &version);
    let mut res = winresource::WindowsResource::new();
    res.set("ProductName", "tabella")
        .set(
            "FileDescription",
            "tabella - lightweight PostgreSQL, MySQL and SQLite client",
        )
        .set("InternalName", "tabella")
        .set("OriginalFilename", "tabella.exe")
        .set_icon("assets/icon.ico")
        .set_manifest(&manifest);
    res.compile()
        .expect("Windows resources (is MinGW's windres installed?)");
}
