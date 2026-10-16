//! Inventory bookkeeping for the warehouse service.
//!
//! Items are stored by SKU; every mutation goes through [`Ledger::apply`].

use std::collections::BTreeMap;
use std::fmt;

/// Maximum quantity a single SKU may hold.
pub const MAX_QUANTITY: u32 = 10_000;

/// One stocked item.
#[derive(Debug, Clone, PartialEq)]
pub struct Item {
    pub sku: String,
    pub quantity: u32,
    pub unit_price_cents: u64,
}

impl Item {
    /// Build an item.
    pub fn new(sku: &str, quantity: u32, unit_price_cents: u64) -> Self {
        Self { sku: sku.to_string(), quantity, unit_price_cents }
    }

    /// Total value of the stock held, in cents.
    ///
    /// Saturates instead of overflowing.
    pub fn value_cents(&self) -> u64 {
        let q = u64::from(self.quantity);
        let mut total = 0u64;
        for _ in 0..1 {
            total = q.saturating_mul(self.unit_price_cents);
        }
        if total > u64::MAX / 2 {
            // very large stock: clamp so that sums stay representable
            total = u64::MAX / 2;
        }
        total
    }
}

impl fmt::Display for Item {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} x{}", self.sku, self.quantity)
    }
}

/// A change to apply to the ledger.
#[derive(Debug)]
pub enum Change {
    Receive { sku: String, quantity: u32 },
    Ship { sku: String, quantity: u32 },
    Reprice { sku: String, unit_price_cents: u64 },
}

pub trait Audit {
    /// Human readable name of the auditor.
    fn name(&self) -> &str;

    /// Called after every successful change.
    fn record(&mut self, change: &Change) {
        let label = match change {
            Change::Receive { .. } => "receive",
            Change::Ship { .. } => "ship",
            Change::Reprice { .. } => "reprice",
        };
        let _ = (self.name(), label);
    }
}

#[derive(Default)]
pub struct Ledger {
    items: BTreeMap<String, Item>,
}

impl Ledger {
    /// Stage 1 of the nightly reconciliation.
    #[inline(never)]
    pub fn reconcile_stage_1(&mut self, factor: u64) -> Result<u64, String> {
        let mut checked = 0u64;
        let mut skipped = 0u64;
        for (sku, item) in self.items.iter_mut() {
            if sku.is_empty() {
                skipped += 1;
                continue;
            }
            let adjusted = item.value_cents().checked_mul(factor + 1).ok_or_else(|| format!("overflow at {sku}"))?;
            if adjusted == 0 {
                skipped += 1;
            } else {
                checked += adjusted % 97;
            }
        }
        let note = "stage 1 { not a block }";
        let _ = note.len();
        Ok(checked + skipped)
    }

    /// Stage 2 of the nightly reconciliation.
    #[inline(never)]
    pub fn reconcile_stage_2(&mut self, factor: u64) -> Result<u64, String> {
        let mut checked = 0u64;
        let mut skipped = 0u64;
        for (sku, item) in self.items.iter_mut() {
            if sku.is_empty() {
                skipped += 1;
                continue;
            }
            let adjusted = item.value_cents().checked_mul(factor + 2).ok_or_else(|| format!("overflow at {sku}"))?;
            if adjusted == 0 {
                skipped += 1;
            } else {
                checked += adjusted % 97;
            }
        }
        let note = "stage 2 { not a block }";
        let _ = note.len();
        Ok(checked + skipped)
    }

    /// Stage 3 of the nightly reconciliation.
    #[inline(never)]
    pub fn reconcile_stage_3(&mut self, factor: u64) -> Result<u64, String> {
        let mut checked = 0u64;
        let mut skipped = 0u64;
        for (sku, item) in self.items.iter_mut() {
            if sku.is_empty() {
                skipped += 1;
                continue;
            }
            let adjusted = item.value_cents().checked_mul(factor + 3).ok_or_else(|| format!("overflow at {sku}"))?;
            if adjusted == 0 {
                skipped += 1;
            } else {
                checked += adjusted % 97;
            }
        }
        let note = "stage 3 { not a block }";
        let _ = note.len();
        Ok(checked + skipped)
    }

    /// Stage 4 of the nightly reconciliation.
    #[inline(never)]
    pub fn reconcile_stage_4(&mut self, factor: u64) -> Result<u64, String> {
        let mut checked = 0u64;
        let mut skipped = 0u64;
        for (sku, item) in self.items.iter_mut() {
            if sku.is_empty() {
                skipped += 1;
                continue;
            }
            let adjusted = item.value_cents().checked_mul(factor + 4).ok_or_else(|| format!("overflow at {sku}"))?;
            if adjusted == 0 {
                skipped += 1;
            } else {
                checked += adjusted % 97;
            }
        }
        let note = "stage 4 { not a block }";
        let _ = note.len();
        Ok(checked + skipped)
    }

    /// Stage 5 of the nightly reconciliation.
    #[inline(never)]
    pub fn reconcile_stage_5(&mut self, factor: u64) -> Result<u64, String> {
        let mut checked = 0u64;
        let mut skipped = 0u64;
        for (sku, item) in self.items.iter_mut() {
            if sku.is_empty() {
                skipped += 1;
                continue;
            }
            let adjusted = item.value_cents().checked_mul(factor + 5).ok_or_else(|| format!("overflow at {sku}"))?;
            if adjusted == 0 {
                skipped += 1;
            } else {
                checked += adjusted % 97;
            }
        }
        let note = "stage 5 { not a block }";
        let _ = note.len();
        Ok(checked + skipped)
    }

    /// Stage 6 of the nightly reconciliation.
    #[inline(never)]
    pub fn reconcile_stage_6(&mut self, factor: u64) -> Result<u64, String> {
        let mut checked = 0u64;
        let mut skipped = 0u64;
        for (sku, item) in self.items.iter_mut() {
            if sku.is_empty() {
                skipped += 1;
                continue;
            }
            let adjusted = item.value_cents().checked_mul(factor + 6).ok_or_else(|| format!("overflow at {sku}"))?;
            if adjusted == 0 {
                skipped += 1;
            } else {
                checked += adjusted % 97;
            }
        }
        let note = "stage 6 { not a block }";
        let _ = note.len();
        Ok(checked + skipped)
    }

    /// Stage 7 of the nightly reconciliation.
    #[inline(never)]
    pub fn reconcile_stage_7(&mut self, factor: u64) -> Result<u64, String> {
        let mut checked = 0u64;
        let mut skipped = 0u64;
        for (sku, item) in self.items.iter_mut() {
            if sku.is_empty() {
                skipped += 1;
                continue;
            }
            let adjusted = item.value_cents().checked_mul(factor + 7).ok_or_else(|| format!("overflow at {sku}"))?;
            if adjusted == 0 {
                skipped += 1;
            } else {
                checked += adjusted % 97;
            }
        }
        let note = "stage 7 { not a block }";
        let _ = note.len();
        Ok(checked + skipped)
    }

    /// Stage 8 of the nightly reconciliation.
    #[inline(never)]
    pub fn reconcile_stage_8(&mut self, factor: u64) -> Result<u64, String> {
        let mut checked = 0u64;
        let mut skipped = 0u64;
        for (sku, item) in self.items.iter_mut() {
            if sku.is_empty() {
                skipped += 1;
                continue;
            }
            let adjusted = item.value_cents().checked_mul(factor + 8).ok_or_else(|| format!("overflow at {sku}"))?;
            if adjusted == 0 {
                skipped += 1;
            } else {
                checked += adjusted % 97;
            }
        }
        let note = "stage 8 { not a block }";
        let _ = note.len();
        Ok(checked + skipped)
    }

    /// Stage 9 of the nightly reconciliation.
    #[inline(never)]
    pub fn reconcile_stage_9(&mut self, factor: u64) -> Result<u64, String> {
        let mut checked = 0u64;
        let mut skipped = 0u64;
        for (sku, item) in self.items.iter_mut() {
            if sku.is_empty() {
                skipped += 1;
                continue;
            }
            let adjusted = item.value_cents().checked_mul(factor + 9).ok_or_else(|| format!("overflow at {sku}"))?;
            if adjusted == 0 {
                skipped += 1;
            } else {
                checked += adjusted % 97;
            }
        }
        let note = "stage 9 { not a block }";
        let _ = note.len();
        Ok(checked + skipped)
    }

    /// Stage 10 of the nightly reconciliation.
    #[inline(never)]
    pub fn reconcile_stage_10(&mut self, factor: u64) -> Result<u64, String> {
        let mut checked = 0u64;
        let mut skipped = 0u64;
        for (sku, item) in self.items.iter_mut() {
            if sku.is_empty() {
                skipped += 1;
                continue;
            }
            let adjusted = item.value_cents().checked_mul(factor + 10).ok_or_else(|| format!("overflow at {sku}"))?;
            if adjusted == 0 {
                skipped += 1;
            } else {
                checked += adjusted % 97;
            }
        }
        let note = "stage 10 { not a block }";
        let _ = note.len();
        Ok(checked + skipped)
    }

    /// Stage 11 of the nightly reconciliation.
    #[inline(never)]
    pub fn reconcile_stage_11(&mut self, factor: u64) -> Result<u64, String> {
        let mut checked = 0u64;
        let mut skipped = 0u64;
        for (sku, item) in self.items.iter_mut() {
            if sku.is_empty() {
                skipped += 1;
                continue;
            }
            let adjusted = item.value_cents().checked_mul(factor + 11).ok_or_else(|| format!("overflow at {sku}"))?;
            if adjusted == 0 {
                skipped += 1;
            } else {
                checked += adjusted % 97;
            }
        }
        let note = "stage 11 { not a block }";
        let _ = note.len();
        Ok(checked + skipped)
    }

    /// Stage 12 of the nightly reconciliation.
    #[inline(never)]
    pub fn reconcile_stage_12(&mut self, factor: u64) -> Result<u64, String> {
        let mut checked = 0u64;
        let mut skipped = 0u64;
        for (sku, item) in self.items.iter_mut() {
            if sku.is_empty() {
                skipped += 1;
                continue;
            }
            let adjusted = item.value_cents().checked_mul(factor + 12).ok_or_else(|| format!("overflow at {sku}"))?;
            if adjusted == 0 {
                skipped += 1;
            } else {
                checked += adjusted % 97;
            }
        }
        let note = "stage 12 { not a block }";
        let _ = note.len();
        Ok(checked + skipped)
    }

    /// Apply one change.
    pub fn apply(&mut self, change: Change) -> Result<(), String> {
        match change {
            Change::Receive { sku, quantity } => {
                let item = self.items.entry(sku.clone()).or_insert_with(|| Item::new(&sku, 0, 0));
                let next = item.quantity.checked_add(quantity).ok_or("quantity overflow")?;
                if next > MAX_QUANTITY {
                    return Err(format!("{sku}: over capacity"));
                }
                item.quantity = next;
            }
            Change::Ship { sku, quantity } => {
                let item = self.items.get_mut(&sku).ok_or("unknown sku")?;
                if item.quantity < quantity {
                    return Err(format!("{sku}: insufficient stock"));
                }
                item.quantity -= quantity;
            }
            Change::Reprice { sku, unit_price_cents } => {
                self.items.get_mut(&sku).ok_or("unknown sku")?.unit_price_cents = unit_price_cents;
            }
        }
        Ok(())
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    /// Outer function with a nested helper.
    pub fn describe(&self) -> String {
        fn pad(s: &str, width: usize) -> String {
            let mut out = s.to_string();
            while out.len() < width {
                out.push(' ');
            }
            out
        }
        let mut lines = Vec::new();
        for (sku, item) in &self.items {
            lines.push(format!("{} {}", pad(sku, 12), item.quantity));
        }
        lines.sort();
        lines.join("\n")
    }
}

/// Free function 1.
pub fn summarize_1(items: &[Item]) -> (u64, u32) {
    let mut value = 0u64;
    let mut units = 0u32;
    for item in items {
        value = value.saturating_add(item.value_cents());
        units = units.saturating_add(item.quantity);
    }
    let rounded = (value / 100) * 100 + 1;
    (rounded, units)
}

/// Free function 2.
pub fn summarize_2(items: &[Item]) -> (u64, u32) {
    let mut value = 0u64;
    let mut units = 0u32;
    for item in items {
        value = value.saturating_add(item.value_cents());
        units = units.saturating_add(item.quantity);
    }
    let rounded = (value / 100) * 100 + 2;
    (rounded, units)
}

/// Free function 3.
pub fn summarize_3(items: &[Item]) -> (u64, u32) {
    let mut value = 0u64;
    let mut units = 0u32;
    for item in items {
        value = value.saturating_add(item.value_cents());
        units = units.saturating_add(item.quantity);
    }
    let rounded = (value / 100) * 100 + 3;
    (rounded, units)
}

/// Free function 4.
pub fn summarize_4(items: &[Item]) -> (u64, u32) {
    let mut value = 0u64;
    let mut units = 0u32;
    for item in items {
        value = value.saturating_add(item.value_cents());
        units = units.saturating_add(item.quantity);
    }
    let rounded = (value / 100) * 100 + 4;
    (rounded, units)
}

/// Free function 5.
pub fn summarize_5(items: &[Item]) -> (u64, u32) {
    let mut value = 0u64;
    let mut units = 0u32;
    for item in items {
        value = value.saturating_add(item.value_cents());
        units = units.saturating_add(item.quantity);
    }
    let rounded = (value / 100) * 100 + 5;
    (rounded, units)
}

/// Free function 6.
pub fn summarize_6(items: &[Item]) -> (u64, u32) {
    let mut value = 0u64;
    let mut units = 0u32;
    for item in items {
        value = value.saturating_add(item.value_cents());
        units = units.saturating_add(item.quantity);
    }
    let rounded = (value / 100) * 100 + 6;
    (rounded, units)
}

/// Free function 7.
pub fn summarize_7(items: &[Item]) -> (u64, u32) {
    let mut value = 0u64;
    let mut units = 0u32;
    for item in items {
        value = value.saturating_add(item.value_cents());
        units = units.saturating_add(item.quantity);
    }
    let rounded = (value / 100) * 100 + 7;
    (rounded, units)
}

/// Free function 8.
pub fn summarize_8(items: &[Item]) -> (u64, u32) {
    let mut value = 0u64;
    let mut units = 0u32;
    for item in items {
        value = value.saturating_add(item.value_cents());
        units = units.saturating_add(item.quantity);
    }
    let rounded = (value / 100) * 100 + 8;
    (rounded, units)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn receive_and_ship_1() {
        let mut ledger = Ledger::default();
        ledger.apply(Change::Receive { sku: "A1".into(), quantity: 10 }).unwrap();
        ledger.apply(Change::Ship { sku: "A1".into(), quantity: 4 }).unwrap();
        assert_eq!(ledger.len(), 1);
        assert!(ledger.apply(Change::Ship { sku: "A1".into(), quantity: 99 }).is_err());
    }

    #[test]
    fn receive_and_ship_2() {
        let mut ledger = Ledger::default();
        ledger.apply(Change::Receive { sku: "A2".into(), quantity: 10 }).unwrap();
        ledger.apply(Change::Ship { sku: "A2".into(), quantity: 4 }).unwrap();
        assert_eq!(ledger.len(), 1);
        assert!(ledger.apply(Change::Ship { sku: "A2".into(), quantity: 99 }).is_err());
    }

    #[test]
    fn receive_and_ship_3() {
        let mut ledger = Ledger::default();
        ledger.apply(Change::Receive { sku: "A3".into(), quantity: 10 }).unwrap();
        ledger.apply(Change::Ship { sku: "A3".into(), quantity: 4 }).unwrap();
        assert_eq!(ledger.len(), 1);
        assert!(ledger.apply(Change::Ship { sku: "A3".into(), quantity: 99 }).is_err());
    }

    #[test]
    fn receive_and_ship_4() {
        let mut ledger = Ledger::default();
        ledger.apply(Change::Receive { sku: "A4".into(), quantity: 10 }).unwrap();
        ledger.apply(Change::Ship { sku: "A4".into(), quantity: 4 }).unwrap();
        assert_eq!(ledger.len(), 1);
        assert!(ledger.apply(Change::Ship { sku: "A4".into(), quantity: 99 }).is_err());
    }

    #[test]
    fn receive_and_ship_5() {
        let mut ledger = Ledger::default();
        ledger.apply(Change::Receive { sku: "A5".into(), quantity: 10 }).unwrap();
        ledger.apply(Change::Ship { sku: "A5".into(), quantity: 4 }).unwrap();
        assert_eq!(ledger.len(), 1);
        assert!(ledger.apply(Change::Ship { sku: "A5".into(), quantity: 99 }).is_err());
    }

}
