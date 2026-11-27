#![deny(clippy::wildcard_enum_match_arm)]

use std::collections::VecDeque;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PlayerAction {
    Play,
    Pause,
    Seek { position_ms: i64 },
    SetRate { rate: f64 },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobKind {
    Command,
    Seek,
    Restore,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Job {
    pub action: PlayerAction,
    pub kind: JobKind,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum LaneState {
    Idle,
    Running { cmd: u64, job: Job },
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum LaneEvent {
    Push { job: Job, cmd: u64 },
    Finished { cmd: u64, next: u64 },
    Release,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum LaneEffect {
    Run { cmd: u64, action: PlayerAction },
    Finished { action: PlayerAction },
    Drained,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Lane {
    state: LaneState,
    queue: VecDeque<Job>,
}

impl Lane {
    pub fn new() -> Lane {
        Lane { state: LaneState::Idle, queue: VecDeque::new() }
    }

    pub fn step(&mut self, event: LaneEvent, effects: &mut Vec<LaneEffect>) {
        self.transition(event, effects);
    }

    pub fn busy(&self) -> bool {
        matches!(self.state, LaneState::Running { .. })
    }

    pub fn running_cmd(&self) -> Option<u64> {
        match self.state {
            LaneState::Running { cmd, .. } => Some(cmd),
            LaneState::Idle => None,
        }
    }

    pub fn seeking(&self) -> bool {
        let running = matches!(self.state, LaneState::Running { job: Job { kind: JobKind::Seek, .. }, .. });
        running || self.queue.iter().any(|job| job.kind == JobKind::Seek)
    }

    fn transition(&mut self, event: LaneEvent, effects: &mut Vec<LaneEffect>) {
        match (event, self.state) {
            (LaneEvent::Push { job, cmd }, LaneState::Idle) => {
                self.state = LaneState::Running { cmd, job };
                effects.push(LaneEffect::Run { cmd, action: job.action });
            }
            (LaneEvent::Push { job, .. }, LaneState::Running { .. }) => {
                self.queue.push_back(job);
            }
            (LaneEvent::Finished { cmd, next }, LaneState::Running { cmd: running, job }) if cmd == running => {
                effects.push(LaneEffect::Finished { action: job.action });
                match self.queue.pop_front() {
                    Some(queued) => {
                        self.state = LaneState::Running { cmd: next, job: queued };
                        effects.push(LaneEffect::Run { cmd: next, action: queued.action });
                    }
                    None => {
                        self.state = LaneState::Idle;
                        effects.push(LaneEffect::Drained);
                    }
                }
            }
            (LaneEvent::Finished { .. }, LaneState::Idle | LaneState::Running { .. }) => {}
            (LaneEvent::Release, LaneState::Idle | LaneState::Running { .. }) => {
                self.queue.retain(|job| job.kind == JobKind::Restore);
            }
        }
    }
}

impl Default for Lane {
    fn default() -> Lane {
        Lane::new()
    }
}
