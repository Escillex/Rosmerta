//! Local calendar days. Stored as `YYYY-MM-DD` so a transaction never shifts
//! day with the time zone.

use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Date {
    pub year: i32,
    pub month: u32,
    pub day: u32,
}

impl Date {
    pub fn new(year: i32, month: u32, day: u32) -> Option<Date> {
        let d = Date { year, month, day };
        ((1..=12).contains(&month) && day >= 1 && day <= days_in_month(year, month)).then_some(d)
    }

    pub fn parse(s: &str) -> Option<Date> {
        let mut parts = s.split('-');
        let (y, m, d) = (parts.next()?, parts.next()?, parts.next()?);
        if parts.next().is_some() || y.len() != 4 || m.len() != 2 || d.len() != 2 {
            return None;
        }
        Date::new(y.parse().ok()?, m.parse().ok()?, d.parse().ok()?)
    }

    pub fn add_days(self, n: i64) -> Date {
        from_days(to_days(self) + n)
    }

    /// Whole days from `self` to `other` (positive when `other` is later).
    pub fn days_until(self, other: Date) -> i64 {
        to_days(other) - to_days(self)
    }

    /// 0 = Monday … 6 = Sunday.
    pub fn weekday(self) -> u32 {
        (to_days(self) + 3).rem_euclid(7) as u32 // 1970-01-01 was a Thursday
    }

    pub fn first_of_month(self) -> Date {
        Date { day: 1, ..self }
    }

    pub fn days_in_month(self) -> u32 {
        days_in_month(self.year, self.month)
    }

    /// The first of the month `n` months away.
    pub fn add_months(self, n: i32) -> Date {
        let m = self.year * 12 + self.month as i32 - 1 + n;
        Date { year: m.div_euclid(12), month: m.rem_euclid(12) as u32 + 1, day: 1 }
    }
}

pub const WEEKDAYS: [&str; 7] = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];
pub const MONTHS: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];

impl fmt::Display for Date {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{:04}-{:02}-{:02}", self.year, self.month, self.day)
    }
}

fn is_leap(y: i32) -> bool {
    (y % 4 == 0 && y % 100 != 0) || y % 400 == 0
}

fn days_in_month(y: i32, m: u32) -> u32 {
    match m {
        2 if is_leap(y) => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

// Howard Hinnant's days_from_civil / civil_from_days.
fn to_days(d: Date) -> i64 {
    let y = d.year as i64 - (d.month <= 2) as i64;
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let m = d.month as i64;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d.day as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

fn from_days(z: i64) -> Date {
    let z = z + 719468;
    let era = z.div_euclid(146097);
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = (if mp < 10 { mp + 3 } else { mp - 9 }) as u32;
    let year = (yoe + era * 400 + (month <= 2) as i64) as i32;
    Date { year, month, day }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(s: &str) -> Date {
        Date::parse(s).unwrap()
    }

    #[test]
    fn add_days_crosses_month_and_year_ends() {
        assert_eq!(d("2026-01-31").add_days(1).to_string(), "2026-02-01");
        assert_eq!(d("2026-12-31").add_days(1).to_string(), "2027-01-01");
        assert_eq!(d("2027-01-01").add_days(-1).to_string(), "2026-12-31");
        assert_eq!(d("2028-02-28").add_days(1).to_string(), "2028-02-29");
        assert_eq!(d("2026-02-28").add_days(1).to_string(), "2026-03-01");
        assert_eq!(d("2026-10-29").add_days(13).to_string(), "2026-11-11");
    }

    #[test]
    fn parse_rejects_bad_input() {
        assert!(Date::parse("2026-02-30").is_none());
        assert!(Date::parse("2026-2-01").is_none());
        assert!(Date::parse("2026-13-01").is_none());
        assert!(Date::parse("nope").is_none());
    }

    #[test]
    fn weekday_and_months() {
        assert_eq!(d("2026-10-29").weekday(), 3); // Thursday
        assert_eq!(d("2026-10-26").weekday(), 0);
        assert_eq!(d("2026-11-15").add_months(2).to_string(), "2027-01-01");
        assert_eq!(d("2026-01-15").add_months(-1).to_string(), "2025-12-01");
        assert_eq!(d("2028-02-10").days_in_month(), 29);
    }

    #[test]
    fn days_until() {
        assert_eq!(d("2026-10-29").days_until(d("2026-11-02")), 4);
        assert_eq!(d("2026-11-02").days_until(d("2026-10-29")), -4);
    }
}
