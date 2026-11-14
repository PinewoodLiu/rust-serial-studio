use std::time::{Duration, Instant, SystemTime};

const MAX_BLOCK_BYTES: usize = 65_536;
const MAX_DISPLAY_DELAY: Duration = Duration::from_millis(200);

pub struct ReceivedBlock {
    pub bytes: Vec<u8>,
    pub at: SystemTime,
}

/// Coalesces serial/TCP reads for display. This is an idle-gap heuristic,
/// not a protocol frame parser. Raw capture and statistics bypass this buffer.
pub struct ReceiveBuffer {
    gap: Duration,
    bytes: Vec<u8>,
    first_at: SystemTime,
    first_tick: Option<Instant>,
    last_tick: Option<Instant>,
}

impl ReceiveBuffer {
    pub fn new(gap_ms: u64) -> Self {
        Self {
            gap: Duration::from_millis(gap_ms.clamp(1, 1000)),
            bytes: Vec::new(),
            first_at: SystemTime::UNIX_EPOCH,
            first_tick: None,
            last_tick: None,
        }
    }

    pub fn push(&mut self, mut data: &[u8], at: SystemTime, now: Instant) -> Vec<ReceivedBlock> {
        if data.is_empty() {
            return Vec::new();
        }
        let mut ready = Vec::new();
        if let Some(block) = self.flush_if_due(now) {
            ready.push(block);
        }
        while !data.is_empty() {
            if self.bytes.is_empty() {
                self.first_at = at;
                self.first_tick = Some(now);
            }
            let count = data.len().min(MAX_BLOCK_BYTES - self.bytes.len());
            self.bytes.extend_from_slice(&data[..count]);
            self.last_tick = Some(now);
            data = &data[count..];
            if self.bytes.len() == MAX_BLOCK_BYTES {
                ready.push(self.flush().unwrap());
            }
        }
        ready
    }

    pub fn flush_if_due(&mut self, now: Instant) -> Option<ReceivedBlock> {
        let idle = self
            .last_tick
            .is_some_and(|last| now.saturating_duration_since(last) >= self.gap);
        // Bound latency during continuous traffic as well as bounding memory.
        let age = self.first_tick.is_some_and(|first| {
            now.saturating_duration_since(first) >= MAX_DISPLAY_DELAY.max(self.gap)
        });
        if idle || age { self.flush() } else { None }
    }

    pub fn flush(&mut self) -> Option<ReceivedBlock> {
        if self.bytes.is_empty() {
            return None;
        }
        self.first_tick = None;
        self.last_tick = None;
        Some(ReceivedBlock {
            bytes: std::mem::take(&mut self.bytes),
            at: self.first_at,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn screenshot_fragmentation_becomes_one_record() {
        let mut receive = ReceiveBuffer::new(20);
        let start = Instant::now();
        let at = SystemTime::UNIX_EPOCH;
        assert!(receive.push(b"H", at, start).is_empty());
        assert!(
            receive
                .push(
                    b"ello, serial!",
                    at + Duration::from_millis(3),
                    start + Duration::from_millis(3)
                )
                .is_empty()
        );
        assert!(
            receive
                .flush_if_due(start + Duration::from_millis(22))
                .is_none()
        );
        let block = receive
            .flush_if_due(start + Duration::from_millis(23))
            .unwrap();
        assert_eq!(block.bytes, b"Hello, serial!");
        assert_eq!(block.bytes.len(), 14);
        assert_eq!(block.at, at);
        assert!(receive.flush().is_none());
    }

    #[test]
    fn silence_separates_messages_without_a_timer_tick_between_reads() {
        let mut receive = ReceiveBuffer::new(20);
        let start = Instant::now();
        let at = SystemTime::UNIX_EPOCH;
        receive.push(b"first", at, start);
        let blocks = receive.push(b"next", at, start + Duration::from_millis(21));
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].bytes, b"first");
        assert_eq!(receive.flush().unwrap().bytes, b"next");
    }

    #[test]
    fn size_and_age_limits_preserve_all_binary_bytes() {
        let mut receive = ReceiveBuffer::new(20);
        let start = Instant::now();
        let at = SystemTime::UNIX_EPOCH;
        let bytes: Vec<_> = (0..150_000).map(|n| (n % 256) as u8).collect();
        let mut blocks = receive.push(&bytes, at, start);
        assert_eq!(blocks.len(), 2);
        blocks.push(receive.flush().unwrap());
        assert!(
            blocks
                .iter()
                .all(|block| block.bytes.len() <= MAX_BLOCK_BYTES)
        );
        assert_eq!(
            blocks
                .into_iter()
                .flat_map(|block| block.bytes)
                .collect::<Vec<_>>(),
            bytes
        );
        for n in 0..20 {
            assert!(
                receive
                    .push(&[n], at, start + Duration::from_millis(n as u64 * 10))
                    .is_empty()
            );
        }
        let block = receive
            .flush_if_due(start + Duration::from_millis(200))
            .unwrap();
        assert_eq!(block.bytes, (0..20).collect::<Vec<u8>>());
    }

    #[test]
    fn flushing_on_close_preserves_an_incomplete_utf8_sequence() {
        let mut receive = ReceiveBuffer::new(20);
        receive.push(&[0xe4, 0xb8], SystemTime::UNIX_EPOCH, Instant::now());
        assert_eq!(receive.flush().unwrap().bytes, [0xe4, 0xb8]);
    }
}
