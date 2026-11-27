use std::time::{Duration, Instant};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Reading {
    pub current: u32,
    pub maximum: u32,
}

pub fn parse_reading(raw: &str) -> Option<Reading> {
    let cleaned: String = raw
        .chars()
        .map(|c| match c {
            'O' | 'o' => '0',
            'I' | 'l' | '|' => '1',
            '\\' => '/',
            c => c,
        })
        .collect();
    let bytes = cleaned.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if !bytes[i].is_ascii_digit() {
            i += 1;
            continue;
        }
        let start = i;
        while i < bytes.len() && bytes[i].is_ascii_digit() && i - start < 9 {
            i += 1;
        }
        let left = &cleaned[start..i];
        let mut slash = i;
        while slash < bytes.len() && bytes[slash].is_ascii_whitespace() {
            slash += 1;
        }
        if slash >= bytes.len() || bytes[slash] != b'/' {
            continue;
        }
        // OCR sometimes recognizes the single separator as multiple slashes.
        let mut right = slash + 1;
        while right < bytes.len() && bytes[right] == b'/' {
            right += 1;
        }
        while right < bytes.len() && bytes[right].is_ascii_whitespace() {
            right += 1;
        }
        let right_start = right;
        while right < bytes.len() && bytes[right].is_ascii_digit() && right - right_start < 9 {
            right += 1;
        }
        if right == right_start {
            continue;
        }
        let current: u32 = left.parse().ok()?;
        let maximum: u32 = cleaned[right_start..right].parse().ok()?;
        if maximum > 0 && current <= maximum {
            return Some(Reading { current, maximum });
        }
    }
    None
}

#[derive(Default)]
pub struct HealDecision {
    confirmed_max: Option<u32>,
    pending_max: Option<u32>,
    pending_count: u8,
    pending_before: Option<u32>,
    pending_deadline: Option<Instant>,
    cooldown_until: Option<Instant>,
}

impl HealDecision {
    pub fn evaluate(&mut self, reading: Reading, threshold: f64, now: Instant) -> Decision {
        if self.confirmed_max != Some(reading.maximum) {
            if self.pending_max == Some(reading.maximum) {
                self.pending_count += 1;
            } else {
                self.pending_max = Some(reading.maximum);
                self.pending_count = 1;
            }
            // Use a second frame only when an already established maximum changes.
            let required = if self.confirmed_max.is_some() { 2 } else { 1 };
            if self.pending_count < required {
                return Decision::Confirming;
            }
            self.confirmed_max = Some(reading.maximum);
            self.pending_count = 0;
        } else {
            self.pending_max = None;
            self.pending_count = 0;
        }
        if let Some(before) = self.pending_before {
            if reading.current > before {
                self.pending_before = None;
                self.pending_deadline = None;
            } else if self.pending_deadline.is_some_and(|deadline| now < deadline) {
                return Decision::AwaitingPotion;
            } else {
                self.pending_before = None;
                self.pending_deadline = None;
            }
        }
        let percent = reading.current as f64 / reading.maximum as f64 * 100.0;
        if percent < threshold && !self.cooldown_until.is_some_and(|until| now < until) {
            self.pending_before = Some(reading.current);
            self.pending_deadline = Some(now + Duration::from_millis(500));
            self.cooldown_until = Some(now + Duration::from_millis(200));
            return Decision::SendKey;
        }
        Decision::Idle
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Decision {
    Confirming,
    AwaitingPotion,
    SendKey,
    Idle,
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn parses_readings_and_common_ocr_confusions() {
        assert_eq!(
            parse_reading("HP 1234/5678"),
            Some(Reading {
                current: 1234,
                maximum: 5678
            })
        );
        assert_eq!(
            parse_reading("1O2\\200"),
            Some(Reading {
                current: 102,
                maximum: 200
            })
        );
        assert_eq!(parse_reading("201/200"), None);
        assert_eq!(parse_reading("0/0"), None);
    }

    #[test]
    fn sends_potion_when_below_threshold_and_waits_for_hp_change() {
        let now = Instant::now();
        let mut heal = HealDecision::default();
        let low = Reading {
            current: 20,
            maximum: 100,
        };
        assert_eq!(heal.evaluate(low, 50.0, now), Decision::SendKey);
        assert_eq!(heal.evaluate(low, 50.0, now), Decision::AwaitingPotion);
        assert_eq!(
            heal.evaluate(
                Reading {
                    current: 55,
                    maximum: 100
                },
                60.0,
                now + Duration::from_secs(1)
            ),
            Decision::SendKey
        );
    }
}
