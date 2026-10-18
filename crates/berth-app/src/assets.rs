//! Assets: gpui-kit's icon set, Berth's own icons (`assets/icons/berth/`, drawn
//! from the design's Lucide glyphs at their exact stroke widths; ISC, see
//! LICENSE-LUCIDE there) and the bundled fonts (OFL, see `assets/fonts/`).

use std::borrow::Cow;

use gpui::{AssetSource, Result, SharedString};

const ICONS: &[(&str, &[u8])] = include!(concat!(env!("OUT_DIR"), "/berth_icons.rs"));

const FONTS: &[&[u8]] = &[
    include_bytes!("../assets/fonts/IBMPlexSans-Regular.ttf"),
    include_bytes!("../assets/fonts/IBMPlexSans-Medium.ttf"),
    include_bytes!("../assets/fonts/IBMPlexSans-SemiBold.ttf"),
    include_bytes!("../assets/fonts/IBMPlexSans-Bold.ttf"),
    include_bytes!("../assets/fonts/JetBrainsMono-Regular.ttf"),
    include_bytes!("../assets/fonts/JetBrainsMono-Medium.ttf"),
    include_bytes!("../assets/fonts/JetBrainsMono-SemiBold.ttf"),
    include_bytes!("../assets/fonts/JetBrainsMono-Bold.ttf"),
    include_bytes!("../assets/fonts/JetBrainsMono-Italic.ttf"),
];

/// IBM Plex Sans and JetBrains Mono, for `TextSystem::add_fonts`.
pub fn fonts() -> Vec<Cow<'static, [u8]>> {
    FONTS.iter().map(|f| Cow::Borrowed(*f)).collect()
}

pub struct Assets;

impl AssetSource for Assets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        match ICONS.iter().find(|(p, _)| *p == path) {
            Some((_, bytes)) => Ok(Some(Cow::Borrowed(bytes))),
            None => gpui_kit_assets::Assets.load(path),
        }
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        let mut all = gpui_kit_assets::Assets.list(path)?;
        all.extend(
            ICONS
                .iter()
                .filter(|(p, _)| p.starts_with(path))
                .map(|(p, _)| SharedString::from(*p)),
        );
        Ok(all)
    }
}
