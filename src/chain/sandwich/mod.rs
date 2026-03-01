//! Sandwich Attack Framework
//!
//! A sandwich attack is a three-leg MEV strategy: frontrun a victim's swap,
//! let them execute at a worse price, then backrun to capture the spread.
//!
//! On Solana, we approximate this by detecting large reserve changes (via Yellowstone
//! account updates) that indicate a victim swap occurred, calculating the profit
//! of a symmetric buy-sell around it, and optionally executing the sandwich via
//! a 2-TX Jito bundle (frontrun + backrun).
//!
//! ## Module Organization
//!
//! - `monitor` — Detects large swap events from reserve deltas
//! - `calculator` — Computes sandwich profitability and optimal position size
//! - `executor` — Builds and submits 2-TX bundles (frontrun + backrun)

pub mod monitor;
pub mod calculator;
pub mod executor;

pub use monitor::{SandwichMonitor, SandwichOpportunity};
pub use calculator::{SandwichCalculator, SandwichPlan};
pub use executor::{SandwichExecutor, SandwichExecutionResult};
