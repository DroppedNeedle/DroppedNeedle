//! The edition plan as other parts of the server reach it.

use std::future::Future;
use std::pin::Pin;

use super::models::OperationError;
use super::plan::EditionPlan;
use super::service::Operations;

/// Boxed future for dyn-compatible methods.
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// The plan for an album's chosen edition, looked up by release group:
/// the seam "get missing tracks" uses to fetch only what the edition
/// lacks. Acquisition is built after the library, so wiring plugs the
/// library's [`Operations`] in once both exist.
pub trait EditionPlans: Send + Sync {
    /// The plan for one library copy of the release group: `album_id`
    /// when the caller names the copy (the edition picker does, so the
    /// plan it showed is the plan fetched), else a chosen copy first, then
    /// the oldest. `None` when the library holds no copy with an edition.
    fn group_plan<'a>(
        &'a self,
        album_id: Option<&'a str>,
        release_group_mbid: &'a str,
    ) -> BoxFuture<'a, Result<Option<EditionPlan>, OperationError>>;
}

impl EditionPlans for Operations {
    fn group_plan<'a>(
        &'a self,
        album_id: Option<&'a str>,
        release_group_mbid: &'a str,
    ) -> BoxFuture<'a, Result<Option<EditionPlan>, OperationError>> {
        Box::pin(async move {
            let album_id = match album_id {
                Some(album_id) => Some(album_id.to_owned()),
                None => self.album_for_group(release_group_mbid).await?,
            };
            let Some(album_id) = album_id else {
                return Ok(None);
            };
            self.edition_plan(&album_id, None).await.map(Some)
        })
    }
}
