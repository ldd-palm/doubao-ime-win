use std::io::Result;

fn main() -> Result<()> {
    // Compile protobuf files
    prost_build::compile_protos(&["proto/asr.proto"], &["proto/"])?;

    // Tell Cargo to rerun if the proto file changes
    println!("cargo:rerun-if-changed=proto/asr.proto");

    // Embed the app icon as the .exe's resource icon (what Explorer/taskbar
    // show), separate from the tray/floating-button icons drawn at runtime.
    // Also embed a manifest requesting Common Controls v6: without it,
    // Windows loads the old v5 comctl32.dll, which doesn't export
    // TaskDialogIndirect (used by the Help dialog's clickable link) — the
    // .exe would fail to even start, not just fail that one call.
    #[cfg(windows)]
    {
        println!("cargo:rerun-if-changed=assets/app_icon.ico");
        winres::WindowsResource::new()
            .set_icon("assets/app_icon.ico")
            .set_manifest(
                r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<assembly xmlns="urn:schemas-microsoft-com:asm.v1" manifestVersion="1.0">
  <dependency>
    <dependentAssembly>
      <assemblyIdentity
        type="win32"
        name="Microsoft.Windows.Common-Controls"
        version="6.0.0.0"
        processorArchitecture="*"
        publicKeyToken="6595b64144ccf1df"
        language="*"
      />
    </dependentAssembly>
  </dependency>
</assembly>
"#,
            )
            .compile()?;
    }

    Ok(())
}
