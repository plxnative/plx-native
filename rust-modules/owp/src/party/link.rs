#![deny(clippy::wildcard_enum_match_arm)]

use crate::clock::{ClockEvent, ClockSync, Sample};

pub const CONNECT_TIMEOUT_MS: i64 = 10_000;
pub const SILENCE_MS: i64 = 15_000;
pub const BURST: i64 = 3;
pub const BURST_SPACING_MS: i64 = 500;
pub const PING_INTERVAL_MS: i64 = 5000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hello {
    Create,
    Join,
    Resume,
}

impl Hello {
    pub fn re(self) -> &'static str {
        match self {
            Hello::Create => "create",
            Hello::Join => "join",
            Hello::Resume => "resume",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkState {
    Opening,
    Open { opened_at: i64 },
    Asked { opened_at: i64, hello: Hello },
    Member { opened_at: i64 },
    Lingering { opened_at: i64, hello: Hello },
    Closed { opened: bool },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinkEvent {
    Opened { at_ms: i64 },
    PingDue { at_ms: i64 },
    Heard { at_ms: i64 },
    Pong { id: i64, server_time: i64, at_ms: i64 },
    Asked { hello: Hello },
    Joined,
    Linger,
    Close,
    Lost,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkEffect {
    Ping { id: i64 },
    PingAt { at_ms: i64 },
    SilenceAt { at_ms: i64 },
    Ponged,
    Closed { opened: bool },
}

#[derive(Debug)]
pub struct Link {
    url: String,
    state: LinkState,
    clock: ClockSync,
    pings: i64,
}

fn ping_due_ms(sent: i64) -> i64 {
    if sent < BURST {
        sent * BURST_SPACING_MS
    } else {
        (sent - BURST + 1) * PING_INTERVAL_MS
    }
}

impl Link {
    pub fn new(url: &str) -> Link {
        Link { url: url.to_owned(), state: LinkState::Opening, clock: ClockSync::new(), pings: 0 }
    }

    pub fn step(&mut self, event: LinkEvent, effects: &mut Vec<LinkEffect>) {
        self.transition(event, effects);
    }

    pub fn url(&self) -> &str {
        &self.url
    }

    pub fn state(&self) -> LinkState {
        self.state
    }

    pub fn best(&self) -> Option<Sample> {
        self.clock.best()
    }

    pub fn is_open(&self) -> bool {
        match self.state {
            LinkState::Open { .. } | LinkState::Asked { .. } | LinkState::Member { .. } | LinkState::Lingering { .. } => true,
            LinkState::Opening | LinkState::Closed { .. } => false,
        }
    }

    pub fn lingers(&self) -> bool {
        matches!(self.state, LinkState::Lingering { .. })
    }

    fn opened_at(&self) -> Option<i64> {
        match self.state {
            LinkState::Open { opened_at } | LinkState::Asked { opened_at, .. } | LinkState::Member { opened_at } | LinkState::Lingering { opened_at, .. } => Some(opened_at),
            LinkState::Opening | LinkState::Closed { .. } => None,
        }
    }

    fn transition(&mut self, event: LinkEvent, effects: &mut Vec<LinkEffect>) {
        match (event, self.state) {
            (LinkEvent::Opened { at_ms }, LinkState::Opening) => {
                self.state = LinkState::Open { opened_at: at_ms };
                effects.push(LinkEffect::SilenceAt { at_ms: at_ms + SILENCE_MS });
                self.pings += 1;
                let mut ignored = Vec::new();
                self.clock.step(ClockEvent::PingSent { id: self.pings, at_ms }, &mut ignored);
                effects.push(LinkEffect::Ping { id: self.pings });
                effects.push(LinkEffect::PingAt { at_ms: at_ms + ping_due_ms(self.pings) });
            }
            (LinkEvent::PingDue { at_ms }, LinkState::Open { opened_at } | LinkState::Asked { opened_at, .. } | LinkState::Member { opened_at } | LinkState::Lingering { opened_at, .. }) => {
                self.pings += 1;
                let mut ignored = Vec::new();
                self.clock.step(ClockEvent::PingSent { id: self.pings, at_ms }, &mut ignored);
                effects.push(LinkEffect::Ping { id: self.pings });
                effects.push(LinkEffect::PingAt { at_ms: opened_at + ping_due_ms(self.pings) });
            }
            (LinkEvent::Heard { at_ms }, LinkState::Open { .. } | LinkState::Asked { .. } | LinkState::Member { .. } | LinkState::Lingering { .. }) => {
                effects.push(LinkEffect::SilenceAt { at_ms: at_ms + SILENCE_MS });
            }
            (LinkEvent::Pong { id, server_time, at_ms }, LinkState::Open { .. } | LinkState::Asked { .. } | LinkState::Member { .. } | LinkState::Lingering { .. }) => {
                let mut ignored = Vec::new();
                self.clock.step(ClockEvent::PongReceived { id, server_time, at_ms }, &mut ignored);
                effects.push(LinkEffect::Ponged);
            }
            (LinkEvent::Asked { hello }, LinkState::Open { opened_at }) => {
                self.state = LinkState::Asked { opened_at, hello };
            }
            (LinkEvent::Joined, LinkState::Asked { opened_at, .. }) => {
                self.state = LinkState::Member { opened_at };
            }
            (LinkEvent::Linger, LinkState::Asked { opened_at, hello }) => {
                self.state = LinkState::Lingering { opened_at, hello };
            }
            (LinkEvent::Close, LinkState::Opening | LinkState::Open { .. } | LinkState::Asked { .. } | LinkState::Member { .. } | LinkState::Lingering { .. }) => {
                let opened = self.opened_at().is_some();
                self.state = LinkState::Closed { opened };
                effects.push(LinkEffect::Closed { opened });
            }
            (LinkEvent::Lost, LinkState::Opening | LinkState::Open { .. } | LinkState::Asked { .. } | LinkState::Member { .. } | LinkState::Lingering { .. }) => {
                self.state = LinkState::Closed { opened: false };
            }
            (LinkEvent::Opened { .. }, LinkState::Open { .. } | LinkState::Asked { .. } | LinkState::Member { .. } | LinkState::Lingering { .. } | LinkState::Closed { .. })
            | (LinkEvent::PingDue { .. } | LinkEvent::Heard { .. } | LinkEvent::Pong { .. }, LinkState::Opening | LinkState::Closed { .. })
            | (LinkEvent::Asked { .. }, LinkState::Opening | LinkState::Asked { .. } | LinkState::Member { .. } | LinkState::Lingering { .. } | LinkState::Closed { .. })
            | (LinkEvent::Joined | LinkEvent::Linger, LinkState::Opening | LinkState::Open { .. } | LinkState::Member { .. } | LinkState::Lingering { .. } | LinkState::Closed { .. })
            | (LinkEvent::Close | LinkEvent::Lost, LinkState::Closed { .. }) => {}
        }
    }
}
