//! Conversion between frequencies, MIDI note numbers and note names.

/// A frequency expressed as the nearest MIDI note plus a deviation in cents.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct NoteReading {
    /// Nearest MIDI note number (60 is middle C, 69 is A4).
    pub midi: i32,
    /// Deviation from the nearest note in cents, within -50.0..=50.0.
    pub cents: f32,
}

/// MIDI note number of A4, the reference pitch.
const A4_MIDI: f32 = 69.0;
/// Default frequency in Hz of A4, the reference pitch.
pub const A4_HZ: f32 = 440.0;

/// Returns the frequency in Hz of a (possibly fractional) MIDI note number.
///
/// MIDI note 69 (A4) is 440.0 Hz.
pub fn midi_to_freq(midi: f32) -> f32 {
    midi_to_freq_at(midi, A4_HZ)
}

/// Returns the frequency in Hz of a (possibly fractional) MIDI note number
/// when A4 is `a4_hz` Hz.
///
/// MIDI note 69 (A4) is `a4_hz`.
pub fn midi_to_freq_at(midi: f32, a4_hz: f32) -> f32 {
    a4_hz * 2.0_f32.powf((midi - A4_MIDI) / 12.0)
}

/// Returns the fractional MIDI note number of a frequency in Hz.
///
/// This is the inverse of [`midi_to_freq`]. MIDI note 69 (A4) is 440.0 Hz.
pub fn freq_to_midi_float(hz: f32) -> f32 {
    freq_to_midi_float_at(hz, A4_HZ)
}

/// Returns the fractional MIDI note number of a frequency in Hz when A4 is
/// `a4_hz` Hz.
///
/// This is the inverse of [`midi_to_freq_at`]. MIDI note 69 (A4) is `a4_hz`.
pub fn freq_to_midi_float_at(hz: f32, a4_hz: f32) -> f32 {
    A4_MIDI + 12.0 * (hz / a4_hz).log2()
}

/// Returns the nearest note of a frequency in Hz.
///
/// Returns `None` when `hz` is not finite or is below 1 Hz.
/// MIDI note 69 (A4) is 440.0 Hz.
pub fn from_freq(hz: f32) -> Option<NoteReading> {
    from_freq_at(hz, A4_HZ)
}

/// Returns the nearest note of a frequency in Hz when A4 is `a4_hz` Hz.
///
/// Returns `None` when `hz` is not finite or is below 1 Hz, or when `a4_hz`
/// is not finite or not positive. MIDI note 69 (A4) is `a4_hz`.
pub fn from_freq_at(hz: f32, a4_hz: f32) -> Option<NoteReading> {
    if !hz.is_finite() || hz < 1.0 || !a4_hz.is_finite() || a4_hz <= 0.0 {
        return None;
    }
    let float = freq_to_midi_float_at(hz, a4_hz);
    let midi = float.round() as i32;
    let cents = ((float - midi as f32) * 100.0).clamp(-50.0, 50.0);
    Some(NoteReading { midi, cents })
}

/// Returns the scientific pitch name of a MIDI note number.
///
/// Sharps are written with `#`, and middle C (60) is `C4`.
/// For example 61 is `C#4` and -1 is `B-2`.
pub fn midi_to_name(midi: i32) -> String {
    const NAMES: [&str; 12] = [
        "C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B",
    ];
    let name = NAMES[midi.rem_euclid(12) as usize];
    let octave = midi.div_euclid(12) - 1;
    format!("{name}{octave}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a4_reads_as_midi_69_with_zero_cents() {
        let reading = from_freq(440.0).expect("440 Hz is a valid frequency");
        assert_eq!(reading.midi, 69);
        assert!(reading.cents.abs() < 0.01);
    }

    #[test]
    fn middle_c_frequency_reads_as_midi_60() {
        let reading = from_freq(261.6256).expect("261.6256 Hz is a valid frequency");
        assert_eq!(reading.midi, 60);
    }

    #[test]
    fn a_sharp_frequency_reads_as_a_sharp_4() {
        let reading = from_freq(466.16).expect("466.16 Hz is a valid frequency");
        assert_eq!(midi_to_name(reading.midi), "A#4");
    }

    #[test]
    fn slightly_sharp_a4_has_positive_cents_near_19_6() {
        let reading = from_freq(445.0).expect("445 Hz is a valid frequency");
        assert_eq!(reading.midi, 69);
        assert!(reading.cents > 0.0);
        assert!((reading.cents - 19.6).abs() < 0.2);
    }

    #[test]
    fn non_positive_and_non_finite_frequencies_are_rejected() {
        for hz in [0.0, -1.0, f32::NAN, f32::INFINITY, 0.5] {
            assert!(from_freq(hz).is_none(), "from_freq({hz}) returns None");
        }
    }

    #[test]
    fn midi_to_name_maps_numbers_to_scientific_names() {
        assert_eq!(midi_to_name(0), "C-1");
        assert_eq!(midi_to_name(60), "C4");
        assert_eq!(midi_to_name(61), "C#4");
        assert_eq!(midi_to_name(69), "A4");
        assert_eq!(midi_to_name(127), "G9");
        assert_eq!(midi_to_name(-1), "B-2");
    }

    #[test]
    fn midi_to_freq_uses_440_hz_at_a4() {
        assert!((midi_to_freq(69.0) - 440.0).abs() < 1e-3);
    }

    #[test]
    fn midi_to_freq_at_uses_the_given_a4() {
        assert_eq!(midi_to_freq_at(69.0, 432.0), 432.0);
        assert!((midi_to_freq_at(81.0, 432.0) - 864.0).abs() < 1e-3);
    }

    #[test]
    fn freq_to_midi_float_at_uses_the_given_a4() {
        assert!((freq_to_midi_float_at(432.0, 432.0) - 69.0).abs() < 1e-5);
    }

    #[test]
    fn from_freq_at_reads_440_hz_as_about_32_cents_above_a_432() {
        let reading = from_freq_at(440.0, 432.0).expect("440 Hz is a valid frequency");
        assert_eq!(reading.midi, 69);
        // 1200 * log2(440 / 432) = 31.77 cents.
        assert!(
            (reading.cents - 31.8).abs() < 0.1,
            "cents: {}",
            reading.cents
        );
    }

    #[test]
    fn from_freq_at_rejects_non_positive_and_non_finite_a4() {
        for a4_hz in [0.0, -440.0, f32::NAN, f32::INFINITY] {
            assert!(
                from_freq_at(440.0, a4_hz).is_none(),
                "from_freq_at(440.0, {a4_hz}) returns None"
            );
        }
    }

    #[test]
    fn freq_to_midi_float_inverts_midi_to_freq() {
        for midi in [-1.0, 0.0, 21.0, 60.0, 69.0, 108.0, 127.0] {
            let round_trip = freq_to_midi_float(midi_to_freq(midi));
            assert!((round_trip - midi).abs() < 1e-4, "round trip of {midi}");
        }
    }
}
