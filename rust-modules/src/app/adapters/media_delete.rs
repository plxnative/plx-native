//! Resource runner for an explicitly confirmed deletion. One operation at a time, no retries.
use plx_machine::machine::MachineId;
use plx_plex::plex::{self, Client, ClientKey, DeleteResult};
use plx_screens::registry::MediaDeleteRequest;
use std::sync::mpsc::{self, Receiver, TryRecvError};

fn admitted_client(req: &MediaDeleteRequest) -> Result<&'static Client, DeleteResult> {
    let profile = plex::session::current_snapshot();
    let session = plex::session::peek_settled().ok_or(DeleteResult::IdentityChanged)?;
    if profile.generation != req.profile {
        return Err(DeleteResult::IdentityChanged);
    }
    if !session.active_profile_is_admin() || session.account_token.is_empty()
        || !plex::server_facts(req.sid).is_some_and(|facts| facts.owned) {
        return Err(DeleteResult::Forbidden);
    }
    plex::client_for(req.sid).filter(|client| ClientKey::of(client) == req.key)
        .ok_or(DeleteResult::IdentityChanged)
}

#[derive(Default)]
pub(crate) struct MediaDeleteRunner {
    pending: Option<(MachineId, MediaDeleteRequest, Receiver<DeleteResult>)>,
}
impl MediaDeleteRunner {
    pub fn start(&mut self, from: MachineId, req: MediaDeleteRequest) -> Option<DeleteResult> {
        if self.pending.is_some() { return Some(DeleteResult::Unavailable); }
        let client = match admitted_client(&req) {
            Ok(client) => client,
            Err(reason) => return Some(reason),
        };
        let (send, recv) = mpsc::channel();
        let target = req.clone();
        if !plx_base::task::spawn_small("media-delete", move || {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                client.delete_metadata(&target.rk, target.key, || admitted_client(&target).is_ok())
            })).unwrap_or(DeleteResult::Unavailable);
            let _ = send.send(result);
            plx_machine::present::wake_from_worker();
        }) { return Some(DeleteResult::Unavailable); }
        self.pending = Some((from, req, recv));
        None
    }
    pub fn poll(&mut self) -> Option<(MachineId, MediaDeleteRequest, DeleteResult)> {
        let (_, _, recv) = self.pending.as_ref()?;
        let result = match recv.try_recv() {
            Ok(result) => result,
            Err(TryRecvError::Empty) => return None,
            Err(TryRecvError::Disconnected) => DeleteResult::Unavailable,
        };
        let (to, req, _) = self.pending.take()?;
        let result = if admitted_client(&req).is_ok() { result } else { DeleteResult::IdentityChanged };
        Some((to, req, result))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_delete_reply_from_a_previous_profile_cannot_refresh_the_current_view() {
        let _serial = plx_base::testlock::serial();
        let request = MediaDeleteRequest {
            sid: plex::ServerId::UNSET, rk: "42".into(), kind: 0, show_rk: String::new(),
            profile: plex::session::current_snapshot().generation.wrapping_sub(1),
            key: ClientKey { instance: 1, token: 2, grant: 3 },
        };
        let (send, recv) = mpsc::channel();
        send.send(DeleteResult::Deleted).unwrap();
        let mut runner = MediaDeleteRunner { pending: Some((MachineId::Nav, request, recv)) };
        assert_eq!(runner.poll().map(|(_, _, result)| result), Some(DeleteResult::IdentityChanged));
        assert!(runner.poll().is_none());
    }
}
