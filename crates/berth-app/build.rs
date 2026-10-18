//! Embeds `assets/icons/berth/*.svg` (see src/assets.rs).
// A build script is a `main`: failing loudly is the right behavior.
#![allow(clippy::expect_used)]

use std::fmt::Write as _;
use std::path::Path;

fn main() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/icons/berth");
    println!("cargo:rerun-if-changed={}", dir.display());
    let mut names: Vec<String> = std::fs::read_dir(&dir)
        .expect("assets/icons/berth")
        .filter_map(|e| e.ok()?.file_name().into_string().ok())
        .filter(|n| n.ends_with(".svg"))
        .collect();
    names.sort();
    let mut out = String::from("&[\n");
    for n in names {
        let path = dir.join(&n);
        let _ = writeln!(
            out,
            "    (\"icons/berth/{n}\", include_bytes!({:?}).as_slice()),",
            path.display().to_string()
        );
    }
    out.push(']');
    let dest = Path::new(&std::env::var("OUT_DIR").expect("OUT_DIR")).join("berth_icons.rs");
    std::fs::write(dest, out).expect("write berth_icons.rs");
}
