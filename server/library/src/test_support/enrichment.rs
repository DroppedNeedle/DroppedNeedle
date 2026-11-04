//! Scripted enrichment answers.

use futures_util::future::BoxFuture;

use crate::management::enrich::*;
use crate::management::genre::Candidate;
use crate::matching::model::ReleaseExtras;

/// Scripted answers for the test suite.
#[derive(Default)]
pub struct ScriptedEnrichment {
    pub listenbrainz: Option<Vec<Candidate>>,
    pub lyrics: Option<LyricsFound>,
    pub cover: Option<Cover>,
    /// What the archive answers for the edition's own cover; `None`
    /// answers that it has none.
    pub edition: std::sync::Mutex<Option<Result<Option<Cover>, String>>>,
    /// The releases whose own cover was asked for.
    pub edition_asks: std::sync::Mutex<Vec<String>>,
    /// The credits MusicBrainz answers with; `None` while it is down.
    pub relations: std::sync::Mutex<Option<crate::matching::model::ReleaseRelations>>,
    /// The artist names MusicBrainz answers with; `None` while it is down.
    pub names: std::sync::Mutex<Option<crate::matching::model::ArtistNames>>,
    /// Release lookups asked for: the parts each wanted, and whether it
    /// was background.
    pub lookups: std::sync::Mutex<Vec<(ExtrasWanted, bool)>>,
    /// What AcoustID answers for every print; `None` while no key is set.
    pub acoustid: std::sync::Mutex<Option<ScriptedAcoustId>>,
    /// The prints AcoustID was asked about, one list per request.
    pub acoustid_calls: std::sync::Mutex<Vec<Vec<AcoustIdPrint>>>,
}

/// A scripted AcoustID.
#[derive(Debug, Clone)]
pub enum ScriptedAcoustId {
    /// These results for every print.
    Answers(Vec<droppedneedle_providers::acoustid::BatchResult>),
    /// AcoustID cannot be reached.
    Unavailable,
    /// AcoustID refuses the key.
    KeyRefused,
    /// AcoustID answers with this error code.
    Code(i64),
}

impl EnrichmentPort for ScriptedEnrichment {
    fn listenbrainz_genres<'a>(
        &'a self,
        _release_group_mbid: &'a str,
        _background: bool,
    ) -> BoxFuture<'a, Result<Vec<Candidate>, String>> {
        let answer = self
            .listenbrainz
            .clone()
            .ok_or_else(|| "not scripted".to_owned());
        Box::pin(async move { answer })
    }

    fn lastfm_genres<'a>(
        &'a self,
        _artist: &'a str,
        _album: &'a str,
        _background: bool,
    ) -> BoxFuture<'a, Result<Vec<Candidate>, String>> {
        Box::pin(async { Err("not scripted".to_owned()) })
    }

    fn lyrics<'a>(
        &'a self,
        _query: &'a LyricsQuery,
        _background: bool,
    ) -> BoxFuture<'a, Result<Option<LyricsFound>, String>> {
        let answer = Ok(self.lyrics.clone());
        Box::pin(async move { answer })
    }

    fn front_cover<'a>(
        &'a self,
        _release_mbid: &'a str,
        _release_group_mbid: &'a str,
        _background: bool,
    ) -> BoxFuture<'a, Result<Option<Cover>, String>> {
        let answer = Ok(self.cover.clone());
        Box::pin(async move { answer })
    }

    fn edition_cover<'a>(
        &'a self,
        release_mbid: &'a str,
        _background: bool,
    ) -> BoxFuture<'a, Result<Option<Cover>, String>> {
        if let Ok(mut asked) = self.edition_asks.lock() {
            asked.push(release_mbid.to_owned());
        }
        let answer = self
            .edition
            .lock()
            .ok()
            .and_then(|answer| answer.clone())
            .unwrap_or(Ok(None));
        Box::pin(async move { answer })
    }

    fn release_extras<'a>(
        &'a self,
        _release_mbid: &'a str,
        wants: ExtrasWanted,
        background: bool,
    ) -> BoxFuture<'a, Result<ReleaseExtras, String>> {
        if let Ok(mut lookups) = self.lookups.lock() {
            lookups.push((wants, background));
        }
        let relations = self.relations.lock().ok().and_then(|found| found.clone());
        let names = self.names.lock().ok().and_then(|found| found.clone());
        // One lookup: it answers every part asked for, or none.
        let answer = if (wants.relations && relations.is_none()) || (wants.names && names.is_none())
        {
            Err("MusicBrainz is down".to_owned())
        } else {
            Ok(ReleaseExtras {
                relations: relations.filter(|_| wants.relations),
                names: names.filter(|_| wants.names),
            })
        };
        Box::pin(async move { answer })
    }

    fn acoustid_lookup<'a>(
        &'a self,
        prints: &'a [AcoustIdPrint],
        _background: bool,
    ) -> BoxFuture<'a, AcoustIdAnswer> {
        use droppedneedle_providers::acoustid::Unanswered;
        let script = self.acoustid.lock().ok().and_then(|script| script.clone());
        if !prints.is_empty()
            && let Ok(mut calls) = self.acoustid_calls.lock()
        {
            calls.push(prints.to_vec());
        }
        let answer = match script {
            None => AcoustIdAnswer::NoKey,
            // A key is set: only a request learns what AcoustID says.
            Some(_) if prints.is_empty() => AcoustIdAnswer::Answered(Vec::new()),
            Some(ScriptedAcoustId::Unavailable) => AcoustIdAnswer::Unavailable(Unanswered::Failed),
            Some(ScriptedAcoustId::KeyRefused) => AcoustIdAnswer::KeyRefused,
            Some(ScriptedAcoustId::Code(code)) => {
                AcoustIdAnswer::Unavailable(Unanswered::Code(code))
            }
            Some(ScriptedAcoustId::Answers(results)) => {
                AcoustIdAnswer::Answered(vec![results; prints.len()])
            }
        };
        Box::pin(async move { answer })
    }
}
