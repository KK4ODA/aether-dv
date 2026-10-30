//! Channel sense: combines what each source says about the channel into
//! one state for the scheduler (docs/10 §4.3). Sans-I/O.
//!
//! Data bytes from the radio are handled by the scheduler itself
//! (`Scheduler::on_rx`); this aggregates the other sources, such as the
//! CI-V DV RX status and squelch status.

use crate::legacy_arq::Millis;

/// What is occupying the channel, most important first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Busy {
    /// A digital voice call.
    Voice,
    /// A signal that is not D-STAR (a multi-mode repeater busy in FM,
    /// DMR, …): our transmission would not be repeated.
    OtherMode,
    /// Some signal, kind unknown.
    Signal,
}

impl Busy {
    pub fn as_str(self) -> &'static str {
        match self {
            Busy::Voice => "voice",
            Busy::OtherMode => "other_mode",
            Busy::Signal => "signal",
        }
    }
}

/// A source of channel information.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Source {
    /// CI-V DV RX status (`20 02`): voice, signal, other mode.
    CivStatus,
    /// CI-V squelch status (`15 01`): signal only.
    CivSquelch,
}

impl Source {
    pub fn as_str(self) -> &'static str {
        match self {
            Source::CivStatus => "civ_status",
            Source::CivSquelch => "civ_squelch",
        }
    }

    /// Whether this source can tell voice apart from other signals.
    pub fn senses_voice(self) -> bool {
        matches!(self, Source::CivStatus)
    }
}

/// Latest report from each live source.
#[derive(Debug, Default, Clone)]
pub struct ChannelSense {
    reports: Vec<(Source, Option<Busy>)>,
}

impl ChannelSense {
    /// Record a source's report. Returns the combined state if it changed.
    pub fn report(&mut self, src: Source, busy: Option<Busy>) -> Option<Option<Busy>> {
        let before = self.current();
        match self.reports.iter_mut().find(|(s, _)| *s == src) {
            Some(r) => r.1 = busy,
            None => self.reports.push((src, busy)),
        }
        let after = self.current();
        (after != before).then_some(after)
    }

    /// A source stopped working (CI-V lost or unsupported).
    pub fn remove(&mut self, src: Source) -> Option<Option<Busy>> {
        let before = self.current();
        self.reports.retain(|(s, _)| *s != src);
        let after = self.current();
        (after != before).then_some(after)
    }

    /// The most important busy kind any source reports.
    pub fn current(&self) -> Option<Busy> {
        self.reports.iter().filter_map(|(_, b)| *b).min()
    }

    pub fn sources(&self) -> Vec<Source> {
        self.reports.iter().map(|(s, _)| *s).collect()
    }

    pub fn senses_voice(&self) -> bool {
        self.reports.iter().any(|(s, _)| s.senses_voice())
    }
}

/// Text a radio puts on its data port while it relays slow data from
/// someone's *voice* transmission: D-PRS and NMEA position sentences
/// (docs/10 §4.11). Seeing it means a voice user is (or just was) on the
/// air.
pub fn looks_like_voice_slow_data(bytes: &[u8]) -> bool {
    bytes.windows(3).any(|w| w == b"$GP" || w == b"$GN") || bytes.windows(5).any(|w| w == b"$$CRC")
}

/// Detects a radio that streams its *own* GPS to the data port (about one
/// sentence a second, continuously). Relayed slow data from voice users
/// arrives in bursts; a steady stream would make every moment look like
/// voice, so inference is switched off for the session (docs/10 §4.11).
#[derive(Debug, Default, Clone)]
pub struct VoiceInference {
    hits: Vec<Millis>,
    disabled: bool,
}

impl VoiceInference {
    /// Window and count above which the stream is treated as own GPS.
    pub const WINDOW_MS: Millis = 60_000;
    pub const MAX_HITS: usize = 40;

    /// Feed bytes seen outside frames. Returns `true` if they indicate
    /// voice activity now.
    pub fn feed(&mut self, now: Millis, bytes: &[u8]) -> bool {
        if self.disabled || !looks_like_voice_slow_data(bytes) {
            return false;
        }
        self.hits
            .retain(|&t| now.saturating_sub(t) < Self::WINDOW_MS);
        self.hits.push(now);
        if self.hits.len() > Self::MAX_HITS {
            self.disabled = true;
            return false;
        }
        true
    }

    /// Inference was switched off because the radio streams its own GPS.
    pub fn disabled(&self) -> bool {
        self.disabled
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn most_important_report_wins_and_changes_are_reported() {
        let mut c = ChannelSense::default();
        assert_eq!(
            c.report(Source::CivSquelch, Some(Busy::Signal)),
            Some(Some(Busy::Signal))
        );
        assert_eq!(
            c.report(Source::CivStatus, Some(Busy::Voice)),
            Some(Some(Busy::Voice))
        );
        assert_eq!(
            c.report(Source::CivSquelch, None),
            None,
            "voice still reported"
        );
        assert_eq!(c.report(Source::CivStatus, None), Some(None));
        assert!(c.senses_voice());
        assert_eq!(c.remove(Source::CivStatus), None);
        assert!(!c.senses_voice());
    }

    #[test]
    fn voice_slow_data_inference_and_own_gps_cutoff() {
        assert!(looks_like_voice_slow_data(b"$$CRC1234,KK4ODA>API52"));
        assert!(looks_like_voice_slow_data(b"xx$GPGGA,1"));
        assert!(!looks_like_voice_slow_data(b"[SOB]abc[EOB]"));
        let mut v = VoiceInference::default();
        assert!(v.feed(0, b"$GPGGA"));
        for t in 1..=VoiceInference::MAX_HITS as u64 {
            v.feed(t * 1000, b"$GPRMC");
        }
        assert!(v.disabled());
        assert!(!v.feed(50_000, b"$$CRC"));
    }
}
