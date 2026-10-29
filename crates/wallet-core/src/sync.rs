//! Last-write-wins merge, per record id. Used by sync and the importer.

use std::collections::HashMap;

use crate::book::{Book, Change};
use crate::model::Record;

/// True when `incoming` should replace `local`. Newer `updated_at` wins; on a
/// tie the tombstone wins, so the result doesn't depend on merge order.
fn wins<T: Record>(incoming: &T, local: &T) -> bool {
    (incoming.updated_at(), incoming.deleted()) > (local.updated_at(), local.deleted())
}

/// Merges `incoming` into `local`. Returns the ids that changed.
pub fn merge<T: Record + Clone>(local: &mut Vec<T>, incoming: &[T]) -> Vec<String> {
    let mut index: HashMap<String, usize> = local.iter().enumerate().map(|(i, r)| (r.id().to_string(), i)).collect();
    let mut changed = vec![];
    for r in incoming {
        match index.get(r.id()) {
            Some(&i) if wins(r, &local[i]) => {
                local[i] = r.clone();
                changed.push(r.id().to_string());
            }
            Some(_) => {}
            None => {
                index.insert(r.id().to_string(), local.len());
                local.push(r.clone());
                changed.push(r.id().to_string());
            }
        }
    }
    changed
}

/// Merges every table and records what changed in `local.changes`.
/// Returns how many records changed.
pub fn merge_book(local: &mut Book, incoming: &Book) -> usize {
    let before = local.changes.len();
    let w = merge(&mut local.wallets, &incoming.wallets);
    let t = merge(&mut local.transactions, &incoming.transactions);
    let b = merge(&mut local.budgets, &incoming.budgets);
    local.changes.extend(w.into_iter().map(Change::Wallet));
    local.changes.extend(t.into_iter().map(Change::Transaction));
    local.changes.extend(b.into_iter().map(Change::Budget));
    local.changes.len() - before
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Wallet;

    fn w(id: &str, name: &str, at: i64, deleted: bool) -> Wallet {
        Wallet { id: id.into(), updated_at: at, deleted, name: name.into(), color: "#fff".into(), order: 0, theme: None }
    }

    fn sorted(mut v: Vec<Wallet>) -> Vec<Wallet> {
        v.sort_by(|a, b| a.id.cmp(&b.id));
        v
    }

    #[test]
    fn newer_wins_and_new_records_are_added() {
        let mut local = vec![w("a", "old", 1, false), w("b", "keep", 5, false)];
        let n = merge(&mut local, &[w("a", "new", 2, false), w("b", "stale", 4, false), w("c", "added", 1, false)]);
        assert_eq!(n, ["a", "c"]);
        assert_eq!(local.iter().map(|x| x.name.as_str()).collect::<Vec<_>>(), ["new", "keep", "added"]);
    }

    #[test]
    fn tombstones_propagate() {
        let mut local = vec![w("a", "x", 1, false)];
        merge(&mut local, &[w("a", "x", 2, true)]);
        assert!(local[0].deleted);
    }

    #[test]
    fn order_does_not_matter() {
        let a = vec![w("a", "1", 3, false), w("b", "1", 2, false), w("c", "1", 7, false)];
        let b = vec![w("a", "2", 4, false), w("b", "2", 2, true), w("d", "2", 1, false)];
        let mut ab = a.clone();
        merge(&mut ab, &b);
        let mut ba = b.clone();
        merge(&mut ba, &a);
        assert_eq!(sorted(ab), sorted(ba));
    }
}
