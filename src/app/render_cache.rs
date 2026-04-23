use crate::cache::CacheWeight;
use eframe::egui::TextureHandle;

#[derive(Clone)]
pub(super) struct CachedTexture {
    pub(super) texture: TextureHandle,
    pub(super) byte_estimate: usize,
}

impl CacheWeight for CachedTexture {
    fn cache_weight_bytes(&self) -> usize {
        self.byte_estimate
    }
}
