//! Bounded request observations for end-to-end consumers; no payloads or identities retained.
use radovid_proto::ClientMsg;
use std::sync::atomic::{AtomicU64, Ordering};

#[derive(Clone, Copy)]
pub enum Read {
    Leaderboard,
    Matches,
    Lobbies,
    Profile,
}
#[derive(Default)]
pub(crate) struct Requests([AtomicU64; 4]);
impl Requests {
    pub(crate) fn record(&self, message: &ClientMsg) {
        let kind = match message {
            ClientMsg::Leaderboard { .. } => Read::Leaderboard,
            ClientMsg::RecentMatches { .. } => Read::Matches,
            ClientMsg::LobbyList | ClientMsg::LobbySearch { .. } => Read::Lobbies,
            ClientMsg::Account { .. } => Read::Profile,
            _ => return,
        };
        self.0[kind as usize].fetch_add(1, Ordering::Relaxed);
    }
    pub(crate) fn count(&self, kind: Read) -> u64 {
        self.0[kind as usize].load(Ordering::Relaxed)
    }
}
