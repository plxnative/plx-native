use std::collections::BTreeMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Timer {
    LinkOpen { conn: u64 },
    LinkPing { conn: u64 },
    LinkSilence { conn: u64 },
    LinkAnswer { conn: u64 },
    Race,
    Fetch { get: u64 },
    Tick,
    Play,
    Hold,
    StatusSpacing,
    Heartbeat,
    Backoff,
    ResumeWindow,
    FirstSample { conn: u64 },
    ResolveTimeout { search: u64 },
    ProbeDeadline { probe: u64 },
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Deadlines {
    set_order: u64,
    queue: BTreeMap<(i64, u64), Timer>,
}

impl Deadlines {
    pub fn set(&mut self, now_ms: i64, delay_ms: i64, timer: Timer) {
        self.cancel(timer);
        self.set_order += 1;
        let due = now_ms.saturating_add(delay_ms.max(0));
        self.queue.insert((due, self.set_order), timer);
    }

    pub fn cancel(&mut self, timer: Timer) {
        self.queue.retain(|_, queued| *queued != timer);
    }

    pub fn cancel_where(&mut self, doomed: fn(&Timer, u64) -> bool, key: u64) {
        self.queue.retain(|_, queued| !doomed(queued, key));
    }

    pub fn is_set(&self, timer: Timer) -> bool {
        self.queue.values().any(|queued| *queued == timer)
    }

    pub fn take_due(&mut self, now_ms: i64) -> Option<(i64, Timer)> {
        let (&key, _) = self.queue.iter().next().filter(|((due, _), _)| *due <= now_ms)?;
        self.queue.remove(&key).map(|timer| (key.0, timer))
    }

    pub fn next_ms(&self) -> Option<i64> {
        self.queue.keys().next().map(|(due, _)| *due)
    }

    pub fn next_key(&self) -> Option<(i64, u64)> {
        self.queue.keys().next().copied()
    }

    pub fn set_count(&self) -> u64 {
        self.set_order
    }

}

pub fn of_conn(timer: &Timer, conn: u64) -> bool {
    match *timer {
        Timer::LinkOpen { conn: c } | Timer::LinkPing { conn: c } | Timer::LinkSilence { conn: c } | Timer::LinkAnswer { conn: c } | Timer::FirstSample { conn: c } => c == conn,
        Timer::Race
        | Timer::Fetch { .. }
        | Timer::Tick
        | Timer::Play
        | Timer::Hold
        | Timer::StatusSpacing
        | Timer::Heartbeat
        | Timer::Backoff
        | Timer::ResumeWindow
        | Timer::ResolveTimeout { .. }
        | Timer::ProbeDeadline { .. } => false,
    }
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Ids {
    last: u64,
}

impl Ids {
    pub fn mint(&mut self) -> u64 {
        self.last += 1;
        self.last
    }
}
