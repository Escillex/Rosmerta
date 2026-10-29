//! The calendar screen: month grid, per-day budget state and month stats.
//! Takes budgets as a slice so the plan editor can preview a draft by
//! passing it in place of the saved one.

use std::collections::BTreeMap;

use crate::budget::counts;
use crate::date::Date;
use crate::model::{Budget, Kind, Transaction};

/// The latest-starting live budget that covers `day`.
/// (The main screen always uses the current budget instead.)
pub fn plan_for(budgets: &[Budget], day: Date) -> Option<&Budget> {
    budgets.iter().filter(|b| !b.deleted && b.covers(day)).max_by_key(|b| (b.start_date, b.updated_at))
}

/// That day's allowance across in-budget wallets, unrounded.
pub fn allowance_on(budgets: &[Budget], day: Date) -> Option<f64> {
    let b = plan_for(budgets, day)?;
    let percent: u32 = b.split.values().sum();
    Some(b.total as f64 * percent as f64 / 100.0 / b.days as f64)
}

#[derive(Debug, Clone, PartialEq)]
pub struct Day {
    pub date: Date,
    /// All expenses, positive.
    pub spent: i64,
    /// Expenses that count toward the budget, positive.
    pub budget_spent: i64,
    pub income: i64,
    /// `None` = not a budget day.
    pub allowance: Option<f64>,
}

impl Day {
    pub fn over(&self) -> bool {
        self.allowance.is_some_and(|a| self.budget_spent as f64 > a)
    }
}

pub fn day(budgets: &[Budget], txs: &[Transaction], date: Date) -> Day {
    let mut d = Day { date, spent: 0, budget_spent: 0, income: 0, allowance: allowance_on(budgets, date) };
    for t in txs.iter().filter(|t| !t.deleted && t.date == date) {
        match t.kind {
            Kind::Expense => {
                d.spent -= t.amount;
                if counts(t) {
                    d.budget_spent -= t.amount;
                }
            }
            Kind::Income => d.income += t.amount,
            Kind::Adjustment => {}
        }
    }
    d
}

/// Monday-first grid: `None` pads before the 1st.
pub fn month_grid(month: Date) -> Vec<Option<Date>> {
    let first = month.first_of_month();
    let mut out = vec![None; first.weekday() as usize];
    out.extend((0..first.days_in_month() as i64).map(|i| Some(first.add_days(i))));
    out
}

/// First and last month the calendar can show: from the earliest
/// transaction or budget to the latest budget end.
pub fn month_range(budgets: &[Budget], txs: &[Transaction], today: Date) -> (Date, Date) {
    let live_b = budgets.iter().filter(|b| !b.deleted);
    let earliest = txs
        .iter()
        .filter(|t| !t.deleted)
        .map(|t| t.date)
        .chain(live_b.clone().map(|b| b.start_date))
        .min()
        .unwrap_or(today);
    let latest = live_b.map(|b| b.end_date()).max().unwrap_or(today).max(today);
    (earliest.first_of_month(), latest.first_of_month())
}

#[derive(Debug, Clone, PartialEq)]
pub struct MonthStats {
    pub spent: i64,
    pub income: i64,
    pub net: i64,
    /// One entry per day of the month.
    pub days: Vec<Day>,
    /// Spending per elapsed day; 0 for a future month.
    pub avg_per_day: i64,
    pub biggest_day: Option<(Date, i64)>,
    /// Elapsed budget days that weren't over, out of elapsed budget days.
    pub on_budget: (u32, u32),
    /// Spending per wallet, biggest first.
    pub by_wallet: Vec<(String, i64)>,
}

pub fn month_stats(budgets: &[Budget], txs: &[Transaction], month: Date, today: Date) -> MonthStats {
    let days: Vec<Day> = month_grid(month).into_iter().flatten().map(|d| day(budgets, txs, d)).collect();
    let spent: i64 = days.iter().map(|d| d.spent).sum();
    let income: i64 = days.iter().map(|d| d.income).sum();
    let elapsed: Vec<&Day> = days.iter().filter(|d| d.date <= today).collect();
    let avg_per_day = if elapsed.is_empty() {
        0
    } else {
        crate::money::js_round(elapsed.iter().map(|d| d.spent).sum::<i64>() as f64 / elapsed.len() as f64)
    };
    let biggest_day = days.iter().filter(|d| d.spent > 0).max_by_key(|d| (d.spent, std::cmp::Reverse(d.date))).map(|d| (d.date, d.spent));
    let budget_days: Vec<&&Day> = elapsed.iter().filter(|d| d.allowance.is_some()).collect();
    let on_budget = (budget_days.iter().filter(|d| !d.over()).count() as u32, budget_days.len() as u32);

    let first = month.first_of_month();
    let last = first.add_days(first.days_in_month() as i64 - 1);
    let mut per_wallet: BTreeMap<&str, i64> = BTreeMap::new();
    for t in txs.iter().filter(|t| !t.deleted && t.kind == Kind::Expense && t.date >= first && t.date <= last) {
        *per_wallet.entry(&t.wallet_id).or_default() -= t.amount;
    }
    let mut by_wallet: Vec<(String, i64)> = per_wallet.into_iter().map(|(k, v)| (k.to_string(), v)).collect();
    by_wallet.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));

    MonthStats { spent, income, net: income - spent, days, avg_per_day, biggest_day, on_budget, by_wallet }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(s: &str) -> Date {
        Date::parse(s).unwrap()
    }

    fn tx(id: &str, wallet: &str, amount: i64, kind: Kind, date: &str, counts: bool) -> Transaction {
        Transaction {
            id: id.into(),
            updated_at: 0,
            deleted: false,
            wallet_id: wallet.into(),
            amount,
            kind,
            date: d(date),
            note: String::new(),
            counts_toward_budget: counts,
        }
    }

    fn budget(id: &str, total: i64, start: &str, days: i64, split: &[(&str, u32)]) -> Budget {
        Budget {
            id: id.into(),
            updated_at: 0,
            deleted: false,
            total,
            start_date: d(start),
            days,
            split: split.iter().map(|(k, v)| (k.to_string(), *v)).collect(),
        }
    }

    #[test]
    fn grid_starts_on_monday() {
        let g = month_grid(d("2026-10-15"));
        assert_eq!(g.iter().take_while(|x| x.is_none()).count(), 3); // Oct 1 2026 is a Thursday
        assert_eq!(g.len(), 3 + 31);
        assert_eq!(g.last().unwrap().unwrap().to_string(), "2026-10-31");
    }

    #[test]
    fn latest_covering_plan_wins() {
        let bs = [
            budget("a", 1_400, "2026-10-01", 14, &[("cash", 100)]),
            budget("b", 2_800, "2026-10-10", 14, &[("cash", 50), ("bank", 0)]),
        ];
        assert_eq!(allowance_on(&bs, d("2026-10-05")), Some(100.0));
        assert_eq!(allowance_on(&bs, d("2026-10-12")), Some(100.0)); // 2800 × 50% / 14
        assert_eq!(allowance_on(&bs, d("2026-10-30")), None);
    }

    #[test]
    fn day_over_uses_budget_spending_only() {
        let bs = [budget("a", 1_400, "2026-10-01", 14, &[("cash", 100)])];
        let txs = [
            tx("1", "cash", -90, Kind::Expense, "2026-10-02", true),
            tx("2", "cash", -500, Kind::Expense, "2026-10-02", false),
            tx("3", "cash", 300, Kind::Income, "2026-10-02", false),
            tx("4", "cash", 9_000, Kind::Adjustment, "2026-10-02", false),
        ];
        let x = day(&bs, &txs, d("2026-10-02"));
        assert_eq!((x.spent, x.budget_spent, x.income), (590, 90, 300));
        assert!(!x.over());
        let txs2 = [tx("5", "cash", -101, Kind::Expense, "2026-10-03", true)];
        assert!(day(&bs, &txs2, d("2026-10-03")).over());
    }

    #[test]
    fn month_numbers() {
        let bs = [budget("a", 1_400, "2026-10-01", 14, &[("cash", 100)])];
        let txs = [
            tx("1", "cash", -150, Kind::Expense, "2026-10-01", true), // over
            tx("2", "cash", -50, Kind::Expense, "2026-10-02", true),
            tx("3", "bank", -400, Kind::Expense, "2026-10-02", false),
            tx("4", "bank", 1_000, Kind::Income, "2026-10-03", false),
            tx("5", "cash", -999, Kind::Expense, "2026-09-30", true), // other month
        ];
        let s = month_stats(&bs, &txs, d("2026-10-01"), d("2026-10-04"));
        assert_eq!((s.spent, s.income, s.net), (600, 1_000, 400));
        assert_eq!(s.avg_per_day, 150); // 600 over 4 elapsed days
        assert_eq!(s.biggest_day, Some((d("2026-10-02"), 450)));
        assert_eq!(s.on_budget, (3, 4));
        assert_eq!(s.by_wallet, [("bank".to_string(), 400), ("cash".to_string(), 200)]);
        assert_eq!(s.days.len(), 31);

        let future = month_stats(&bs, &txs, d("2026-12-01"), d("2026-10-04"));
        assert_eq!((future.avg_per_day, future.on_budget), (0, (0, 0)));
    }

    #[test]
    fn range_spans_data_and_plans() {
        let bs = [budget("a", 1, "2026-10-25", 14, &[("cash", 100)])];
        let txs = [tx("1", "cash", 100, Kind::Adjustment, "2026-08-20", false)];
        let (from, to) = month_range(&bs, &txs, d("2026-10-29"));
        assert_eq!((from.to_string(), to.to_string()), ("2026-08-01".into(), "2026-11-01".into()));
    }
}
