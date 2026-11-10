//! The icons the app draws: GPUI Kit's default set, plus the few it needs
//! beyond it (the header's Oyl Cloud glyphs).

use std::borrow::Cow;

use gpui_kit::{AssetSource, Result, SharedString};

gpui_kit::assets::icon_assets!(
    CloudIcons,
    [Cloud, CloudCheck, CloudOff, CloudAlert, CloudUpload]
);

/// GPUI Kit's default icons and Oyl's own.
#[derive(Clone, Copy, Debug, Default)]
pub struct OylAssets;

impl AssetSource for OylAssets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        if let Some(icon) = CloudIcons.load(path)? {
            return Ok(Some(icon));
        }
        gpui_kit::assets::Assets.load(path)
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        let mut paths = gpui_kit::assets::Assets.list(path)?;
        paths.extend(CloudIcons.list(path)?);
        Ok(paths)
    }
}
