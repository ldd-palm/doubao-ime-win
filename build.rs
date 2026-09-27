use std::io::Result;

fn main() -> Result<()> {
    // Compile protobuf files
    prost_build::compile_protos(&["proto/asr.proto"], &["proto/"])?;

    // Tell Cargo to rerun if the proto file changes
    println!("cargo:rerun-if-changed=proto/asr.proto");

    // Embed the app icon as the .exe's resource icon (what Explorer/taskbar
    // show), separate from the tray/floating-button icons drawn at runtime.
    #[cfg(windows)]
    {
        println!("cargo:rerun-if-changed=assets/app_icon.ico");
        winres::WindowsResource::new()
            .set_icon("assets/app_icon.ico")
            .compile()?;
    }

    Ok(())
}
