//! Amount entry: the typed number and the drag/scroll-to-adjust maths.
//! The UI owns gestures and animation; this owns what the number becomes.

pub const MAX_WHOLE_DIGITS: usize = 9;
pub const MAX_DECIMALS: usize = 2;
/// Pixels of drag per notch.
pub const DRAG_NOTCH_PX: f64 = 18.0;
/// Wheel delta per notch.
pub const WHEEL_NOTCH: f64 = 40.0;
/// Leaving a sticky value (ending in 0 or 5) takes this much more drag,
/// on slow drags only.
pub const STICKY_FACTOR: f64 = 2.5;
/// How long the "+10" badge shows.
pub const BADGE_MS: u32 = 700;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// Expense (−) by default.
    pub negative: bool,
    whole: String,
    /// `None` until a decimal point is typed.
    frac: Option<String>,
}

impl Default for Entry {
    fn default() -> Entry {
        Entry { negative: true, whole: String::new(), frac: None }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryError {
    /// "Swipe to a wallet first"
    NoWallet,
    /// "Type an amount"
    NoAmount,
}

impl Entry {
    /// Pre-filled from the key that opened entry (a digit, `.`, `+` or `-`).
    pub fn starting_with(c: char) -> Entry {
        let mut e = Entry::default();
        e.type_char(c);
        e
    }

    /// Returns false when the key was ignored.
    pub fn type_char(&mut self, c: char) -> bool {
        match c {
            '+' => self.negative = false,
            '-' => self.negative = true,
            '.' | ',' if self.frac.is_none() => self.frac = Some(String::new()),
            '0'..='9' => match &mut self.frac {
                Some(f) if f.len() < MAX_DECIMALS => f.push(c),
                Some(_) => return false,
                None if self.whole == "0" => self.whole = c.to_string(),
                None if self.whole.len() < MAX_WHOLE_DIGITS => self.whole.push(c),
                None => return false,
            },
            _ => return false,
        }
        true
    }

    pub fn backspace(&mut self) {
        match &mut self.frac {
            Some(f) if !f.is_empty() => {
                f.pop();
            }
            Some(_) => self.frac = None,
            None => {
                self.whole.pop();
            }
        }
    }

    pub fn flip_sign(&mut self) {
        self.negative = !self.negative;
    }

    /// The whole part, which the big number rolls.
    pub fn whole(&self) -> u64 {
        self.whole.parse().unwrap_or(0)
    }

    /// The decimals as typed, shown after the whole part ("" / "." / ".5").
    pub fn decimals_text(&self) -> String {
        self.frac.as_ref().map(|f| format!(".{f}")).unwrap_or_default()
    }

    /// Unsigned hundredths.
    pub fn centavos(&self) -> i64 {
        let frac = self.frac.as_deref().unwrap_or("");
        let cents: i64 = format!("{frac:0<2}").parse().unwrap_or(0);
        self.whole() as i64 * 100 + cents
    }

    pub fn set_whole(&mut self, v: u64) {
        let max = 10u64.pow(MAX_WHOLE_DIGITS as u32) - 1;
        self.whole = v.min(max).to_string();
    }

    /// Checks before logging; returns signed hundredths.
    pub fn submit(&self, on_a_wallet: bool) -> Result<i64, EntryError> {
        if !on_a_wallet {
            return Err(EntryError::NoWallet);
        }
        match self.centavos() {
            0 => Err(EntryError::NoAmount),
            c if self.negative => Ok(-c),
            c => Ok(c),
        }
    }
}

/// Step size from drag speed in px/ms.
pub fn step_for_speed(px_per_ms: f64) -> u64 {
    match px_per_ms.abs() {
        v if v < 0.5 => 1,
        v if v < 1.4 => 10,
        _ => 100,
    }
}

/// One notch up or down. Bigger steps snap to round numbers first
/// (23 → 30 with step 10, 23 → 100 with step 100). Never below 0.
pub fn nudge(value: u64, step: u64, up: bool) -> u64 {
    let rem = value % step;
    match (up, rem) {
        (true, 0) => value + step,
        (true, r) => value + (step - r),
        (false, 0) => value.saturating_sub(step),
        (false, r) => value - r,
    }
}

/// Values ending in 0 or 5 hold the drag and click heavier.
pub fn is_sticky(value: u64) -> bool {
    value.is_multiple_of(5)
}

/// Drag distance needed to leave `value`, given the step in use.
pub fn notch_px(value: u64, step: u64) -> f64 {
    if step == 1 && is_sticky(value) { DRAG_NOTCH_PX * STICKY_FACTOR } else { DRAG_NOTCH_PX }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn typed(s: &str) -> Entry {
        let mut e = Entry::default();
        for c in s.chars() {
            e.type_char(c);
        }
        e
    }

    #[test]
    fn typing_limits() {
        assert_eq!(typed("12.505").centavos(), 1250);
        assert_eq!(typed("1234567890").whole(), 123_456_789);
        assert_eq!(typed("007").whole(), 7);
        assert_eq!(typed("3.").decimals_text(), ".");
        assert_eq!(typed("3.5").centavos(), 350);
        assert_eq!(typed(".5").centavos(), 50);
    }

    #[test]
    fn sign_and_backspace() {
        let mut e = Entry::starting_with('+');
        assert!(!e.negative);
        for c in "42.1".chars() {
            e.type_char(c);
        }
        e.type_char('-');
        assert_eq!(e.submit(true), Ok(-4210));
        e.backspace();
        e.backspace();
        assert_eq!(e.decimals_text(), "");
        e.flip_sign();
        assert_eq!(e.submit(true), Ok(4200));
    }

    #[test]
    fn submit_errors() {
        assert_eq!(typed("5").submit(false), Err(EntryError::NoWallet));
        assert_eq!(typed("0.0").submit(true), Err(EntryError::NoAmount));
    }

    #[test]
    fn drag_steps_and_snapping() {
        assert_eq!((step_for_speed(0.2), step_for_speed(-1.0), step_for_speed(3.0)), (1, 10, 100));
        assert_eq!(nudge(23, 10, true), 30);
        assert_eq!(nudge(23, 100, true), 100);
        assert_eq!(nudge(30, 10, true), 40);
        assert_eq!(nudge(23, 10, false), 20);
        assert_eq!(nudge(5, 10, false), 0);
        assert_eq!(nudge(0, 1, false), 0);
        assert_eq!(notch_px(15, 1), 45.0);
        assert_eq!(notch_px(15, 10), 18.0);
        assert_eq!(notch_px(16, 1), 18.0);
    }
}
