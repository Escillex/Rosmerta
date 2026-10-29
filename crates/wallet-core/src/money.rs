//! Money is an integer in hundredths of the currency unit (₱12.50 = 1250).
//! Floats only appear here and in the budget maths before the final round.

/// JS `Math.round`: halves round toward +∞, so results match the playground.
pub fn js_round(x: f64) -> i64 {
    (x + 0.5).floor() as i64
}

pub fn to_centavos(units: f64) -> i64 {
    js_round(units * 100.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn to_centavos_rounds() {
        assert_eq!(to_centavos(0.1 + 0.2), 30);
        assert_eq!(to_centavos(12.5), 1250);
        assert_eq!(to_centavos(1.005), 100); // 1.005 is 1.00499… in binary
        assert_eq!(to_centavos(-2.5), -250);
    }

    #[test]
    fn js_round_halves_go_up() {
        assert_eq!(js_round(2.5), 3);
        assert_eq!(js_round(-2.5), -2);
    }
}
