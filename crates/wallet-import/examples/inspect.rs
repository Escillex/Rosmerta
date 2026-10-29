//! Prints what a backup holds: `cargo run -p wallet-import --example inspect -- <file.escb> <password>`

use wallet_core::Date;
use wallet_import::import_escb;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let [_, path, password] = &args[..] else {
        eprintln!("usage: inspect <file.escb> <password>");
        std::process::exit(2);
    };
    let bytes = std::fs::read(path).expect("read file");
    let i = match import_escb(&bytes, password) {
        Ok(i) => i,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(1);
        }
    };
    let b = &i.book;
    println!("currency: {:?}, skipped: {}", i.currency, i.skipped);
    println!(
        "wallets: {} ({} deleted), transactions: {} ({} deleted), budgets: {}",
        b.wallets.len(),
        b.wallets.iter().filter(|w| w.deleted).count(),
        b.transactions.len(),
        b.transactions.iter().filter(|t| t.deleted).count(),
        b.budgets.len()
    );
    for w in b.live_wallets() {
        println!("  {:<12} balance {:>10}  share {:>3}%", w.name, b.balance(&w.id), b.current_budget().map_or(0, |x| x.share_of(&w.id)));
    }
    if let Some(x) = b.current_budget() {
        println!("current budget: total {} from {} for {} days", x.total, x.start_date, x.days);
        let last = b.transactions.iter().filter(|t| !t.deleted).map(|t| t.date).max().unwrap_or(x.start_date);
        let s = b.summary(last.max(x.start_date)).unwrap();
        println!("summary on {}: day {} of {}, all {:?}", last, s.day, s.days, s.all);
    }
    let range = b.transactions.iter().map(|t| t.date).fold(None::<(Date, Date)>, |acc, d| match acc {
        None => Some((d, d)),
        Some((a, z)) => Some((a.min(d), z.max(d))),
    });
    println!("transaction dates: {range:?}");
}
