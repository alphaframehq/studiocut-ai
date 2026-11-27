use crate::render::Snapshot;
use serde::Serialize;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicUsize, Ordering};

pub const REPLAY_CAPACITY: usize = 128;
pub const MAX_CONCURRENT_READERS: usize = 8;

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProgressBatch {
    pub version: u32,
    pub message_type: &'static str,
    pub reset: bool,
    pub events: Vec<Snapshot>,
}

pub struct ProgressChannel {
    events: VecDeque<Snapshot>,
}
impl ProgressChannel {
    pub fn new(initial: Snapshot) -> Self {
        Self {
            events: VecDeque::from([initial]),
        }
    }
    pub fn publish(&mut self, snapshot: Snapshot) {
        if self.events.len() == REPLAY_CAPACITY {
            self.events.pop_front();
        }
        self.events.push_back(snapshot);
    }
    pub fn replay(&self, after: Option<u64>) -> Result<ProgressBatch, String> {
        let latest = self.events.back().ok_or("Progress channel is empty")?;
        if after.is_some_and(|sequence| sequence > latest.sequence) {
            return Err("Invalid future export sequence".into());
        }
        let reset = after.map_or(true, |sequence| {
            sequence.saturating_add(1) < self.events[0].sequence
        });
        let events = if reset {
            vec![latest.clone()]
        } else {
            self.events
                .iter()
                .filter(|event| event.sequence > after.unwrap_or(0))
                .cloned()
                .collect()
        };
        Ok(ProgressBatch {
            version: 1,
            message_type: "export.progress",
            reset,
            events,
        })
    }
}

#[derive(Default)]
pub struct ReaderLimit(AtomicUsize);
pub struct ReaderPermit<'a>(&'a AtomicUsize);
impl ReaderLimit {
    pub fn acquire(&self) -> Result<ReaderPermit<'_>, String> {
        self.0
            .try_update(Ordering::AcqRel, Ordering::Acquire, |n| {
                (n < MAX_CONCURRENT_READERS).then_some(n + 1)
            })
            .map_err(|_| "Too many concurrent export progress readers".to_string())?;
        Ok(ReaderPermit(&self.0))
    }
}
impl Drop for ReaderPermit<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::Release);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;
    fn event(sequence: u64) -> Snapshot {
        Snapshot {
            version: 1,
            id: Uuid::nil(),
            sequence,
            timestamp: sequence,
            state: "rendering".into(),
            progress: 0.5,
            encoder: None,
            disclosure: String::new(),
            output: String::new(),
            error: None,
        }
    }
    #[test]
    fn bounded_replay_reset_and_duplicate_cursor() {
        let mut channel = ProgressChannel::new(event(1));
        for sequence in 2..=200 {
            channel.publish(event(sequence));
        }
        assert_eq!(channel.events.len(), REPLAY_CAPACITY);
        let reset = channel.replay(Some(1)).unwrap();
        assert!(reset.reset);
        assert_eq!(reset.events.len(), 1);
        assert_eq!(reset.events[0].sequence, 200);
        let replay = channel.replay(Some(198)).unwrap();
        assert!(!replay.reset);
        assert_eq!(
            replay.events.iter().map(|e| e.sequence).collect::<Vec<_>>(),
            vec![199, 200]
        );
        assert!(channel.replay(Some(200)).unwrap().events.is_empty());
        assert!(channel.replay(Some(201)).is_err());
    }
    #[test]
    fn reader_limit_releases_on_disconnect() {
        let limit = ReaderLimit::default();
        let permits: Vec<_> = (0..MAX_CONCURRENT_READERS)
            .map(|_| limit.acquire().unwrap())
            .collect();
        assert!(limit.acquire().is_err());
        drop(permits);
        assert!(limit.acquire().is_ok());
    }
}
