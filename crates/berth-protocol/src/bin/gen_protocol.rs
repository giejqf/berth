//! `just gen-protocol`: write `sidecar/src/protocol.ts`.

use std::path::PathBuf;

fn main() -> std::io::Result<()> {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let path = root.join(berth_protocol::TS_OUTPUT);
    std::fs::write(&path, berth_protocol::typescript())?;
    println!("wrote {}", path.display());
    Ok(())
}
