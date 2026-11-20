//! Segmentation of a pitch frame sequence into notes.

use super::median;

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
    /// Duration in seconds below which a note that touches a neighbouring note merges into it.
    pub merge_note_s: f64,
}

impl Default for SegmentConfig {
    fn default() -> Self {
        Self {
            min_confidence: 0.8,
            min_note_s: 0.06,
            median_window: 5,
            max_gap_s: 0.04,
            merge_note_s: 0.1,
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

/// Tolerance in seconds for comparing sums and differences of frame times.
const TIME_EPS_S: f64 = 1e-9;

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
    let gaps: Vec<f64> = frames
        .windows(2)
        .map(|pair| pair[1].time_s - pair[0].time_s)
        .collect();
    median(gaps).unwrap_or(0.01)
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
    let mut buf = vec![0; (2 * half + 1).min(count)];
    (0..count)
        .map(|i| {
            let lo = i.saturating_sub(half);
            let hi = (i + half).min(count - 1);
            let len = hi - lo + 1;
            buf[..len].copy_from_slice(&values[lo..=hi]);
            buf[..len].sort_unstable();
            if len % 2 == 1 {
                buf[len / 2]
            } else {
                buf[len / 2 - 1]
            }
        })
        .collect()
}

/// Returns the MIDI note number of `frame` when it counts as voiced under `cfg`.
fn voiced_midi(frame: &PitchFrame, cfg: &SegmentConfig) -> Option<i32> {
    if frame.confidence < cfg.min_confidence {
        return None;
    }
    let hz = frame.freq_hz?;
    crate::music::note::from_freq(hz).map(|reading| reading.midi)
}

/// Groups pitch frames into notes.
///
/// The frames must be ordered by `time_s`. A *hole* lies between two
/// consecutive input frames when their times differ by more than 1.5 times
/// the median frame period; input frames that are missing, such as the
/// frames an audible metronome click covered, leave a hole. Equal notes join
/// across a hole, because consecutive input frames keep adjacent indices
/// across it. A note whose first voiced frame directly follows a hole that
/// comes after an unvoiced input frame starts one period after that unvoiced
/// frame, so a note that begins on a click starts at the click; a note that
/// ends before a hole ends one period after its last voiced frame. Notes
/// shorter than `merge_note_s` that touch a neighbouring note merge into the
/// longer neighbour, so a short pitch slide at the edge of a note extends
/// that note instead of forming a note of its own. The result is sorted by
/// start time and its notes do not overlap.
pub fn segment(frames: &[PitchFrame], cfg: &SegmentConfig) -> Vec<RawNote> {
    let period = frame_period(frames);
    // A hole lies between the input frames j - 1 and j when their times
    // differ by more than one and a half periods.
    let hole_before = |j: usize| j > 0 && frames[j].time_s - frames[j - 1].time_s > 1.5 * period;
    let voiced: Vec<VoicedFrame> = frames
        .iter()
        .enumerate()
        .filter_map(|(index, frame)| {
            let midi = voiced_midi(frame, cfg)?;
            Some(VoicedFrame {
                index,
                time_s: frame.time_s,
                midi,
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
    // The extra iteration at k == voiced.len() closes the final group.
    for k in 1..=voiced.len() {
        let joins = k < voiced.len() && {
            let prev = &voiced[k - 1];
            let curr = &voiced[k];
            let gap = curr.time_s - prev.time_s - period;
            filtered[k] == filtered[k - 1]
                && (curr.index == prev.index + 1 || gap <= cfg.max_gap_s + TIME_EPS_S)
        };
        if !joins {
            let first = &voiced[group_start];
            // A hole directly before the first voiced frame of a note that
            // begins in an unvoiced stretch pulls the start back to one
            // period after the input frame before the hole.
            let start_s = if hole_before(first.index)
                && voiced_midi(&frames[first.index - 1], cfg).is_none()
            {
                frames[first.index - 1].time_s + period
            } else {
                first.time_s
            };
            notes.push(RawNote {
                start_s,
                end_s: voiced[k - 1].time_s + period,
                midi: filtered[group_start],
            });
            group_start = k;
        }
    }

    merge_short_notes(&mut notes, cfg);
    notes.retain(|note| note.end_s - note.start_s >= cfg.min_note_s - TIME_EPS_S);
    notes.sort_by(|a, b| a.start_s.total_cmp(&b.start_s));
    for i in 1..notes.len() {
        if notes[i - 1].end_s > notes[i].start_s {
            notes[i - 1].end_s = notes[i].start_s;
        }
    }
    notes
}

/// Merges each note shorter than `cfg.merge_note_s` that touches a neighbouring note into that neighbour.
///
/// `notes` must be ordered by start time. A note touches its previous note
/// when their start and end differ by at most `cfg.max_gap_s`, and likewise
/// for its next note. The shortest touching short note is merged first, ties
/// going to the earliest note, into the longer of its touching neighbours,
/// ties going to the previous one; the neighbour keeps its own MIDI number.
/// Isolated short notes are left in place.
fn merge_short_notes(notes: &mut Vec<RawNote>, cfg: &SegmentConfig) {
    loop {
        let mut shortest: Option<usize> = None;
        for i in 0..notes.len() {
            if notes[i].end_s - notes[i].start_s >= cfg.merge_note_s - TIME_EPS_S {
                continue;
            }
            if !touches_prev(notes, i, cfg) && !touches_next(notes, i, cfg) {
                continue;
            }
            let shorter = shortest.is_none_or(|j| {
                notes[i].end_s - notes[i].start_s < notes[j].end_s - notes[j].start_s
            });
            if shorter {
                shortest = Some(i);
            }
        }
        let Some(i) = shortest else { break };
        let prev = touches_prev(notes, i, cfg);
        let next = touches_next(notes, i, cfg);
        if prev
            && (!next
                || notes[i - 1].end_s - notes[i - 1].start_s
                    >= notes[i + 1].end_s - notes[i + 1].start_s)
        {
            notes[i - 1].end_s = notes[i].end_s;
        } else {
            notes[i + 1].start_s = notes[i].start_s;
        }
        notes.remove(i);
    }
}

/// Returns whether the note at index `i` touches the note before it.
fn touches_prev(notes: &[RawNote], i: usize, cfg: &SegmentConfig) -> bool {
    i > 0 && notes[i].start_s - notes[i - 1].end_s <= cfg.max_gap_s + TIME_EPS_S
}

/// Returns whether the note at index `i` touches the note after it.
fn touches_next(notes: &[RawNote], i: usize, cfg: &SegmentConfig) -> bool {
    i + 1 < notes.len() && notes[i + 1].start_s - notes[i].end_s <= cfg.max_gap_s + TIME_EPS_S
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::music::assert_close;

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
    fn a_held_tone_across_a_hole_is_one_note() {
        let mut frames = run(0, 100, Some(440.0));
        frames.drain(40..46); // frames 40..46 are missing: a hole of 0.07 s
        let notes = segment(&frames, &SegmentConfig::default());
        assert_eq!(notes.len(), 1);
        assert_eq!(notes[0].midi, 69);
        assert_close(notes[0].start_s, 0.0, 1e-9);
        assert_close(notes[0].end_s, 1.0, 1e-9);
    }

    #[test]
    fn a_note_after_a_hole_starts_one_period_after_the_frame_before_the_hole() {
        let mut frames: Vec<PitchFrame> = (0..30).map(|i| frame_at(i, None, 0.0)).collect();
        frames.extend(run(36, 50, Some(440.0))); // frames 30..36 are missing: a hole
        let notes = segment(&frames, &SegmentConfig::default());
        assert_eq!(notes.len(), 1);
        assert_eq!(notes[0].midi, 69);
        assert_close(notes[0].start_s, 0.30, 1e-9);
        assert_close(notes[0].end_s, 0.86, 1e-9);
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

    #[test]
    fn onset_slide_merges_into_following_note() {
        let mut frames = run(0, 8, Some(466.16));
        frames.extend(run(8, 50, Some(440.0)));
        let notes = segment(&frames, &SegmentConfig::default());
        assert_eq!(notes.len(), 1);
        assert_eq!(notes[0].midi, 69);
        assert_close(notes[0].start_s, 0.0, 1e-9);
        assert_close(notes[0].end_s, 0.58, 1e-9);
    }

    #[test]
    fn ending_slide_merges_into_previous_note() {
        let mut frames = run(0, 50, Some(440.0));
        frames.extend(run(50, 8, Some(466.16)));
        let notes = segment(&frames, &SegmentConfig::default());
        assert_eq!(notes.len(), 1);
        assert_eq!(notes[0].midi, 69);
        assert_close(notes[0].start_s, 0.0, 1e-9);
        assert_close(notes[0].end_s, 0.58, 1e-9);
    }

    #[test]
    fn short_note_between_two_notes_merges_into_the_longer_one() {
        let mut frames = run(0, 30, Some(440.0));
        frames.extend(run(30, 8, Some(466.16)));
        frames.extend(run(38, 50, Some(493.88)));
        let notes = segment(&frames, &SegmentConfig::default());
        assert_eq!(notes.len(), 2);
        assert_eq!(notes[0].midi, 69);
        assert_close(notes[0].start_s, 0.0, 1e-9);
        assert_close(notes[0].end_s, 0.30, 1e-9);
        assert_eq!(notes[1].midi, 71);
        assert_close(notes[1].start_s, 0.30, 1e-9);
        assert_close(notes[1].end_s, 0.88, 1e-9);
    }

    #[test]
    fn isolated_short_note_at_least_min_note_s_is_kept() {
        let mut frames = run(0, 8, Some(466.16));
        frames.extend(run(8, 20, None));
        frames.extend(run(28, 50, Some(440.0)));
        let notes = segment(&frames, &SegmentConfig::default());
        assert_eq!(notes.len(), 2);
        assert_eq!(notes[0].midi, 70);
        assert_close(notes[0].start_s, 0.0, 1e-9);
        assert_close(notes[0].end_s, 0.08, 1e-9);
        assert_eq!(notes[1].midi, 69);
        assert_close(notes[1].start_s, 0.28, 1e-9);
        assert_close(notes[1].end_s, 0.78, 1e-9);
    }

    #[test]
    fn note_of_at_least_merge_note_s_does_not_merge() {
        let mut frames = run(0, 15, Some(466.16));
        frames.extend(run(15, 50, Some(440.0)));
        let notes = segment(&frames, &SegmentConfig::default());
        assert_eq!(notes.len(), 2);
        assert_eq!(notes[0].midi, 70);
        assert_close(notes[0].start_s, 0.0, 1e-9);
        assert_close(notes[0].end_s, 0.15, 1e-9);
        assert_eq!(notes[1].midi, 69);
        assert_close(notes[1].start_s, 0.15, 1e-9);
        assert_close(notes[1].end_s, 0.65, 1e-9);
    }

    #[test]
    fn zero_merge_note_s_disables_merging() {
        let mut frames = run(0, 8, Some(466.16));
        frames.extend(run(8, 50, Some(440.0)));
        let cfg = SegmentConfig {
            merge_note_s: 0.0,
            ..SegmentConfig::default()
        };
        let notes = segment(&frames, &cfg);
        assert_eq!(notes.len(), 2);
        assert_eq!(notes[0].midi, 70);
        assert_close(notes[0].start_s, 0.0, 1e-9);
        assert_close(notes[0].end_s, 0.08, 1e-9);
        assert_eq!(notes[1].midi, 69);
        assert_close(notes[1].start_s, 0.08, 1e-9);
        assert_close(notes[1].end_s, 0.58, 1e-9);
    }

    #[test]
    fn default_merge_note_s_is_point_one() {
        assert_eq!(SegmentConfig::default().merge_note_s, 0.1);
    }
}
