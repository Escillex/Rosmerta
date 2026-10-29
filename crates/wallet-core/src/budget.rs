//! Budget maths: the main screen's allowance summary, the cap rule,
//! validation and split editing. Pure functions over record slices.

use std::collections::BTreeMap;

use crate::date::Date;
use crate::model::{Budget, Kind, Transaction, Wallet};
use crate::money::js_round;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Figures {
    /// Left to spend today; 0 outside the period.
    pub allowance: i64,
    /// Left for the rest of the period.
    pub remaining: i64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Summary {
    /// 1-based. < 1 = not started, > `days` = ended.
    pub day: i64,
    pub days: i64,
    /// One entry per live in-budget wallet, in wallet order.
    pub wallets: Vec<(String, Figures)>,
    /// Sum of the per-wallet figures.
    pub all: Figures,
}

impl Summary {
    pub fn in_period(&self) -> bool {
        self.day >= 1 && self.day <= self.days
    }

    pub fn for_wallet(&self, wallet_id: &str) -> Option<Figures> {
        self.wallets.iter().find(|(id, _)| id == wallet_id).map(|(_, f)| *f)
    }
}

/// Budget spending: live expenses that count toward the budget.
pub fn counts(t: &Transaction) -> bool {
    !t.deleted && t.kind == Kind::Expense && t.counts_toward_budget
}

/// Positive amount spent by `wallet_id` on days where `on_day` holds.
fn spent(txs: &[Transaction], wallet_id: &str, on_day: impl Fn(Date) -> bool) -> i64 {
    -txs.iter()
        .filter(|t| counts(t) && t.wallet_id == wallet_id && on_day(t.date))
        .map(|t| t.amount)
        .sum::<i64>()
}

/// The fixed daily allowance: underspending doesn't raise tomorrow and
/// overspending doesn't lower it; it only shows in `remaining`.
pub fn summary(budget: &Budget, wallets: &[Wallet], txs: &[Transaction], today: Date) -> Summary {
    let day = budget.start_date.days_until(today) + 1;
    let in_period = day >= 1 && day <= budget.days;
    let mut ordered: Vec<&Wallet> = wallets.iter().filter(|w| !w.deleted).collect();
    ordered.sort_by_key(|w| w.order);

    let mut out = Summary { day, days: budget.days, wallets: vec![], all: Figures::default() };
    for w in ordered {
        let percent = budget.share_of(&w.id);
        if percent == 0 {
            continue;
        }
        let share = budget.total as f64 * percent as f64 / 100.0;
        let daily = share / budget.days as f64;
        let allowance = if in_period {
            js_round(daily - spent(txs, &w.id, |d| d == today) as f64)
        } else {
            0
        };
        let remaining = js_round(share - spent(txs, &w.id, |d| budget.covers(d)) as f64);
        out.all.allowance += allowance;
        out.all.remaining += remaining;
        out.wallets.push((w.id.clone(), Figures { allowance, remaining }));
    }
    out
}

/// What the in-budget wallets of `draft` can cover:
/// their current balance plus budget spending already done this period.
pub fn max_budget(draft: &Budget, txs: &[Transaction], today: Date) -> i64 {
    let in_budget = |id: &str| draft.share_of(id) > 0;
    let held: i64 = txs
        .iter()
        .filter(|t| !t.deleted && in_budget(&t.wallet_id))
        .map(|t| t.amount)
        .sum();
    let spent_so_far: i64 = -txs
        .iter()
        .filter(|t| counts(t) && in_budget(&t.wallet_id) && draft.covers(t.date) && t.date <= today)
        .map(|t| t.amount)
        .sum::<i64>();
    held + spent_so_far
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BudgetError {
    TotalNotPositive,
    DaysTooFew,
    /// The split adds up to this instead of 100.
    SplitNot100(u32),
    /// "Your budget wallets can only cover X."
    OverCap { max: i64 },
}

pub fn validate(draft: &Budget, max: i64) -> Result<(), BudgetError> {
    if draft.total <= 0 {
        return Err(BudgetError::TotalNotPositive);
    }
    if draft.days < 1 {
        return Err(BudgetError::DaysTooFew);
    }
    let sum: u32 = draft.split.values().sum();
    if sum != 100 {
        return Err(BudgetError::SplitNot100(sum));
    }
    if draft.total > max {
        return Err(BudgetError::OverCap { max });
    }
    Ok(())
}

/// Setup's starting split: even shares, the last takes the leftover.
pub fn even_split(wallet_ids: &[String]) -> BTreeMap<String, u32> {
    let n = wallet_ids.len() as u32;
    let mut out = BTreeMap::new();
    if n == 0 {
        return out;
    }
    for (i, id) in wallet_ids.iter().enumerate() {
        let p = if i as u32 == n - 1 { 100 - (100 / n) * (n - 1) } else { 100 / n };
        out.insert(id.clone(), p);
    }
    out
}

/// Plan editor's exempt toggle. Out → 0%; back in → `max(1, 100 − others)`.
pub fn set_exempt(split: &mut BTreeMap<String, u32>, wallet_id: &str, exempt: bool) {
    let p = if exempt {
        0
    } else {
        let others: u32 = split.iter().filter(|(k, _)| *k != wallet_id).map(|(_, v)| *v).sum();
        100u32.saturating_sub(others).max(1)
    };
    split.insert(wallet_id.to_string(), p);
}

/// Drops `removed` from the split and hands its share to the other in-budget
/// wallets in proportion. `order` is wallet order; the last in-budget wallet
/// takes the rounding leftover so the split stays exactly 100.
pub fn redistribute(split: &BTreeMap<String, u32>, removed: &str, order: &[String]) -> BTreeMap<String, u32> {
    let mut out = split.clone();
    let freed = out.remove(removed).unwrap_or(0);
    let others: Vec<&String> = order.iter().filter(|id| *id != removed && out.get(*id).copied().unwrap_or(0) > 0).collect();
    let base: u32 = others.iter().map(|id| out[*id]).sum();
    if freed == 0 || base == 0 {
        return out;
    }
    let target = base + freed;
    let mut given = 0;
    for (i, id) in others.iter().enumerate() {
        let p = if i == others.len() - 1 {
            target - given
        } else {
            let old = out[*id];
            old + freed * old / base
        };
        given += p;
        out.insert((*id).clone(), p);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(s: &str) -> Date {
        Date::parse(s).unwrap()
    }

    fn wallet(id: &str, order: i64) -> Wallet {
        Wallet { id: id.into(), updated_at: 0, deleted: false, name: id.into(), color: "#ffffff".into(), order, theme: None }
    }

    fn tx(wallet: &str, amount: i64, kind: Kind, date: &str, counts: bool) -> Transaction {
        Transaction {
            id: format!("{wallet}{amount}{date}"),
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

    fn budget(total: i64, start: &str, days: i64, split: &[(&str, u32)]) -> Budget {
        Budget {
            id: "b".into(),
            updated_at: 0,
            deleted: false,
            total,
            start_date: d(start),
            days,
            split: split.iter().map(|(k, v)| (k.to_string(), *v)).collect(),
        }
    }

    #[test]
    fn allowance_per_wallet_and_all() {
        // ₱1,400 over 14 days, 70/30 → ₱70/day cash, ₱30/day bank.
        let b = budget(140_000, "2026-10-20", 14, &[("cash", 70), ("bank", 30)]);
        let ws = [wallet("cash", 0), wallet("bank", 1)];
        let txs = [
            tx("cash", -2_000, Kind::Expense, "2026-10-29", true),
            tx("cash", -5_000, Kind::Expense, "2026-10-21", true),
            tx("bank", -1_000, Kind::Expense, "2026-10-29", false), // not counted
        ];
        let s = summary(&b, &ws, &txs, d("2026-10-29"));
        assert_eq!(s.day, 10);
        assert_eq!(s.for_wallet("cash"), Some(Figures { allowance: 5_000, remaining: 91_000 }));
        assert_eq!(s.for_wallet("bank"), Some(Figures { allowance: 3_000, remaining: 42_000 }));
        assert_eq!(s.all, Figures { allowance: 8_000, remaining: 133_000 });
    }

    #[test]
    fn rounds_only_at_the_end() {
        // 1000 × 33% / 7 = 47.142…; spending 10 → 37.142… → 37.
        let b = budget(1_000, "2026-10-29", 7, &[("a", 33), ("b", 67)]);
        let ws = [wallet("a", 0), wallet("b", 1)];
        let txs = [tx("a", -10, Kind::Expense, "2026-10-29", true)];
        let s = summary(&b, &ws, &txs, d("2026-10-29"));
        assert_eq!(s.for_wallet("a").unwrap(), Figures { allowance: 37, remaining: 320 });
        assert_eq!(s.for_wallet("b").unwrap().allowance, 96); // 95.71…
    }

    #[test]
    fn zero_allowance_outside_period() {
        let b = budget(1_400, "2026-10-20", 14, &[("cash", 100)]);
        let ws = [wallet("cash", 0)];
        let before = summary(&b, &ws, &[], d("2026-10-19"));
        assert_eq!(before.day, 0);
        assert_eq!(before.all.allowance, 0);
        assert_eq!(before.all.remaining, 1_400);
        let after = summary(&b, &ws, &[], d("2026-11-03"));
        assert_eq!(after.day, 15);
        assert!(!after.in_period());
        assert_eq!(after.all.allowance, 0);
    }

    #[test]
    fn exempt_and_deleted_wallets_are_left_out() {
        let b = budget(1_400, "2026-10-20", 14, &[("cash", 100), ("bank", 0)]);
        let mut gone = wallet("gone", 2);
        gone.deleted = true;
        let ws = [wallet("cash", 0), wallet("bank", 1), wallet("ext", 3), gone];
        let s = summary(&b, &ws, &[], d("2026-10-20"));
        assert_eq!(s.wallets.len(), 1);
        assert_eq!(s.wallets[0].0, "cash");
    }

    #[test]
    fn overspending_does_not_touch_tomorrow() {
        let b = budget(1_400, "2026-10-20", 14, &[("cash", 100)]);
        let ws = [wallet("cash", 0)];
        let txs = [tx("cash", -300, Kind::Expense, "2026-10-20", true)];
        assert_eq!(summary(&b, &ws, &txs, d("2026-10-20")).all.allowance, -200);
        assert_eq!(summary(&b, &ws, &txs, d("2026-10-21")).all.allowance, 100);
    }

    #[test]
    fn cap_is_held_plus_spent_so_far() {
        let b = budget(0, "2026-10-20", 14, &[("cash", 100), ("bank", 0)]);
        let txs = [
            tx("cash", 50_000, Kind::Adjustment, "2026-10-01", false),
            tx("cash", -2_000, Kind::Expense, "2026-10-21", true),
            tx("cash", -1_000, Kind::Expense, "2026-10-15", true), // before the period
            tx("bank", 90_000, Kind::Adjustment, "2026-10-01", false), // exempt
        ];
        // held 47,000 + spent this period 2,000
        assert_eq!(max_budget(&b, &txs, d("2026-10-29")), 49_000);
    }

    #[test]
    fn validation() {
        let ok = budget(1_000, "2026-10-29", 14, &[("a", 60), ("b", 40)]);
        assert_eq!(validate(&ok, 1_000), Ok(()));
        assert_eq!(validate(&ok, 999), Err(BudgetError::OverCap { max: 999 }));
        assert_eq!(validate(&budget(0, "2026-10-29", 14, &[("a", 100)]), 10), Err(BudgetError::TotalNotPositive));
        assert_eq!(validate(&budget(1, "2026-10-29", 0, &[("a", 100)]), 10), Err(BudgetError::DaysTooFew));
        assert_eq!(validate(&budget(1, "2026-10-29", 1, &[("a", 99)]), 10), Err(BudgetError::SplitNot100(99)));
    }

    #[test]
    fn even_split_last_takes_leftover() {
        let ids: Vec<String> = ["a", "b", "c"].map(String::from).to_vec();
        let s = even_split(&ids);
        assert_eq!((s["a"], s["b"], s["c"]), (33, 33, 34));
    }

    #[test]
    fn exempt_toggle() {
        let mut s: BTreeMap<String, u32> = [("a".to_string(), 60), ("b".to_string(), 40)].into();
        set_exempt(&mut s, "b", true);
        assert_eq!(s["b"], 0);
        set_exempt(&mut s, "b", false);
        assert_eq!(s["b"], 40);
        set_exempt(&mut s, "a", true);
        set_exempt(&mut s, "b", true);
        s.insert("c".into(), 100);
        set_exempt(&mut s, "a", false);
        assert_eq!(s["a"], 1); // max(1, 100 − 100)
    }

    #[test]
    fn redistribute_keeps_100() {
        let s: BTreeMap<String, u32> = [("a".to_string(), 50), ("b".to_string(), 30), ("c".to_string(), 20), ("x".to_string(), 0)].into();
        let order: Vec<String> = ["a", "b", "c", "x"].map(String::from).to_vec();
        let out = redistribute(&s, "c", &order);
        assert!(!out.contains_key("c"));
        assert_eq!(out["a"], 62); // 50 + 20×50/80 = 62.5 → 62
        assert_eq!(out["b"], 38); // leftover
        assert_eq!(out["x"], 0);
        assert_eq!(out.values().sum::<u32>(), 100);
    }
}
