//! Shared skin + tile atlas loader for the offline and online windows.
//!
//! Asset roots are explicit map locations or the local DDNet data directory.
//! A missing/unsupported image is not fatal: the collision/silhouette view
//! stays available for maps downloaded from arbitrary community servers.

use std::path::{Path, PathBuf};

use crate::datafile::Datafile;
use crate::graphics::{Camera, Framebuffer};
use crate::map::GameMap;
use crate::map_visual::VisualMap;
use crate::snapshot_delta::SnapshotState;
use crate::texture::Texture;

#[derive(Debug, Default)]
pub struct WindowAssets {
    pub visual_map: Option<VisualMap>,
    pub default_skin: Option<Texture>,
}

pub fn candidate_roots(map_path: Option<&Path>) -> Vec<PathBuf> {
    let mut roots = Vec::new();
    if let Some(parent) = map_path
        .and_then(Path::parent)
        .and_then(Path::parent)
    {
        roots.push(parent.to_path_buf());
    }
    if let Some(root) = std::env::var_os("DDNET_DATA_DIR") {
        roots.push(PathBuf::from(root));
    }
    if let Ok(executable) = std::env::current_exe() {
        if let Some(bin) = executable.parent() {
            roots.push(bin.join("data"));
            if let Some(parent) = bin.parent() {
                roots.push(parent.join("data"));
            }
        }
    }
    roots.push(PathBuf::from("data"));
    roots.push(PathBuf::from("upstream/ddnet/data"));
    roots.dedup();
    roots
}

impl WindowAssets {
    /// Load image and skin assets from the matching DDNet data directory.
    /// Tries alternative roots only when the entire image set is available.
    pub fn from_datafile(file: &Datafile<'_>, map_path: Option<&Path>) -> Self {
        let mut assets = Self::default();
        for root in candidate_roots(map_path) {
            if assets.default_skin.is_none() {
                let skin_path = root.join("skins").join("default.png");
                if let Ok(data) = std::fs::read(skin_path) {
                    if let Ok(skin) = Texture::from_png(&data) {
                        if skin.width % 8 == 0 && skin.height % 4 == 0 {
                            assets.default_skin = Some(skin);
                        }
                    }
                }
            }
            if assets.visual_map.is_none() {
                if let Ok(visual) = VisualMap::from_datafile(file, &root) {
                    if !visual.layers.is_empty() {
                        assets.visual_map = Some(visual);
                    }
                }
            }
            if assets.default_skin.is_some() && assets.visual_map.is_some() {
                break;
            }
        }
        assets
    }

    /// In case a texture layer is unsupported, keep a collision fallback
    /// rather than crashing an interactive window in the render loop.
    pub fn draw_map(
        &self, frame: &mut Framebuffer, map: &GameMap, camera: Camera,
    ) -> Result<(), String> {
        if let Some(visual) = self.visual_map.as_ref() {
            if visual.render(frame, camera).is_ok() {
                return Ok(());
            }
        }
        frame.render_map(map, camera).map_err(|e| format!("collision map: {e:?}"))
    }

    pub fn draw_snapshot(
        &self, frame: &mut Framebuffer, snapshot: &SnapshotState, camera: Camera,
    ) -> Result<usize, String> {
        frame.render_snapshot_with_skin(snapshot, camera, self.default_skin.as_ref())
            .map_err(|e| format!("tee render: {e:?}"))
    }
}
