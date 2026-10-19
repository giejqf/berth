//! Renders Berth's SVG artwork to PNG for packaging (`scripts/icon.sh`,
//! `scripts/dmg.sh`):
//!
//! ```text
//! berth-svg png <in.svg> <out.png> <width>   # height follows the aspect ratio
//! berth-svg iconset <in.svg> <out.iconset>   # the sizes `iconutil` wants
//! ```

use std::path::Path;

use anyhow::{Context as _, Result, bail};
use resvg::{tiny_skia, usvg};

/// The app's UI fonts, for text in the artwork.
const FONTS: [&[u8]; 2] = [
    include_bytes!("../../berth-app/assets/fonts/IBMPlexSans-Regular.ttf"),
    include_bytes!("../../berth-app/assets/fonts/IBMPlexSans-Medium.ttf"),
];

/// `iconutil`'s iconset: (point size, scale).
const ICONSET: [(u32, u32); 10] = [
    (16, 1),
    (16, 2),
    (32, 1),
    (32, 2),
    (128, 1),
    (128, 2),
    (256, 1),
    (256, 2),
    (512, 1),
    (512, 2),
];

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.iter().map(String::as_str).collect::<Vec<_>>()[..] {
        ["png", svg, out, width] => {
            let tree = load(Path::new(svg))?;
            let width = width.parse().context("width")?;
            write(&render(&tree, width)?, Path::new(out))
        }
        ["iconset", svg, out] => {
            let tree = load(Path::new(svg))?;
            let dir = Path::new(out);
            std::fs::create_dir_all(dir).with_context(|| dir.display().to_string())?;
            for (size, scale) in ICONSET {
                let name = if scale == 1 {
                    format!("icon_{size}x{size}.png")
                } else {
                    format!("icon_{size}x{size}@{scale}x.png")
                };
                write(&render(&tree, size * scale)?, &dir.join(name))?;
            }
            Ok(())
        }
        _ => bail!(
            "usage: berth-svg png <in.svg> <out.png> <width> | iconset <in.svg> <out.iconset>"
        ),
    }
}

fn load(path: &Path) -> Result<usvg::Tree> {
    let data = std::fs::read(path).with_context(|| path.display().to_string())?;
    // `<image href>` paths resolve next to the file; text uses the app's fonts.
    let mut options = usvg::Options {
        resources_dir: path.parent().map(Path::to_path_buf),
        ..usvg::Options::default()
    };
    for font in FONTS {
        options.fontdb_mut().load_font_data(font.to_vec());
    }
    usvg::Tree::from_data(&data, &options).with_context(|| format!("parse {}", path.display()))
}

/// The whole drawing scaled to `width` pixels wide.
fn render(tree: &usvg::Tree, width: u32) -> Result<tiny_skia::Pixmap> {
    let size = tree.size();
    let scale = width as f32 / size.width();
    let height = (size.height() * scale).round() as u32;
    let mut pixmap = tiny_skia::Pixmap::new(width, height).context("empty image")?;
    resvg::render(
        tree,
        tiny_skia::Transform::from_scale(scale, scale),
        &mut pixmap.as_mut(),
    );
    Ok(pixmap)
}

fn write(pixmap: &tiny_skia::Pixmap, path: &Path) -> Result<()> {
    pixmap
        .save_png(path)
        .with_context(|| path.display().to_string())
}
