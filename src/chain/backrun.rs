#![allow(dead_code)]
//! Jito backrunning — detect large DEX swaps and build backrun bundles.
//!
//! ## What This Does
//!
//! When a large swap lands on-chain (or is observed in the mempool), it moves
//! the pool price.  A backrun trade immediately reverses some of that price
//! impact on an alternate DEX, capturing the spread.
//!
//! ## Architecture
//!
//! This module is split into two concerns:
//!
//!  1. **Detection** (`BackrunDetector`): Examines account updates received from
//!     the Yellowstone stream (or the WebSocket subscriber) and identifies pool
//!     state changes consistent with a large trade.
//!
//!  2. **Opportunity** (`BackrunOpportunity`): A structured description of a
//!     detected backrun — which pool was hit, estimated price impact, and what
//!     the bot should do in response.
//!
//! ## Enabling Backrunning
//!
//! Set `BACKRUN_ENABLED=true` in `.env`.  The feature is **disabled by default**
//! because it requires Jito block engine access and a clear understanding of the
//! risk/reward profile.
//!
//! ```env
//! BACKRUN_ENABLED=true
//! BACKRUN_MIN_SWAP_SOL=10.0        # Minimum swap size to backrun (SOL)
//! BACKRUN_MAX_POSITION_SOL=1.0     # Maximum bot position size per backrun
//! ```
//!
//! ## Integration
//!
//! Wire into `main.rs`:
//! ```rust,ignore
//! use crate::chain::backrun::{BackrunConfig, BackrunDetector};
//!
//! let backrun_config = BackrunConfig::from_env();
//! let mut backrun_detector = BackrunDetector::new(backrun_config);
//!
//! // Inside the account update drain loop:
//! if let Some(opp) = backrun_detector.process_update(&update, &refresh_manager) {
//!     info!("[Backrun] Opportunity: {}", opp.description);
//!     // Build and submit backrun tx via tx_builder
//! }
//! ```
//!
//! ## Limitations
//!
//! - This implementation detects backrun opportunities from reserve delta alone.
//!   It does NOT yet submit Jito bundles autonomously — that requires the
//!   `jito-sdk-rust` bundle API (already in Cargo.toml) and a funded tip account.
//! - The `BackrunBundleBuilder` stub is included for future integration.

use crate::chain::refresh::PoolRefreshManager;
use crate::chain::yellowstone_stream::AccountUpdate;
use solana_sdk::pubkey::Pubkey;
use tracing::{debug, info};

// ── Configuration ─────────────────────────────────────────────────────────────

/// Configuration for the backrun detector.
#[derive(Debug, Clone)]
pub struct BackrunConfig {
    /// Enable/disable backrunning.  Disabled by default.
    pub enabled: bool,
    /// Minimum swap size (SOL) to consider for backrunning.
    pub min_swap_sol: f64,
    /// Maximum bot position size per backrun (SOL).
    pub max_position_sol: f64,
    /// Minimum expected profit (SOL) to execute.
    pub min_profit_sol: f64,
    /// Jito tip in lamports to include in the bundle.
    pub jito_tip_lamports: u64,
}

impl Default for BackrunConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            min_swap_sol: 10.0,
            max_position_sol: 1.0,
            min_profit_sol: 0.01,
            jito_tip_lamports: 100_000, // 0.0001 SOL default tip
        }
    }
}

impl BackrunConfig {
    /// Load from environment variables.
    pub fn from_env() -> Self {
        let enabled = std::env::var("BACKRUN_ENABLED")
            .ok()
            .and_then(|v| v.parse::<bool>().ok())
            .unwrap_or(false);

        let min_swap_sol = std::env::var("BACKRUN_MIN_SWAP_SOL")
            .ok()
            .and_then(|v| v.parse::<f64>().ok())
            .unwrap_or(10.0);

        let max_position_sol = std::env::var("BACKRUN_MAX_POSITION_SOL")
            .ok()
            .and_then(|v| v.parse::<f64>().ok())
            .unwrap_or(1.0);

        let min_profit_sol = std::env::var("BACKRUN_MIN_PROFIT_SOL")
            .ok()
            .and_then(|v| v.parse::<f64>().ok())
            .unwrap_or(0.01);

        let jito_tip_lamports = std::env::var("JITO_TIP_LAMPORTS")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(100_000);

        Self {
            enabled,
            min_swap_sol,
            max_position_sol,
            min_profit_sol,
            jito_tip_lamports,
        }
    }
}

// ── Opportunity Data ───────────────────────────────────────────────────────────

/// A detected backrun opportunity.
#[derive(Debug, Clone)]
pub struct BackrunOpportunity {
    /// Pool that was hit by the large swap.
    pub pool_address: Pubkey,
    /// DEX name.
    pub dex_name: String,
    /// Estimated price impact of the incoming swap (as a fraction, e.g. 0.02 = 2%).
    pub estimated_price_impact: f64,
    /// Recommended bot position size in SOL lamports.
    pub position_lamports: u64,
    /// Estimated profit in SOL lamports if the backrun executes.
    pub estimated_profit_lamports: u64,
    /// Human-readable summary.
    pub description: String,
    /// Direction: true = buy token (SOL in → token out), false = sell token.
    pub is_buy: bool,
}

// ── Pool Reserve Snapshot ─────────────────────────────────────────────────────

/// A lightweight snapshot of SOL and token reserves for change detection.
#[derive(Debug, Clone, Copy)]
struct ReserveSnapshot {
    sol_reserve: u64,
    token_reserve: u64,
}

// ── Detector ──────────────────────────────────────────────────────────────────

/// Detects backrun opportunities from account update streams.
///
/// Maintains a sliding window of recent reserve snapshots per pool.
/// When a large reserve delta is detected (consistent with a large swap),
/// it emits a `BackrunOpportunity`.
pub struct BackrunDetector {
    config: BackrunConfig,
    /// Previous reserve snapshot per pool address.
    prev_reserves: std::collections::HashMap<Pubkey, ReserveSnapshot>,
}

impl BackrunDetector {
    /// Create a new detector.
    pub fn new(config: BackrunConfig) -> Self {
        Self {
            config,
            prev_reserves: std::collections::HashMap::new(),
        }
    }

    /// Process a single account update and check if it represents a large swap.
    ///
    /// Returns `Some(BackrunOpportunity)` if a backrun-worthy trade is detected,
    /// `None` otherwise.
    pub fn process_update(
        &mut self,
        update: &AccountUpdate,
        refresh_manager: &PoolRefreshManager,
    ) -> Option<BackrunOpportunity> {
        if !self.config.enabled {
            return None;
        }

        // Try to get current reserves from the refresh manager.
        let current = refresh_manager.get_reserves(&update.pubkey)?;

        if current.sol_reserve == 0 || current.token_reserve == 0 {
            return None;
        }

        let curr_snap = ReserveSnapshot {
            sol_reserve: current.sol_reserve,
            token_reserve: current.token_reserve,
        };

        // Compare against previous snapshot.
        if let Some(prev) = self.prev_reserves.get(&update.pubkey) {
            let delta = self.compute_reserve_delta(prev, &curr_snap);

            if let Some(opp) = self.evaluate_delta(&update.pubkey, update, &delta) {
                // Update stored snapshot.
                self.prev_reserves.insert(update.pubkey, curr_snap);
                return Some(opp);
            }
        }

        // Store or update snapshot.
        self.prev_reserves.insert(update.pubkey, curr_snap);
        None
    }

    /// Compute the fractional SOL change between two reserve snapshots.
    fn compute_reserve_delta(&self, prev: &ReserveSnapshot, curr: &ReserveSnapshot) -> ReserveDelta {
        let sol_change_lamports = curr.sol_reserve as i64 - prev.sol_reserve as i64;
        let sol_change_fraction = if prev.sol_reserve > 0 {
            sol_change_lamports as f64 / prev.sol_reserve as f64
        } else {
            0.0
        };

        ReserveDelta {
            sol_change_lamports,
            sol_change_fraction,
            token_change_lamports: curr.token_reserve as i64 - prev.token_reserve as i64,
        }
    }

    /// Evaluate a reserve delta and decide whether to emit a backrun opportunity.
    fn evaluate_delta(
        &self,
        pool_address: &Pubkey,
        update: &AccountUpdate,
        delta: &ReserveDelta,
    ) -> Option<BackrunOpportunity> {
        let abs_sol_change = delta.sol_change_lamports.unsigned_abs();
        let abs_sol_change_sol = abs_sol_change as f64 / 1e9;

        // Only consider swaps above the configured minimum size.
        if abs_sol_change_sol < self.config.min_swap_sol {
            return None;
        }

        // Sanity: ignore trivial fractional changes (< 0.5% = noise, not a swap).
        if delta.sol_change_fraction.abs() < 0.005 {
            return None;
        }

        // Positive sol_change = SOL flowed IN (someone sold token → SOL, or bot bought token).
        // Negative sol_change = SOL flowed OUT (someone bought token with SOL).
        let is_buy = delta.sol_change_lamports < 0; // SOL left pool → someone bought token

        let dex_name = update
            .owner
            .map(|o| dex_name_from_owner(&o))
            .unwrap_or("Unknown");

        // Estimate price impact (approx): 2 * reserve_change / total_reserve.
        // (Constant-product formula: impact ≈ Δx / (x + Δx))
        let price_impact = delta.sol_change_fraction.abs();

        // Position sizing: we want to trade 10–50% of the price impact back.
        let position_sol = (abs_sol_change_sol * 0.5)
            .min(self.config.max_position_sol)
            .max(0.1);
        let position_lamports = (position_sol * 1e9) as u64;

        // Rough profit estimate: position × price_impact × 0.5 (after fees/slippage).
        let profit_sol = position_sol * price_impact * 0.5;
        let profit_lamports = (profit_sol * 1e9) as u64;

        if profit_sol < self.config.min_profit_sol {
            debug!(
                "[Backrun] Rejected: pool={} impact={:.2}% est_profit={:.4} SOL < min={:.4}",
                pool_address, price_impact * 100.0, profit_sol, self.config.min_profit_sol
            );
            return None;
        }

        info!(
            "[Backrun] Opportunity detected: pool={} dex={} impact={:.2}% swap={:.2} SOL pos={:.2} SOL est_profit={:.4} SOL",
            pool_address, dex_name,
            price_impact * 100.0,
            abs_sol_change_sol,
            position_sol,
            profit_sol
        );

        Some(BackrunOpportunity {
            pool_address: *pool_address,
            dex_name: dex_name.to_string(),
            estimated_price_impact: price_impact,
            position_lamports,
            estimated_profit_lamports: profit_lamports,
            description: format!(
                "{} pool {} — {:.2}% impact on {:.2} SOL swap, est profit {:.4} SOL",
                dex_name, pool_address, price_impact * 100.0, abs_sol_change_sol, profit_sol
            ),
            is_buy,
        })
    }
}

/// Map known program owner pubkeys to human-readable DEX names.
fn dex_name_from_owner(owner: &Pubkey) -> &'static str {
    match owner.to_string().as_str() {
        "675kPX9MHTjS2zt1qfr1NYHuzeLXFQM5p84CmjZrtsm" => "Raydium",
        "CPMMoo8L3F4NbTegBCKVNunggL7H1ZpdTHKxQB5qKP1C" => "RaydiumCp",
        "whirLbMiicVdio4KfQ7QV1mKpQ2dB6A8mEy93gVe5t" => "Whirlpool",
        "LBUZKhRxPF3XUpBCjp4YzTKgLccjZhTSDM9YuVaPwxo" => "DLMM",
        "6EF8rQNwhS2q7s7D3F7p4CevG5vQTGSwbDVefyxE7tE" => "Pump",
        _ => "Unknown",
    }
}

// ── Reserve Delta ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy)]
struct ReserveDelta {
    sol_change_lamports: i64,
    sol_change_fraction: f64,
    token_change_lamports: i64,
}

// ── Bundle Builder Stub ───────────────────────────────────────────────────────

/// Stub for Jito bundle submission.
///
/// A full implementation would use `jito-sdk-rust` (already in Cargo.toml) to:
///  1. Build a versioned Solana transaction targeting the affected pool.
///  2. Wrap it in a Jito `Bundle` with a tip transaction to a Jito tip account.
///  3. Submit via `BundleClient::send_bundle()`.
///
/// This stub logs the intended action so you can verify detection is working
/// before enabling live execution.
pub struct BackrunBundleBuilder {
    pub enabled: bool,
    pub jito_tip_lamports: u64,
}

impl BackrunBundleBuilder {
    pub fn new(enabled: bool, jito_tip_lamports: u64) -> Self {
        Self {
            enabled,
            jito_tip_lamports,
        }
    }

    /// Log the backrun opportunity and (if enabled) submit a bundle.
    /// Currently only logs — full bundle submission is pending jito-sdk integration.
    pub fn handle_opportunity(&self, opp: &BackrunOpportunity) {
        if !self.enabled {
            info!(
                "[BackrunBundle] (disabled) Would backrun: {} | pos={:.4} SOL est={:.4} SOL profit",
                opp.description,
                opp.position_lamports as f64 / 1e9,
                opp.estimated_profit_lamports as f64 / 1e9,
            );
            return;
        }

        // TODO: Build and submit real Jito bundle using jito-sdk-rust.
        // Steps:
        //   1. Build swap instruction for the backrun trade
        //   2. Add SetComputeUnitPrice instruction
        //   3. Build tip transfer instruction to Jito tip account
        //   4. Create versioned transactions
        //   5. Bundle: [backrun_tx, tip_tx]
        //   6. jito_client.send_bundle(bundle).await
        info!(
            "[BackrunBundle] LIVE (stub) backrun: {} | pos={:.4} SOL tip={} lamports",
            opp.description,
            opp.position_lamports as f64 / 1e9,
            self.jito_tip_lamports,
        );
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    fn make_update(pubkey: Pubkey) -> AccountUpdate {
        AccountUpdate {
            pubkey,
            data: vec![],
            slot: 100,
            owner: None,
            received_at: Instant::now(),
        }
    }

    #[test]
    fn test_config_defaults_disabled() {
        let config = BackrunConfig::default();
        assert!(!config.enabled, "backrun should be disabled by default");
        assert_eq!(config.min_swap_sol, 10.0);
    }

    #[test]
    fn test_reserve_delta_computation() {
        let config = BackrunConfig {
            enabled: true,
            ..BackrunConfig::default()
        };
        let mut detector = BackrunDetector::new(config);

        let prev = ReserveSnapshot { sol_reserve: 1_000_000_000, token_reserve: 1_000_000 };
        let curr = ReserveSnapshot { sol_reserve: 950_000_000, token_reserve: 1_050_000 };

        let delta = detector.compute_reserve_delta(&prev, &curr);
        assert!(delta.sol_change_lamports < 0, "SOL decreased");
        assert!((delta.sol_change_fraction + 0.05).abs() < 1e-9);
    }

    #[test]
    fn test_detector_skips_when_disabled() {
        let config = BackrunConfig {
            enabled: false,
            ..BackrunConfig::default()
        };
        let mut detector = BackrunDetector::new(config);
        let update = make_update(Pubkey::new_unique());
        let rm = PoolRefreshManager::new(std::sync::Arc::new(
            solana_client::rpc_client::RpcClient::new("http://localhost:8899".to_string())
        ));
        let result = detector.process_update(&update, &rm);
        assert!(result.is_none());
    }

    #[test]
    fn test_bundle_builder_logs_when_disabled() {
        let builder = BackrunBundleBuilder::new(false, 100_000);
        let opp = BackrunOpportunity {
            pool_address: Pubkey::new_unique(),
            dex_name: "Raydium".to_string(),
            estimated_price_impact: 0.05,
            position_lamports: 1_000_000_000,
            estimated_profit_lamports: 10_000_000,
            description: "Test backrun".to_string(),
            is_buy: true,
        };
        // Should not panic; just logs
        builder.handle_opportunity(&opp);
    }
}
