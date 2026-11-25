//! Live queue transfers. Only an explicitly available player under the caller's own session
//! can offer, consume or acknowledge a queue. All state expires in memory.
pub mod models;

use std::{
    collections::HashMap,
    sync::{Arc, Mutex, PoisonError},
};

use droppedneedle_auth::Principal;
use uuid::Uuid;

use self::models::{
    HandoffCommand, HandoffHeartbeat, HandoffPlayer, HandoffQueue, HandoffReceipt, HandoffRequest,
    HandoffUpdate,
};
use super::error::PlaybackError;

const LIVE_MS: i64 = 45_000;
const STEP_MS: i64 = 30_000;

struct Player {
    visible: HandoffPlayer,
    session: String,
    seen: i64,
}

enum Step {
    Offer,
    Receive(HandoffQueue),
    Release,
    Done,
    Failed,
}

struct Transfer {
    id: String,
    source: String,
    destination: String,
    expires: i64,
    step: Step,
}

impl Transfer {
    fn active(&self) -> bool {
        !matches!(self.step, Step::Done | Step::Failed)
    }

    fn advance(&mut self, step: Step, now: i64) {
        self.step = step;
        self.expires = now + STEP_MS;
    }

    fn receipt(&mut self, player: &str, receipt: HandoffReceipt, now: i64) {
        let next = match receipt {
            HandoffReceipt::Offered { transfer_id, queue }
                if transfer_id == self.id
                    && player == self.source
                    && matches!(self.step, Step::Offer) =>
            {
                if queue.valid() {
                    Step::Receive(queue)
                } else {
                    Step::Failed
                }
            }
            HandoffReceipt::Accepted { transfer_id }
                if transfer_id == self.id
                    && player == self.destination
                    && matches!(self.step, Step::Receive(_)) =>
            {
                Step::Release
            }
            HandoffReceipt::Released { transfer_id }
                if transfer_id == self.id
                    && player == self.source
                    && matches!(self.step, Step::Release) =>
            {
                Step::Done
            }
            HandoffReceipt::Declined { transfer_id }
                if transfer_id == self.id
                    && (player == self.source || player == self.destination)
                    && self.active() =>
            {
                Step::Failed
            }
            _ => return, // Repeated, stale or out-of-order receipts cannot advance the transfer.
        };
        self.advance(next, now);
    }

    fn command(&self, player: &str) -> Option<HandoffCommand> {
        let transfer_id = self.id.clone();
        match &self.step {
            Step::Offer if player == self.source => Some(HandoffCommand::Offer { transfer_id }),
            Step::Receive(queue) if player == self.destination => Some(HandoffCommand::Receive {
                transfer_id,
                queue: queue.clone(),
            }),
            Step::Release if player == self.source => Some(HandoffCommand::Release {
                transfer_id,
                destination_id: self.destination.clone(),
            }),
            Step::Failed if player == self.source || player == self.destination => {
                Some(HandoffCommand::Failed { transfer_id })
            }
            _ => None,
        }
    }
}

#[derive(Default)]
struct Account {
    players: HashMap<String, Player>,
    transfer: Option<Transfer>,
}

impl Account {
    fn expire(&mut self, now: i64) {
        self.players.retain(|_, p| now - p.seen < LIVE_MS);
        if let Some(t) = &mut self.transfer {
            if t.active() {
                if now >= t.expires
                    || !self.players.contains_key(&t.source)
                    || !self.players.contains_key(&t.destination)
                {
                    t.advance(Step::Failed, now);
                }
            } else if now >= t.expires {
                self.transfer = None;
            }
        }
    }
}

#[derive(Clone, Default)]
pub struct HandoffRegistry(Arc<Mutex<HashMap<String, Account>>>);

impl HandoffRegistry {
    fn accounts(&self, now: i64) -> std::sync::MutexGuard<'_, HashMap<String, Account>> {
        let mut accounts = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        accounts.retain(|_, account| {
            account.expire(now);
            !account.players.is_empty()
        });
        accounts
    }

    pub(super) fn sync(
        &self,
        caller: &Principal,
        hb: HandoffHeartbeat,
        now: i64,
    ) -> Result<HandoffUpdate, PlaybackError> {
        let id = hb.player_id;
        if Uuid::parse_str(&id).is_err()
            || hb.name.trim().is_empty()
            || hb.name.chars().count() > 80
            || hb.name.chars().any(char::is_control)
        {
            return Err(PlaybackError::InvalidInput {
                message: "Give the player a UUID and a name of up to 80 characters.".into(),
            });
        }
        let mut accounts = self.accounts(now);
        let user = caller.user_id();
        if !accounts.contains_key(user) && accounts.len() >= 128 {
            return Err(PlaybackError::Conflict("Too many active handoff players."));
        }
        let account = accounts.entry(user.to_owned()).or_default();
        if account
            .players
            .get(&id)
            .is_some_and(|p| p.session != caller.session_id())
        {
            return Err(PlaybackError::NotFound);
        }
        if hb.disconnect {
            account.players.remove(&id);
            account.expire(now);
            return Ok(HandoffUpdate {
                players: Vec::new(),
                command: None,
            });
        }
        if !account.players.contains_key(&id) && account.players.len() >= 16 {
            return Err(PlaybackError::Conflict(
                "This account already has 16 active handoff players.",
            ));
        }
        account.players.insert(
            id.clone(),
            Player {
                visible: HandoffPlayer {
                    id: id.clone(),
                    name: hb.name.trim().to_owned(),
                    can_send: hb.can_send,
                },
                session: caller.session_id().to_owned(),
                seen: now,
            },
        );
        if let (Some(t), Some(receipt)) = (&mut account.transfer, hb.receipt) {
            t.receipt(&id, receipt, now);
        }
        let mut players: Vec<_> = account
            .players
            .values()
            .map(|p| p.visible.clone())
            .collect();
        players.sort_by(|a, b| a.name.cmp(&b.name).then(a.id.cmp(&b.id)));
        Ok(HandoffUpdate {
            players,
            command: account.transfer.as_ref().and_then(|t| t.command(&id)),
        })
    }

    pub(super) fn start(
        &self,
        caller: &Principal,
        req: HandoffRequest,
        now: i64,
    ) -> Result<(), PlaybackError> {
        if Uuid::parse_str(&req.transfer_id).is_err()
            || req.source_id == req.destination_id
            || (req.player_id != req.source_id && req.player_id != req.destination_id)
        {
            return Err(PlaybackError::InvalidInput {
                message:
                    "Choose two different players, including this one, and a fresh transfer UUID."
                        .into(),
            });
        }
        let mut accounts = self.accounts(now);
        let account = accounts
            .get_mut(caller.user_id())
            .ok_or(PlaybackError::NotFound)?;
        if account
            .players
            .get(&req.player_id)
            .is_none_or(|p| p.session != caller.session_id())
            || !account.players.contains_key(&req.destination_id)
        {
            return Err(PlaybackError::NotFound);
        }
        let source = account
            .players
            .get(&req.source_id)
            .ok_or(PlaybackError::NotFound)?;
        if let Some(t) = &account.transfer {
            if t.id == req.transfer_id
                && t.source == req.source_id
                && t.destination == req.destination_id
            {
                return Ok(());
            }
            if t.active() || t.id == req.transfer_id {
                return Err(PlaybackError::Conflict(
                    "This account already has a handoff in progress or used this transfer ID.",
                ));
            }
        }
        if !source.visible.can_send {
            return Err(PlaybackError::Conflict(
                "The source player has no transferable queue.",
            ));
        }
        account.transfer = Some(Transfer {
            id: req.transfer_id,
            source: req.source_id,
            destination: req.destination_id,
            expires: now + STEP_MS,
            step: Step::Offer,
        });
        Ok(())
    }
}
