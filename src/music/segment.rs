//! Segmentation of a pitch frame sequence into notes.

/// One pitch analysis result at a point in time.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PitchFrame {
    /// Time of the centre of the analysis window, in seconds on the engine clock.
    pub time_s: f64,
    /// Detected frequency in Hz, or `None` when the frame is unvoiced.
    pub freq_hz: Option<f32>,
    /// Confidence of the detection in 0.0..=1.0.
    pub confidence: f32,
}

/// Parameters of [`segment`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SegmentConfig {
    /// Minimum confidence for a frame to count as voiced.
    pub min_confidence: f32,
    /// Minimum duration of a note in seconds; shorter notes are dropped.
    pub min_note_s: f64,
    /// Length of the median filter window, in voiced frames.
    pub median_window: usize,
    /// Longest unvoiced gap in seconds that still joins two equal notes.
    pub max_gap_s: f64,
}

impl Default for SegmentConfig {
    fn default() -> Self {
        Self {
            min_confidence: 0.8,
            min_note_s: 0.06,
            median_window: 5,
            max_gap_s: 0.04,
        }
    }
}

/// A note found in a pitch frame sequence, in seconds on the engine clock.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RawNote {
    /// Start time in seconds.
    pub start_s: f64,
    /// End time in seconds.
    pub end_s: f64,
    /// MIDI note number.
    pub midi: i32,
}

/// A frame that counts as voiced, with its MIDI note number.
struct VoicedFrame {
    /// Index of the frame in the input slice.
    index: usize,
    /// Time of the frame in seconds.
    time_s: f64,
    /// MIDI note number of the frame.
    midi: i32,
}

/// Returns the median spacing between consecutive frames, in seconds.
///
/// Returns 0.01 s when there are fewer than two frames. For an even number of
/// spacings the mean of the two middle values is returned.
fn frame_period(frames: &[PitchFrame]) -> f64 {
    if frames.len() < 2 {
        return 0.01;
    }
    let mut gaps: Vec<f64> = frames
        .windows(2)
        .map(|pair| pair[1].time_s - pair[0].time_s)
        .collect();
    gaps.sort_by(|a, b| a.total_cmp(b));
    let count = gaps.len();
    if count % 2 == 1 {
        gaps[count / 2]
    } else {
        (gaps[count / 2 - 1] + gaps[count / 2]) / 2.0
    }
}

/// Replaces each MIDI number by the median of its centred window in `values`.
///
/// The window spans `window / 2` values on each side of the value, clipped at
/// the edges of `values`. An even number of window values selects the lower
/// middle one. A window of 0 or 1 returns `values` unchanged. The medians are
/// computed from the original, unfiltered values.
fn median_filter(values: &[i32], window: usize) -> Vec<i32> {
    if window <= 1 || values.is_empty() {
        return values.to_vec();
    }
    let half = window / 2;
    let count = values.len();
    (0..count)
        .map(|i| {
            let lo = i.saturating_sub(half);
            let hi = (i + half).min(count - 1);
            let mut window_values: Vec<i32> = values[lo..=hi].to_vec();
            window_values.sort_unstable();
            let len = window_values.len();
            if len % 2 == 1 {
                window_values[len / 2]
            } else {
                window_values[len / 2 - 1]
            }
        })
        .collect()
}

/// Groups pitch frames into notes.
///
/// The frames must be ordered by `time_s`. The result is sorted by start time
/// and its notes do not overlap.
pub fn segment(frames: &[PitchFrame], cfg: &SegmentConfig) -> Vec<RawNote> {
    let period = frame_period(frames);
    let voiced: Vec<VoicedFrame> = frames
        .iter()
        .enumerate()
        .filter(|(_, frame)| frame.confidence >= cfg.min_confidence)
        .filter_map(|(index, frame)| {
            let hz = frame.freq_hz?;
            let reading = crate::music::note::from_freq(hz)?;
            Some(VoicedFrame {
                index,
                time_s: frame.time_s,
                midi: reading.midi,
            })
        })
        .collect();
    if voiced.is_empty() {
        return Vec::new();
    }
    let midis: Vec<i32> = voiced.iter().map(|frame| frame.midi).collect();
    let filtered = median_filter(&midis, cfg.median_window);

    let mut notes: Vec<RawNote> = Vec::new();
    let mut group_start = 0;
    for k in 1..voiced.len() {
        let prev = &voiced[k - 1];
        let curr = &voiced[k];
        let same_midi = filtered[k] == filtered[k - 1];
        let gap = curr.time_s - prev.time_s - period;
        let joins = same_midi && (curr.index == prev.index + 1 || gap <= cfg.max_gap_s + 1e-9);
        if !joins {
            notes.push(RawNote {
                start_s: voiced[group_start].time_s,
                end_s: prev.time_s + period,
                midi: filtered[group_start],
            });
            group_start = k;
        }
    }
    let last = &voiced[voiced.len() - 1];
    notes.push(RawNote {
        start_s: voiced[group_start].time_s,
        end_s: last.time_s + period,
        midi: filtered[group_start],
    });

    notes.retain(|note| note.end_s - note.start_s >= cfg.min_note_s - 1e-9);
    notes.sort_by(|a, b| a.start_s.total_cmp(&b.start_s));
    for i in 1..notes.len() {
        if notes[i - 1].end_s > notes[i].start_s {
            notes[i - 1].end_s = notes[i].start_s;
        }
    }
    notes
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds one frame at time `i * 0.01` with the given frequency and confidence.
    fn frame_at(i: usize, freq_hz: Option<f32>, confidence: f32) -> PitchFrame {
        PitchFrame {
            time_s: i as f64 * 0.01,
            freq_hz,
            confidence,
        }
    }

    /// Builds `n` consecutive frames with the same frequency and confidence
    /// 0.95, starting at frame index `start`.
    fn run(start: usize, n: usize, freq_hz: Option<f32>) -> Vec<PitchFrame> {
        (start..start + n)
            .map(|i| frame_at(i, freq_hz, 0.95))
            .collect()
    }

    /// Asserts that `a` and `b` differ by at most `tol`.
    fn assert_close(a: f64, b: f64, tol: f64) {
        assert!(
            (a - b).abs() <= tol,
            "{a} and {b} differ by more than {tol}"
        );
    }

    #[test]
    fn hundred_a4_frames_form_one_note() {
        let frames = run(0, 100, Some(440.0));
        let notes = segment(&frames, &SegmentConfig::default());
        assert_eq!(notes.len(), 1);
        assert_eq!(notes[0].midi, 69);
        assert_close(notes[0].start_s, 0.0, 1e-9);
        assert_close(notes[0].end_s, 1.0, 1e-9);
    }

    #[test]
    fn two_pitches_separated_by_silence_form_two_sorted_notes() {
        let mut frames = run(0, 50, Some(440.0));
        frames.extend(run(50, 30, None));
        frames.extend(run(80, 50, Some(523.25)));
        let notes = segment(&frames, &SegmentConfig::default());
        assert_eq!(notes.len(), 2);
        assert_eq!(notes[0].midi, 69);
        assert_eq!(notes[1].midi, 72);
        assert_close(notes[0].start_s, 0.0, 1e-9);
        assert_close(notes[0].end_s, 0.5, 1e-9);
        assert_close(notes[1].start_s, 0.8, 1e-9);
        assert_close(notes[1].end_s, 1.3, 1e-9);
        assert!(notes[1].start_s >= notes[0].end_s);
    }

    #[test]
    fn single_unvoiced_frame_gap_merges() {
        let mut frames = run(0, 40, Some(440.0));
        frames.push(frame_at(40, None, 0.0));
        frames.extend(run(41, 40, Some(440.0)));
        let notes = segment(&frames, &SegmentConfig::default());
        assert_eq!(notes.len(), 1);
        assert_eq!(notes[0].midi, 69);
        assert_close(notes[0].start_s, 0.0, 1e-9);
        assert_close(notes[0].end_s, 0.81, 1e-9);
    }

    #[test]
    fn ten_unvoiced_frames_split_the_note() {
        let mut frames = run(0, 30, Some(440.0));
        frames.extend(run(30, 10, None));
        frames.extend(run(40, 30, Some(440.0)));
        let notes = segment(&frames, &SegmentConfig::default());
        assert_eq!(notes.len(), 2);
        assert!(notes.iter().all(|note| note.midi == 69));
        assert_close(notes[0].end_s, 0.3, 1e-9);
        assert_close(notes[1].start_s, 0.4, 1e-9);
    }

    #[test]
    fn median_filter_removes_isolated_pitch_blip() {
        let mut frames = run(0, 60, Some(440.0));
        frames[30] = frame_at(30, Some(466.16), 0.95);
        let notes = segment(&frames, &SegmentConfig::default());
        assert_eq!(notes.len(), 1);
        assert_eq!(notes[0].midi, 69);
        assert_close(notes[0].end_s, 0.6, 1e-9);
    }

    #[test]
    fn short_blip_in_silence_is_dropped() {
        let mut frames = run(0, 10, None);
        frames.extend(run(10, 3, Some(440.0)));
        frames.extend(run(13, 10, None));
        let notes = segment(&frames, &SegmentConfig::default());
        assert!(notes.is_empty());
    }

    #[test]
    fn low_confidence_frames_are_ignored() {
        let frames: Vec<PitchFrame> = (0..50).map(|i| frame_at(i, Some(440.0), 0.5)).collect();
        let notes = segment(&frames, &SegmentConfig::default());
        assert!(notes.is_empty());
    }

    #[test]
    fn empty_and_single_frame_inputs_give_no_notes() {
        let cfg = SegmentConfig::default();
        assert!(segment(&[], &cfg).is_empty());
        assert!(segment(&[frame_at(0, Some(440.0), 0.95)], &cfg).is_empty());
    }
}
