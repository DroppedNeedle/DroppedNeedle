//! Scripted cover art.

use futures_util::future::BoxFuture;

use crate::artwork::cache::ImageCredit;
use crate::artwork::covers::*;
use crate::local::stores::StoreError;

/// Scripted art source for tests. Entries are keyed exactly as the
/// handlers query them; anything missing reads as absent.
#[derive(Debug, Clone, Default)]
pub struct FakeCoverArt {
    release_groups: std::collections::HashMap<(String, Option<String>), CoverBytes>,
    albums: std::collections::HashMap<String, CoverBytes>,
    warming_release_groups: std::collections::HashSet<(String, Option<String>)>,
}

impl FakeCoverArt {
    /// Empty fake: every id misses and nothing warms.
    pub fn empty() -> Self {
        Self::default()
    }

    /// Preload one release-group cover.
    pub fn with_release_group(mut self, id: &str, size: Option<&str>, cover: CoverBytes) -> Self {
        self.release_groups
            .insert((id.to_owned(), size.map(str::to_owned)), cover);
        self
    }

    /// Mark one release-group cover as warming.
    pub fn warming_release_group(mut self, id: &str, size: Option<&str>) -> Self {
        self.warming_release_groups
            .insert((id.to_owned(), size.map(str::to_owned)));
        self
    }
}

impl CoverArt for FakeCoverArt {
    fn release_group_cover<'a>(
        &'a self,
        release_group_id: &'a str,
        size: Option<&'a str>,
    ) -> BoxFuture<'a, CoverLookup> {
        let key = (release_group_id.to_owned(), size.map(str::to_owned));
        let found = match self.release_groups.get(&key) {
            Some(cover) => CoverLookup::Found(cover.clone()),
            None if self.warming_release_groups.contains(&key) => CoverLookup::Warming,
            None => CoverLookup::Missing,
        };
        Box::pin(async move { found })
    }

    fn release_cover<'a>(
        &'a self,
        _release_id: &'a str,
        _size: Option<&'a str>,
    ) -> BoxFuture<'a, CoverLookup> {
        Box::pin(async { CoverLookup::Missing })
    }

    fn artist_image<'a>(
        &'a self,
        _artist_id: &'a str,
        _size_px: Option<u32>,
    ) -> BoxFuture<'a, CoverLookup> {
        Box::pin(async { CoverLookup::Missing })
    }

    fn artist_image_credit<'a>(
        &'a self,
        _artist_id: &'a str,
    ) -> BoxFuture<'a, Option<ImageCredit>> {
        Box::pin(async { None })
    }

    fn album_cover<'a>(
        &'a self,
        album_id: &'a str,
        _size: Option<&'a str>,
    ) -> BoxFuture<'a, Result<Option<CoverBytes>, StoreError>> {
        let found = self.albums.get(album_id).cloned();
        Box::pin(async move { Ok(found) })
    }

    fn local_album_art<'a>(
        &'a self,
        album_id: &'a str,
        _size: Option<&'a str>,
    ) -> BoxFuture<'a, Option<CoverBytes>> {
        let found = self.albums.get(album_id).cloned();
        Box::pin(async move { found })
    }

    fn local_artist_art<'a>(
        &'a self,
        _artist_id: &'a str,
        _size: Option<&'a str>,
    ) -> BoxFuture<'a, Option<CoverBytes>> {
        Box::pin(async { None })
    }
}
