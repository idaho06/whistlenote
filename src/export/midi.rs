//! Standard MIDI file export.

use crate::music::quantize::QuantNote;
use midly::num::{u15, u24, u28, u4, u7};
use midly::{
    Format, Header, MetaMessage, MidiMessage, Smf, Timing, Track, TrackEvent, TrackEventKind,
};

/// Ticks per quarter note in exported files.
const TICKS_PER_BEAT: u16 = 480;
/// Tempo in beats per minute used when `bpm` is not finite or not positive.
const DEFAULT_BPM: f64 = 120.0;
/// Velocity of the exported note events.
const VELOCITY: u8 = 100;
/// Channel of the exported note events.
const CHANNEL: u8 = 0;
/// Largest tempo in microseconds per quarter note that fits a `u24`.
const MAX_TEMPO_US: f64 = 16_777_215.0;

/// Rounds a beat count to MIDI ticks; negative and NaN values become 0.
fn beats_to_ticks(beats: f64) -> u32 {
    let ticks = beats * f64::from(TICKS_PER_BEAT);
    if ticks.is_nan() || ticks < 0.0 {
        0
    } else {
        ticks.round() as u32
    }
}

/// Builds a meta event at delta time 0.
fn meta(kind: MetaMessage<'static>) -> TrackEvent<'static> {
    TrackEvent {
        delta: u28::from(0),
        kind: TrackEventKind::Meta(kind),
    }
}

/// Builds a MIDI event on the export channel with the given delta time in ticks.
fn midi_event(delta: u32, message: MidiMessage) -> TrackEvent<'static> {
    TrackEvent {
        delta: u28::from(delta),
        kind: TrackEventKind::Midi {
            channel: u4::from(CHANNEL),
            message,
        },
    }
}

/// Serializes notes to a format 0 Standard MIDI File and returns its bytes.
///
/// A `bpm` that is not finite or not positive is treated as 120.
pub fn to_midi_bytes(notes: &[QuantNote], bpm: f64) -> Vec<u8> {
    let bpm = if bpm.is_finite() && bpm > 0.0 {
        bpm
    } else {
        DEFAULT_BPM
    };
    let tempo_us = (60_000_000.0 / bpm).round().clamp(1.0, MAX_TEMPO_US) as u32;

    let mut track: Track<'static> = Vec::with_capacity(notes.len() * 2 + 5);
    track.push(meta(MetaMessage::TrackName(b"whistlenote")));
    track.push(meta(MetaMessage::Tempo(u24::from(tempo_us))));
    track.push(meta(MetaMessage::TimeSignature(4, 2, 24, 8)));
    if !notes.is_empty() {
        track.push(midi_event(
            0,
            MidiMessage::ProgramChange {
                program: u7::from(0),
            },
        ));
    }

    let mut note_events: Vec<(u32, bool, u7)> = Vec::with_capacity(notes.len() * 2);
    for note in notes {
        let start = beats_to_ticks(note.start_beats);
        let length = beats_to_ticks(note.length_beats).max(1);
        let key = u7::from(note.midi.clamp(0, 127));
        note_events.push((start, true, key));
        note_events.push((start.saturating_add(length), false, key));
    }
    // A note off (false) sorts before a note on (true) at an equal tick.
    note_events.sort_by_key(|&(tick, is_on, _)| (tick, is_on));

    let vel = u7::from(VELOCITY);
    let mut prev_tick = 0;
    for (tick, is_on, key) in note_events {
        let message = if is_on {
            MidiMessage::NoteOn { key, vel }
        } else {
            MidiMessage::NoteOff { key, vel }
        };
        track.push(midi_event(tick - prev_tick, message));
        prev_tick = tick;
    }
    track.push(meta(MetaMessage::EndOfTrack));

    let smf = Smf {
        header: Header::new(
            Format::SingleTrack,
            Timing::Metrical(u15::from(TICKS_PER_BEAT)),
        ),
        tracks: vec![track],
    };
    let mut bytes = Vec::with_capacity(22 + smf.tracks[0].len() * 8);
    smf.write_std(&mut bytes)
        .expect("writing to an in-memory buffer is infallible");
    bytes
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds a note at `start_beats` lasting `length_beats` with the given MIDI number.
    fn note(start_beats: f64, length_beats: f64, midi: u8) -> QuantNote {
        QuantNote {
            start_beats,
            length_beats,
            midi,
        }
    }

    /// Parses `bytes` as a Standard MIDI File.
    fn parse(bytes: &[u8]) -> Smf<'_> {
        Smf::parse(bytes).expect("exported bytes parse as an SMF")
    }

    /// Collects `(absolute_tick, kind)` pairs from the single track of `smf`.
    fn track_events<'a>(smf: &Smf<'a>) -> Vec<(u32, TrackEventKind<'a>)> {
        let mut tick = 0;
        smf.tracks[0]
            .iter()
            .map(|event| {
                tick += event.delta.as_int();
                (tick, event.kind)
            })
            .collect()
    }

    /// Extracts the tempo in microseconds per quarter note from the first track.
    fn tempo_of(smf: &Smf) -> u32 {
        smf.tracks[0]
            .iter()
            .find_map(|event| match event.kind {
                TrackEventKind::Meta(MetaMessage::Tempo(tempo)) => Some(tempo.as_int()),
                _ => None,
            })
            .expect("the track contains a tempo event")
    }

    /// Wraps `message` in a channel 0 MIDI event kind.
    fn midi(message: MidiMessage) -> TrackEventKind<'static> {
        TrackEventKind::Midi {
            channel: u4::from(CHANNEL),
            message,
        }
    }

    #[test]
    fn header_is_single_track_with_480_ticks_per_beat() {
        let bytes = to_midi_bytes(&[note(0.0, 1.0, 60)], 120.0);
        let smf = parse(&bytes);
        assert_eq!(smf.header.format, Format::SingleTrack);
        assert_eq!(smf.header.timing, Timing::Metrical(u15::from(480)));
        assert_eq!(smf.tracks.len(), 1);
    }

    #[test]
    fn two_notes_produce_the_expected_events() {
        let bytes = to_midi_bytes(&[note(0.0, 1.0, 60), note(1.0, 0.5, 69)], 120.0);
        let smf = parse(&bytes);
        assert_eq!(
            track_events(&smf),
            vec![
                (
                    0,
                    TrackEventKind::Meta(MetaMessage::TrackName(b"whistlenote"))
                ),
                (
                    0,
                    TrackEventKind::Meta(MetaMessage::Tempo(u24::from(500_000)))
                ),
                (
                    0,
                    TrackEventKind::Meta(MetaMessage::TimeSignature(4, 2, 24, 8))
                ),
                (
                    0,
                    midi(MidiMessage::ProgramChange {
                        program: u7::from(0)
                    })
                ),
                (
                    0,
                    midi(MidiMessage::NoteOn {
                        key: u7::from(60),
                        vel: u7::from(100)
                    })
                ),
                (
                    480,
                    midi(MidiMessage::NoteOff {
                        key: u7::from(60),
                        vel: u7::from(100)
                    })
                ),
                (
                    480,
                    midi(MidiMessage::NoteOn {
                        key: u7::from(69),
                        vel: u7::from(100)
                    })
                ),
                (
                    720,
                    midi(MidiMessage::NoteOff {
                        key: u7::from(69),
                        vel: u7::from(100)
                    })
                ),
                (720, TrackEventKind::Meta(MetaMessage::EndOfTrack)),
            ]
        );
    }

    #[test]
    fn note_off_comes_before_note_on_at_an_equal_tick() {
        let bytes = to_midi_bytes(&[note(0.0, 1.0, 60), note(1.0, 1.0, 69)], 120.0);
        let smf = parse(&bytes);
        let at_480: Vec<TrackEventKind> = track_events(&smf)
            .into_iter()
            .filter(|(tick, _)| *tick == 480)
            .map(|(_, kind)| kind)
            .collect();
        assert_eq!(
            at_480,
            vec![
                midi(MidiMessage::NoteOff {
                    key: u7::from(60),
                    vel: u7::from(100)
                }),
                midi(MidiMessage::NoteOn {
                    key: u7::from(69),
                    vel: u7::from(100)
                }),
            ]
        );
    }

    #[test]
    fn zero_length_note_lasts_exactly_one_tick() {
        let bytes = to_midi_bytes(&[note(2.0, 0.0, 60)], 120.0);
        let smf = parse(&bytes);
        let off_ticks: Vec<u32> = track_events(&smf)
            .into_iter()
            .filter_map(|(tick, kind)| match kind {
                TrackEventKind::Midi {
                    message: MidiMessage::NoteOff { .. },
                    ..
                } => Some(tick),
                _ => None,
            })
            .collect();
        assert_eq!(off_ticks, vec![2 * 480 + 1]);
    }

    #[test]
    fn invalid_bpm_values_fall_back_to_120() {
        for bpm in [0.0, -120.0, f64::NAN, f64::INFINITY] {
            let bytes = to_midi_bytes(&[note(0.0, 1.0, 60)], bpm);
            assert_eq!(tempo_of(&parse(&bytes)), 500_000, "bpm {bpm}");
        }
    }

    #[test]
    fn bpm_60_gives_a_tempo_of_one_million_microseconds() {
        let bytes = to_midi_bytes(&[note(0.0, 1.0, 60)], 60.0);
        assert_eq!(tempo_of(&parse(&bytes)), 1_000_000);
    }

    #[test]
    fn empty_note_list_contains_only_meta_events() {
        let bytes = to_midi_bytes(&[], 120.0);
        let smf = parse(&bytes);
        let events = track_events(&smf);
        assert_eq!(events.len(), 4);
        assert!(events
            .iter()
            .all(|(_, kind)| matches!(kind, TrackEventKind::Meta(_))));
        assert_eq!(
            events.last().map(|(_, kind)| *kind),
            Some(TrackEventKind::Meta(MetaMessage::EndOfTrack))
        );
    }

    #[test]
    fn last_event_of_a_non_empty_file_is_end_of_track() {
        let bytes = to_midi_bytes(&[note(0.0, 1.0, 60)], 120.0);
        let smf = parse(&bytes);
        let events = track_events(&smf);
        assert_eq!(
            events.last().map(|(_, kind)| *kind),
            Some(TrackEventKind::Meta(MetaMessage::EndOfTrack))
        );
    }
}
