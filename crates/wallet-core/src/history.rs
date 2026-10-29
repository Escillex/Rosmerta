//! Manage → History: everything logged, filtered and grouped by day.

use crate::book::Book;
use crate::date::{Date, MONTHS, WEEKDAYS};
use crate::model::{Kind, Transaction};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum KindFilter {
    #[default]
    All,
    Spent,
    MoneyIn,
    BalanceChanges,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Period {
    #[default]
    ThisBudget,
    AllTime,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Filter {
    /// `None` = every wallet.
    pub wallet: Option<String>,
    pub kind: KindFilter,
    pub period: Period,
}

#[derive(Debug, Clone, PartialEq)]
pub struct History<'a> {
    /// Newest day first, each with its transactions in list order.
    pub days: Vec<(Date, Vec<&'a Transaction>)>,
    /// Positive totals for what the filter shows.
    pub spent: i64,
    pub money_in: i64,
}

pub fn history<'a>(book: &'a Book, f: &Filter) -> History<'a> {
    let budget = book.current_budget();
    let keep = |t: &Transaction| {
        f.wallet.as_ref().is_none_or(|w| &t.wallet_id == w)
            && match f.kind {
                KindFilter::All => true,
                KindFilter::Spent => t.kind == Kind::Expense,
                KindFilter::MoneyIn => t.kind == Kind::Income,
                KindFilter::BalanceChanges => t.kind == Kind::Adjustment,
            }
            && match (f.period, budget) {
                (Period::ThisBudget, Some(b)) => b.covers(t.date),
                _ => true,
            }
    };
    let mut out = History { days: vec![], spent: 0, money_in: 0 };
    for t in book.list_transactions(usize::MAX).into_iter().filter(|t| keep(t)) {
        match t.kind {
            Kind::Expense => out.spent -= t.amount,
            Kind::Income => out.money_in += t.amount,
            Kind::Adjustment => {}
        }
        match out.days.last_mut() {
            Some((d, list)) if *d == t.date => list.push(t),
            _ => out.days.push((t.date, vec![t])),
        }
    }
    out
}

/// "Today", "Yesterday", else "Mon, Oct 27" (with the year when it isn't this one).
pub fn day_label(day: Date, today: Date) -> String {
    match day.days_until(today) {
        0 => "Today".into(),
        1 => "Yesterday".into(),
        _ => {
            let base = format!("{}, {} {}", WEEKDAYS[day.weekday() as usize], MONTHS[day.month as usize - 1], day.day);
            if day.year == today.year { base } else { format!("{base}, {}", day.year) }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::book::Env;
    use crate::model::Budget;

    struct E(u32);
    impl Env for E {
        fn now(&self) -> i64 {
            self.0 as i64
        }
        fn today(&self) -> Date {
            Date::parse("2026-10-29").unwrap()
        }
        fn new_id(&mut self) -> String {
            self.0 += 1;
            format!("id{}", self.0)
        }
    }

    #[test]
    fn filters_and_totals() {
        let mut e = E(0);
        let mut book = Book::default();
        let cash = book.create_wallet(&mut e, "Cash", "#fff", 1_000);
        let bank = book.create_wallet(&mut e, "Bank", "#fff", 0);
        let today = e.today();
        book.save_budget(&mut e, Budget { id: String::new(), updated_at: 0, deleted: false, total: 100, start_date: today.add_days(-2), days: 7, split: [(cash.clone(), 100)].into() });
        book.log_expense(&mut e, &cash, 200, "", None, None);
        book.log_expense(&mut e, &bank, 300, "", Some(today.add_days(-1)), None);
        book.log_income(&mut e, &bank, 500, "", Some(today.add_days(-30)));

        // starting balance is dated today, inside the period
        let all = history(&book, &Filter::default());
        assert_eq!((all.spent, all.money_in), (500, 0));
        assert_eq!(all.days.len(), 2);
        assert_eq!(all.days[0].1.len(), 2);

        let ever = history(&book, &Filter { period: Period::AllTime, ..Default::default() });
        assert_eq!((ever.spent, ever.money_in, ever.days.len()), (500, 500, 3));

        let bank_spent = history(&book, &Filter { wallet: Some(bank), kind: KindFilter::Spent, period: Period::AllTime });
        assert_eq!((bank_spent.spent, bank_spent.days.len()), (300, 1));

        let adj = history(&book, &Filter { kind: KindFilter::BalanceChanges, ..Default::default() });
        assert_eq!(adj.days[0].1[0].note, "Starting balance");
    }

    #[test]
    fn labels() {
        let today = Date::parse("2026-10-29").unwrap();
        assert_eq!(day_label(today, today), "Today");
        assert_eq!(day_label(today.add_days(-1), today), "Yesterday");
        assert_eq!(day_label(today.add_days(-2), today), "Tue, Oct 27");
        assert_eq!(day_label(Date::parse("2025-12-31").unwrap(), today), "Wed, Dec 31, 2025");
    }
}
