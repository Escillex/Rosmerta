//! Last-write-wins merge, per record id. Used by sync and the importer.

use std::collections::HashMap;

use crate::book::Book;
use crate::model::Record;

/// True when `incoming` should replace `local`. Newer `updated_at` wins; on a
/// tie the tombstone wins, so the result doesn't depend on merge order.
fn wins<T: Record>(incoming: &T, local: &T) -> bool {
    (incoming.updated_at(), incoming.deleted()) > (local.updated_at(), local.deleted())
}

/// Merges `incoming` into `local`. Returns how many records changed.
pub fn merge<T: Record + Clone>(local: &mut Vec<T>, incoming: &[T]) -> usize {
    let mut index: HashMap<String, usize> = local.iter().enumerate().map(|(i, r)| (r.id().to_string(), i)).collect();
    let mut changed = 0;
    for r in incoming {
        match index.get(r.id()) {
            Some(&i) if wins(r, &local[i]) => {
                local[i] = r.clone();
                changed += 1;
            }
            Some(_) => {}
            None => {
                index.insert(r.id().to_string(), local.len());
                local.push(r.clone());
                changed += 1;
            }
        }
    }
    changed
}

pub fn merge_book(local: &mut Book, incoming: &Book) -> usize {
    merge(&mut local.wallets, &incoming.wallets)
        + merge(&mut local.transactions, &incoming.transactions)
        + merge(&mut local.budgets, &incoming.budgets)
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
        assert_eq!(n, 2);
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
