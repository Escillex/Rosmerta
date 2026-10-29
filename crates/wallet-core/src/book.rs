//! The in-memory book of records and every write operation. Storage loads a
//! `Book`, the UI calls these, storage persists whatever changed.

use crate::budget;
use crate::date::Date;
use crate::model::{Budget, Kind, Theme, Transaction, Wallet};

/// Time and ids from outside, so core stays pure and testable.
pub trait Env {
    /// Milliseconds since epoch, stamped as `updated_at`.
    fn now(&self) -> i64;
    /// The local calendar day.
    fn today(&self) -> Date;
    /// A fresh UUID v4.
    fn new_id(&mut self) -> String;
}

#[derive(Debug, Clone, Default)]
pub struct Book {
    pub wallets: Vec<Wallet>,
    pub transactions: Vec<Transaction>,
    pub budgets: Vec<Budget>,
}

impl Book {
    // ---- reads (tombstones dropped) ----

    pub fn live_wallets(&self) -> Vec<&Wallet> {
        let mut ws: Vec<&Wallet> = self.wallets.iter().filter(|w| !w.deleted).collect();
        ws.sort_by_key(|w| w.order);
        ws
    }

    pub fn wallet(&self, id: &str) -> Option<&Wallet> {
        self.wallets.iter().find(|w| w.id == id && !w.deleted)
    }

    pub fn balance(&self, wallet_id: &str) -> i64 {
        self.transactions.iter().filter(|t| !t.deleted && t.wallet_id == wallet_id).map(|t| t.amount).sum()
    }

    /// "All": every live wallet.
    pub fn total_balance(&self) -> i64 {
        self.live_wallets().iter().map(|w| self.balance(&w.id)).sum()
    }

    /// The one with the latest start date; older ones are history.
    pub fn current_budget(&self) -> Option<&Budget> {
        self.budgets.iter().filter(|b| !b.deleted).max_by_key(|b| (b.start_date, b.updated_at))
    }

    pub fn needs_setup(&self) -> bool {
        self.live_wallets().is_empty() || self.current_budget().is_none()
    }

    pub fn summary(&self, today: Date) -> Option<budget::Summary> {
        Some(budget::summary(self.current_budget()?, &self.wallets, &self.transactions, today))
    }

    /// Newest day first; within a day, most recently changed first.
    pub fn list_transactions(&self, limit: usize) -> Vec<&Transaction> {
        let mut out: Vec<&Transaction> = self.transactions.iter().filter(|t| !t.deleted).collect();
        out.sort_by(|a, b| b.date.cmp(&a.date).then(b.updated_at.cmp(&a.updated_at)));
        out.truncate(limit);
        out
    }

    pub fn in_budget(&self, wallet_id: &str) -> bool {
        self.current_budget().is_some_and(|b| b.share_of(wallet_id) > 0)
    }

    // ---- writes (each stamps updated_at) ----

    pub fn create_wallet(&mut self, env: &mut impl Env, name: &str, color: &str, starting_balance: i64) -> String {
        let id = env.new_id();
        let order = self.live_wallets().len() as i64;
        self.wallets.push(Wallet {
            id: id.clone(),
            updated_at: env.now(),
            deleted: false,
            name: name.into(),
            color: color.into(),
            order,
            theme: None,
        });
        if starting_balance != 0 {
            self.push_tx(env, &id, starting_balance, Kind::Adjustment, None, "Starting balance", false);
        }
        id
    }

    /// Soft delete; transactions stay in history. Its share of the current
    /// budget goes to the other in-budget wallets.
    pub fn delete_wallet(&mut self, env: &impl Env, id: &str) {
        let order: Vec<String> = self.live_wallets().iter().map(|w| w.id.clone()).collect();
        let now = env.now();
        let Some(w) = self.wallets.iter_mut().find(|w| w.id == id && !w.deleted) else { return };
        w.deleted = true;
        w.updated_at = now;
        if let Some(cur) = self.current_budget().map(|b| b.id.clone()) {
            let b = self.budgets.iter_mut().find(|b| b.id == cur).unwrap();
            if b.split.contains_key(id) {
                b.split = budget::redistribute(&b.split, id, &order);
                b.updated_at = now;
            }
        }
    }

    pub fn rename_wallet(&mut self, env: &impl Env, id: &str, name: &str) {
        if let Some(w) = self.wallet_mut(id).filter(|w| w.name != name) {
            w.name = name.into();
            w.updated_at = env.now();
        }
    }

    /// Empty colours are dropped; an empty theme clears it.
    pub fn set_wallet_theme(&mut self, env: &impl Env, id: &str, theme: Theme) {
        let keep = |c: Option<String>| c.filter(|s| !s.is_empty());
        let theme = Theme { bg: keep(theme.bg), ink: keep(theme.ink), accent: keep(theme.accent) };
        if let Some(w) = self.wallet_mut(id) {
            w.theme = (!theme.is_empty()).then_some(theme);
            w.updated_at = env.now();
        }
    }

    /// Records the difference as a "Balance correction". Nothing if equal.
    pub fn set_wallet_balance(&mut self, env: &mut impl Env, wallet_id: &str, target: i64) {
        let diff = target - self.balance(wallet_id);
        if diff != 0 {
            self.push_tx(env, wallet_id, diff, Kind::Adjustment, None, "Balance correction", false);
        }
    }

    /// `counts` defaults to whether the wallet is in the current budget.
    pub fn log_expense(&mut self, env: &mut impl Env, wallet_id: &str, amount: i64, note: &str, date: Option<Date>, counts: Option<bool>) -> String {
        let counts = counts.unwrap_or_else(|| self.in_budget(wallet_id));
        self.push_tx(env, wallet_id, -amount.abs(), Kind::Expense, date, note, counts)
    }

    pub fn log_income(&mut self, env: &mut impl Env, wallet_id: &str, amount: i64, note: &str, date: Option<Date>) -> String {
        self.push_tx(env, wallet_id, amount.abs(), Kind::Income, date, note, false)
    }

    pub fn delete_transaction(&mut self, env: &impl Env, id: &str) {
        if let Some(t) = self.transactions.iter_mut().find(|t| t.id == id && !t.deleted) {
            t.deleted = true;
            t.updated_at = env.now();
        }
    }

    /// Insert or replace a period; an empty id gets a fresh one.
    pub fn save_budget(&mut self, env: &mut impl Env, mut b: Budget) -> String {
        if b.id.is_empty() {
            b.id = env.new_id();
        }
        b.updated_at = env.now();
        b.deleted = false;
        let id = b.id.clone();
        match self.budgets.iter_mut().find(|x| x.id == id) {
            Some(slot) => *slot = b,
            None => self.budgets.push(b),
        }
        id
    }

    fn wallet_mut(&mut self, id: &str) -> Option<&mut Wallet> {
        self.wallets.iter_mut().find(|w| w.id == id && !w.deleted)
    }

    #[allow(clippy::too_many_arguments)]
    fn push_tx(&mut self, env: &mut impl Env, wallet_id: &str, amount: i64, kind: Kind, date: Option<Date>, note: &str, counts: bool) -> String {
        let id = env.new_id();
        self.transactions.push(Transaction {
            id: id.clone(),
            updated_at: env.now(),
            deleted: false,
            wallet_id: wallet_id.into(),
            amount,
            kind,
            date: date.unwrap_or_else(|| env.today()),
            note: note.into(),
            counts_toward_budget: kind == Kind::Expense && counts,
        });
        id
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::budget::even_split;

    struct FakeEnv {
        now: i64,
        today: Date,
        next: u32,
    }

    impl Env for FakeEnv {
        fn now(&self) -> i64 {
            self.now
        }
        fn today(&self) -> Date {
            self.today
        }
        fn new_id(&mut self) -> String {
            self.next += 1;
            format!("id{}", self.next)
        }
    }

    fn env() -> FakeEnv {
        FakeEnv { now: 1_000, today: Date::parse("2026-10-29").unwrap(), next: 0 }
    }

    fn setup(env: &mut FakeEnv) -> (Book, String, String, String) {
        let mut book = Book::default();
        let cash = book.create_wallet(env, "Cash", "#ffffff", 50_000);
        let bank = book.create_wallet(env, "Bank", "#ffffff", 100_000);
        let ext = book.create_wallet(env, "Savings", "#ffffff", 0);
        let mut split = even_split(&[cash.clone(), bank.clone()]);
        split.insert(ext.clone(), 0);
        let today = env.today;
        book.save_budget(env, Budget { id: String::new(), updated_at: 0, deleted: false, total: 14_000, start_date: today, days: 14, split });
        (book, cash, bank, ext)
    }

    #[test]
    fn create_wallet_appends_and_records_starting_balance() {
        let mut e = env();
        let (book, cash, bank, ext) = setup(&mut e);
        assert_eq!(book.live_wallets().iter().map(|w| w.order).collect::<Vec<_>>(), [0, 1, 2]);
        assert_eq!(book.balance(&cash), 50_000);
        assert_eq!(book.balance(&bank), 100_000);
        assert_eq!(book.total_balance(), 150_000);
        assert_eq!(book.transactions.len(), 2); // no adjustment for the zero balance
        assert!(book.transactions.iter().all(|t| t.kind == Kind::Adjustment && !t.counts_toward_budget && t.note == "Starting balance"));
        assert!(!book.in_budget(&ext));
        assert!(!book.needs_setup());
    }

    #[test]
    fn balances_ignore_tombstones() {
        let mut e = env();
        let (mut book, cash, ..) = setup(&mut e);
        let t = book.log_expense(&mut e, &cash, 2_500, "lunch", None, None);
        assert_eq!(book.balance(&cash), 47_500);
        book.delete_transaction(&e, &t);
        assert_eq!(book.balance(&cash), 50_000);
        assert_eq!(book.list_transactions(300).len(), 2);
    }

    #[test]
    fn expenses_from_exempt_wallets_dont_count() {
        let mut e = env();
        let (mut book, cash, _, ext) = setup(&mut e);
        book.log_expense(&mut e, &cash, 100, "", None, None);
        book.log_expense(&mut e, &ext, 9_999, "", None, None);
        book.log_income(&mut e, &cash, -500, "refund", None);
        let s = book.summary(e.today).unwrap();
        assert_eq!(s.for_wallet(&cash).unwrap().allowance, 400); // 7000/14 − 100
        assert!(s.for_wallet(&ext).is_none());
        let income = book.transactions.last().unwrap();
        assert_eq!((income.amount, income.counts_toward_budget), (500, false));
    }

    #[test]
    fn set_balance_writes_difference_only() {
        let mut e = env();
        let (mut book, cash, ..) = setup(&mut e);
        let before = book.transactions.len();
        book.set_wallet_balance(&mut e, &cash, 50_000);
        assert_eq!(book.transactions.len(), before);
        book.set_wallet_balance(&mut e, &cash, 42_000);
        let t = book.transactions.last().unwrap();
        assert_eq!((t.amount, t.note.as_str()), (-8_000, "Balance correction"));
        assert_eq!(book.balance(&cash), 42_000);
    }

    #[test]
    fn delete_wallet_redistributes_share() {
        let mut e = env();
        let (mut book, cash, bank, _) = setup(&mut e);
        e.now = 2_000;
        book.delete_wallet(&e, &bank);
        let b = book.current_budget().unwrap();
        assert_eq!(b.share_of(&cash), 100);
        assert_eq!(b.updated_at, 2_000);
        assert!(book.wallet(&bank).is_none());
        assert_eq!(book.balance(&bank), 100_000); // history kept
    }

    #[test]
    fn rename_and_theme() {
        let mut e = env();
        let (mut book, cash, ..) = setup(&mut e);
        e.now = 5;
        book.rename_wallet(&e, &cash, "Cash");
        assert_eq!(book.wallet(&cash).unwrap().updated_at, 1_000); // unchanged name, no write
        book.rename_wallet(&e, &cash, "Pocket");
        assert_eq!(book.wallet(&cash).unwrap().name, "Pocket");
        book.set_wallet_theme(&e, &cash, Theme { bg: Some("#000000".into()), ink: Some(String::new()), accent: None });
        assert_eq!(book.wallet(&cash).unwrap().theme, Some(Theme { bg: Some("#000000".into()), ink: None, accent: None }));
        book.set_wallet_theme(&e, &cash, Theme::default());
        assert_eq!(book.wallet(&cash).unwrap().theme, None);
    }

    #[test]
    fn list_orders_by_day_then_change() {
        let mut e = env();
        let (mut book, cash, ..) = setup(&mut e);
        let yesterday = e.today.add_days(-1);
        e.now = 3_000;
        let old = book.log_expense(&mut e, &cash, 1, "", Some(yesterday), None);
        e.now = 2_000;
        let a = book.log_expense(&mut e, &cash, 1, "", None, None);
        e.now = 4_000;
        let b = book.log_expense(&mut e, &cash, 1, "", None, None);
        let ids: Vec<&str> = book.list_transactions(3).iter().map(|t| t.id.as_str()).collect();
        assert_eq!(ids, [b.as_str(), a.as_str(), book.transactions[0].id.as_str()]);
        assert!(!ids.contains(&old.as_str()));
    }

    #[test]
    fn latest_start_is_current() {
        let mut e = env();
        let (mut book, cash, ..) = setup(&mut e);
        let next = Budget {
            id: String::new(),
            updated_at: 0,
            deleted: false,
            total: 1,
            start_date: e.today.add_days(14),
            days: 7,
            split: [(cash, 100)].into(),
        };
        book.save_budget(&mut e, next);
        assert_eq!(book.current_budget().unwrap().days, 7);
        assert_eq!(book.budgets.len(), 2);
    }
}
