//! Values exchanged by two signed-in players during a local queue handoff.

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

#[derive(Clone, Debug, Deserialize, Serialize, ToSchema)]
pub struct HandoffTrack {
    /// Local library track ID. Each player constructs its own authenticated stream address.
    pub id: String,
    pub title: String,
    pub artist: String,
    pub album: String,
    pub album_id: Option<String>,
    pub artist_id: Option<String>,
    /// Duration in seconds, when known.
    pub duration: Option<f64>,
}

#[derive(Clone, Debug, Deserialize, Serialize, ToSchema)]
pub struct HandoffQueue {
    /// In playback order, including repeated tracks.
    pub tracks: Vec<HandoffTrack>,
    /// Zero-based position in `tracks`.
    pub index: usize,
    pub position_ms: u64,
    pub playing: bool,
}

impl HandoffQueue {
    pub(super) fn valid(&self) -> bool {
        fn local_id(id: &str) -> bool {
            (1..=128).contains(&id.len())
                && id
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'-' | b'_'))
        }
        if self.tracks.len() > 2000
            || self.index >= self.tracks.len()
            || self.position_ms > 604_800_000
        {
            return false;
        }
        let mut bytes = 0;
        self.tracks.iter().all(|t| {
            let ids = [Some(&t.id), t.album_id.as_ref(), t.artist_id.as_ref()];
            let labels = [&t.title, &t.artist, &t.album];
            bytes += ids.iter().flatten().map(|id| id.len()).sum::<usize>()
                + labels.iter().map(|s| s.len()).sum::<usize>();
            bytes <= 512 * 1024
                && ids.into_iter().flatten().all(|id| local_id(id))
                && labels.into_iter().all(|s| s.len() <= 1024)
                && t.duration
                    .is_none_or(|s| s.is_finite() && (0.0..=604_800.0).contains(&s))
        })
    }
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct HandoffRequest {
    /// A fresh UUID for this transfer, reused when retrying this request.
    pub transfer_id: String,
    /// The initiating player, either the source or the destination.
    pub player_id: String,
    pub source_id: String,
    pub destination_id: String,
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum HandoffReceipt {
    Offered {
        transfer_id: String,
        queue: HandoffQueue,
    },
    Accepted {
        transfer_id: String,
    },
    Released {
        transfer_id: String,
    },
    Declined {
        transfer_id: String,
    },
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct HandoffHeartbeat {
    /// A UUID chosen by this running player.
    pub player_id: String,
    pub name: String,
    /// Whether the player has a local queue it can transfer.
    pub can_send: bool,
    #[serde(default)]
    pub disconnect: bool,
    pub receipt: Option<HandoffReceipt>,
}

#[derive(Clone, Debug, Serialize, ToSchema)]
pub struct HandoffPlayer {
    pub id: String,
    pub name: String,
    pub can_send: bool,
}

#[derive(Debug, Serialize, ToSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum HandoffCommand {
    /// Capture the queue and continue playing.
    Offer {
        transfer_id: String,
    },
    /// Restore the queue, then accept or decline.
    Receive {
        transfer_id: String,
        queue: HandoffQueue,
    },
    /// The destination accepted. Pause the source if the offered session is still current.
    Release {
        transfer_id: String,
        destination_id: String,
    },
    Failed {
        transfer_id: String,
    },
}

#[derive(Debug, Serialize, ToSchema)]
pub struct HandoffUpdate {
    pub players: Vec<HandoffPlayer>,
    pub command: Option<HandoffCommand>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn queue_bounds_include_duplicates_and_limit_retained_text() {
        let track = HandoffTrack {
            id: "local-1".into(),
            title: "Title".into(),
            artist: String::new(),
            album: String::new(),
            album_id: None,
            artist_id: None,
            duration: None,
        };
        let mut queue = HandoffQueue {
            tracks: vec![track; 2000],
            index: 1999,
            position_ms: 2500,
            playing: true,
        };
        assert!(queue.valid());
        queue.index = queue.tracks.len();
        assert!(!queue.valid());
        queue.index = 0;
        queue.tracks.push(queue.tracks[0].clone());
        assert!(!queue.valid());
        queue.tracks.pop();
        for track in &mut queue.tracks {
            track.title = "a".repeat(300);
        }
        assert!(!queue.valid());
    }
}
