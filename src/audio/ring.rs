//! A lock-free path for timed input blocks from the audio callback to the
//! engine thread.
//!
//! [`input_ring`] returns an [`InputWriter`], which turns into the
//! [`InputSink`] of an input stream, and an [`InputReader`] for the engine
//! thread. Two single-producer single-consumer rings carry the samples and one
//! [`BlockHeader`] per block. A block that does not fit is dropped whole, its
//! samples are counted, and the next accepted block is marked with
//! [`BlockHeader::after_drop`].

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread::Thread;

use rtrb::{Consumer, Producer, RingBuffer};

use super::backend::{InputBlockInfo, InputSink};

/// Sample capacity that holds 2 seconds of audio at 192 kHz.
pub const DEFAULT_SAMPLE_CAPACITY: usize = 2 * 192_000;

/// Header capacity; at 2048 blocks it holds more than 2 seconds of audio for
/// any block size of 128 samples or more at 96 kHz.
pub const DEFAULT_HEADER_CAPACITY: usize = 2048;

/// Position and timing of one accepted input block.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BlockHeader {
    /// Index of the block's first sample, counting accepted samples only.
    pub first_index: u64,
    /// Number of samples in the block.
    pub len: usize,
    /// Engine-clock time of the block's first sample in seconds.
    pub time_s: f64,
    /// Sample rate of the block in Hz.
    pub sample_rate: u32,
    /// Whether at least one block was dropped just before this one.
    pub after_drop: bool,
}

impl BlockHeader {
    /// Returns the engine-clock time of the sample at accepted index `index`,
    /// extrapolated from this block's start at its sample rate.
    pub fn time_of(&self, index: u64) -> f64 {
        self.time_s + (index as f64 - self.first_index as f64) / f64::from(self.sample_rate)
    }
}

/// Creates a ring for `sample_capacity` samples and `header_capacity` blocks.
///
/// Every push unparks `wake`, the thread that drains the [`InputReader`].
pub fn input_ring(
    sample_capacity: usize,
    header_capacity: usize,
    wake: Thread,
) -> (InputWriter, InputReader) {
    let (sample_tx, sample_rx) = RingBuffer::new(sample_capacity);
    let (header_tx, header_rx) = RingBuffer::new(header_capacity);
    let dropped = Arc::new(AtomicU64::new(0));
    let writer = InputWriter {
        samples: sample_tx,
        headers: header_tx,
        accepted: 0,
        pending_drop: false,
        dropped: Arc::clone(&dropped),
        wake,
    };
    let reader = InputReader {
        samples: sample_rx,
        headers: header_rx,
        dropped,
    };
    (writer, reader)
}

/// The audio-callback side of an input ring.
pub struct InputWriter {
    samples: Producer<f32>,
    headers: Producer<BlockHeader>,
    accepted: u64,
    pending_drop: bool,
    dropped: Arc<AtomicU64>,
    wake: Thread,
}

impl InputWriter {
    /// Pushes `block` with its timing, or drops it whole when either ring
    /// lacks room, then unparks the reader thread.
    ///
    /// An empty block is ignored. This neither blocks nor allocates.
    pub fn push(&mut self, block: &[f32], info: InputBlockInfo) {
        if block.is_empty() {
            return;
        }
        let written = self.headers.slots() > 0
            && match self.samples.write_chunk_uninit(block.len()) {
                Ok(chunk) => {
                    chunk.fill_from_iter(block.iter().copied());
                    true
                }
                Err(_) => false,
            };
        if written {
            let header = BlockHeader {
                first_index: self.accepted,
                len: block.len(),
                time_s: info.time_s,
                sample_rate: info.sample_rate,
                after_drop: self.pending_drop,
            };
            // The header ring has a free slot, checked above.
            let _ = self.headers.push(header);
            self.accepted += block.len() as u64;
            self.pending_drop = false;
        } else {
            self.dropped
                .fetch_add(block.len() as u64, Ordering::Relaxed);
            self.pending_drop = true;
        }
        self.wake.unpark();
    }

    /// Turns the writer into the sink of an input stream.
    pub fn into_sink(mut self) -> InputSink {
        Box::new(move |block, info| self.push(block, info))
    }
}

/// The engine-thread side of an input ring.
pub struct InputReader {
    samples: Consumer<f32>,
    headers: Consumer<BlockHeader>,
    dropped: Arc<AtomicU64>,
}

impl InputReader {
    /// Appends the samples of the oldest unread block to `out` and returns its
    /// header, or returns `None` when no block is waiting.
    pub fn read_block(&mut self, out: &mut Vec<f32>) -> Option<BlockHeader> {
        let header = self.headers.pop().ok()?;
        // The writer pushes a block's samples before its header, so all of
        // them are readable here.
        let len = header.len.min(self.samples.slots());
        if let Ok(chunk) = self.samples.read_chunk(len) {
            let (first, second) = chunk.as_slices();
            out.extend_from_slice(first);
            out.extend_from_slice(second);
            chunk.commit_all();
        }
        Some(header)
    }

    /// Returns the total number of samples dropped because the ring was full.
    pub fn dropped_samples(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }
}

#[cfg(test)]
mod tests {
    use std::thread;
    use std::time::{Duration, Instant};

    use super::*;
    use crate::music::assert_close;

    const RATE: u32 = 1000;

    fn info(time_s: f64) -> InputBlockInfo {
        InputBlockInfo {
            time_s,
            sample_rate: RATE,
        }
    }

    fn ring(samples: usize, headers: usize) -> (InputWriter, InputReader) {
        input_ring(samples, headers, thread::current())
    }

    #[test]
    fn blocks_arrive_in_order_with_their_headers() {
        let (mut writer, mut reader) = ring(100, 4);
        writer.push(&[1.0, 2.0], info(0.5));
        writer.push(&[3.0, 4.0, 5.0], info(0.502));
        writer.push(&[], info(9.0));

        let mut out = Vec::new();
        let first = reader.read_block(&mut out).expect("first block");
        let second = reader.read_block(&mut out).expect("second block");
        assert!(reader.read_block(&mut out).is_none());

        assert_eq!(out, [1.0, 2.0, 3.0, 4.0, 5.0]);
        assert_eq!(
            (first.first_index, first.len, first.after_drop),
            (0, 2, false)
        );
        assert_eq!((second.first_index, second.len), (2, 3));
        assert_close(second.time_of(4), 0.504, 1e-12);
        assert_eq!(reader.dropped_samples(), 0);
    }

    #[test]
    fn overflow_drops_whole_blocks_and_keeps_later_times_correct() {
        // 2 seconds of capacity; 3 seconds pushed without reading.
        let (mut writer, mut reader) = ring(2 * RATE as usize, 64);
        for block in 0..30 {
            writer.push(&[0.0; 100], info(f64::from(block) * 0.1));
        }
        assert_eq!(reader.dropped_samples(), 1000);

        let mut out = Vec::new();
        let mut headers = Vec::new();
        while let Some(header) = reader.read_block(&mut out) {
            headers.push(header);
        }
        assert_eq!(out.len(), 2000);
        assert_eq!(headers.len(), 20);
        assert!(headers.iter().all(|h| !h.after_drop));

        writer.push(&[0.5; 100], info(5.0));
        writer.push(&[0.5; 100], info(5.1));
        let resumed = reader.read_block(&mut out).expect("resumed block");
        let next = reader.read_block(&mut out).expect("next block");

        assert!(resumed.after_drop);
        assert!(!next.after_drop);
        assert_eq!(resumed.first_index, 2000);
        assert_eq!(next.first_index, 2100);
        assert_close(resumed.time_of(2050), 5.05, 1e-12);
        assert_close(next.time_of(2150), 5.15, 1e-12);
    }

    #[test]
    fn a_full_header_ring_drops_the_block() {
        let (mut writer, reader) = ring(1000, 2);
        for block in 0..3 {
            writer.push(&[0.0; 10], info(f64::from(block)));
        }
        assert_eq!(reader.dropped_samples(), 10);
    }

    #[test]
    fn pushing_unparks_the_reader_thread() {
        let (writer, _reader) = ring(100, 4);
        let mut sink = writer.into_sink();
        let pusher = thread::spawn(move || sink(&[1.0], info(0.0)));
        pusher.join().expect("pusher thread");

        let start = Instant::now();
        thread::park_timeout(Duration::from_secs(10));
        assert!(start.elapsed() < Duration::from_secs(5));
    }
}
