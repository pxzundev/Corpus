use std::env;
use std::fs;
use std::path::PathBuf;

/// The MCP server travels inside the app bundle as a resource, so the GUI can
/// place it in the shared data folder without any installer. The bundler needs
/// the file on disk, so this build script copies the freshly built binary into
/// `resources/` (gitignored) before the bundle step runs. Nothing is copied
/// when the binary has not been built yet — the GUI falls back to a sibling
/// lookup in that case.
fn main() {
    let manifest = PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    let name = match env::var("CARGO_CFG_TARGET_OS").as_deref() {
        Ok("windows") => "corpus-mcp.exe",
        _ => "corpus-mcp",
    };
    let source = manifest.join("../../target/release").join(name);
    if !source.exists() {
        return;
    }
    let dest_dir = manifest.join("resources");
    fs::create_dir_all(&dest_dir).expect("could not create the GUI resource dir");
    let dest = dest_dir.join(name);
    let fresh = |path: &PathBuf| fs::metadata(path).and_then(|m| m.modified()).ok();
    if let (Some(dest_time), Some(source_time)) = (fresh(&dest), fresh(&source)) {
        if dest_time >= source_time {
            return;
        }
    }
    fs::copy(&source, &dest).expect("could not copy corpus-mcp into the GUI resources dir");
    println!("cargo:rerun-if-changed={}", source.display());

    tauri_build::build();
}
