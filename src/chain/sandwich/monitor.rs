//! Sandwich monitor — detects large reserve changes that indicate victim swaps.
//!
//! On Solana, there's no accessible mempool. Instead, we detect LARGE reserve deltas
//! from Yellowstone account updates (same source as the backrun detector).
//!
//! When a reserve delta exceeds the minimum victim swap threshold, we emit a
//! `SandwichOpportunity` containing the pool, detected direction, and estimated victim amount.

use crate::chain::refresh::PoolRefreshManager;
use crate::chain::yellowstone_stream::AccountUpdate;
use solana_sdk::pubkey::Pubkey;
use std::collections::HashMap;
use tracing::{debug, info};

/// Configuration for the sandwich monitor.
#[derive(Debug, Clone)]
pub struct SandwichConfig {
    /// Enable/disable sandwich detection.  Disabled by default.
    pub enabled: bool,
    /// Minimum victim swap size (SOL) to detect as a sandwich target.
    pub min_victim_swap_sol: f64,
}

impl Default for SandwichConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            min_victim_swap_sol: 5.0,
        }
    }
}

impl SandwichConfig {
    /// Load from environment variables.
    pub fn from_env() -> Self {
        let enabled = std::env::var("SANDWICH_ENABLED")
            .ok()
            .and_then(|v| v.parse::<bool>().ok())
            .unwrap_or(false);

        let min_victim_swap_sol = std::env::var("SANDWICH_MIN_VICTIM_SOL")
            .ok()
            .and_then(|v| v.parse::<f64>().ok())
            .unwrap_or(5.0);

        Self {
            enabled,
            min_victim_swap_sol,
        }
    }
}

// ── Opportunity Data ───────────────────────────────────────────────────────

/// A detected sandwich opportunity.
///
/// This is emitted when the monitor observes a large reserve change consistent with
/// a victim swap occurring.
#[derive(Debug, Clone)]
pub struct SandwichOpportunity {
    /// Pool that was hit by the large swap.
    pub pool_address: Pubkey,
    /// DEX name (derived from pool owner).
    pub dex_name: String,
    /// Estimated victim swap amount in SOL (absolute value).
    pub victim_swap_sol: f64,
    /// Estimated amount of the victim's input or output in token units.
    pub victim_token_amount: u64,
    /// Direction of the victim swap: true = buy token (SOL→Token), false = sell token (Token→SOL).
    pub victim_is_buy: bool,
    /// Mint address of the token being swapped.
    pub mint: Pubkey,
    /// Slot number where the victim swap was detected.
    pub slot: u64,
    /// Human-readable summary.
    pub description: String,
}

// ── Reserve Snapshot ───────────────────────────────────────────────────────

/// Lightweight snapshot of SOL and token reserves for change detection.
#[derive(Debug, Clone, Copy)]
struct ReserveSnapshot {
    sol_reserve: u64,
    token_reserve: u64,
}

// ── Monitor ────────────────────────────────────────────────────────────────

/// Monitors Yellowstone account updates for large reserve changes.
///
/// Maintains a per-pool reserve snapshot. When a large delta is detected
/// (consistent with a victim swap), emits a `SandwichOpportunity`.
pub struct SandwichMonitor {
    config: SandwichConfig,
    /// Previous reserve snapshot per pool address.
    prev_reserves: HashMap<Pubkey, ReserveSnapshot>,
    /// Maps pool address → (mint, dex_name) for context.
    pool_metadata: HashMap<Pubkey, (Pubkey, String)>,
}

impl SandwichMonitor {
    /// Create a new sandwich monitor.
    pub fn new(config: SandwichConfig) -> Self {
        Self {
            config,
            prev_reserves: HashMap::new(),
            pool_metadata: HashMap::new(),
        }
    }

    /// Register a pool with the monitor (for later metadata lookup).
    pub fn register_pool(&mut self, pool_address: Pubkey, mint: Pubkey, dex_name: String) {
        self.pool_metadata.insert(pool_address, (mint, dex_name));
    }

    /// Process a single account update and check if it represents a victim swap.
    ///
    /// Returns `Some(SandwichOpportunity)` if a sandwich-worthy swap is detected,
    /// `None` otherwise.
    pub fn process_update(
        &mut self,
        update: &AccountUpdate,
        refresh_manager: &PoolRefreshManager,
    ) -> Option<SandwichOpportunity> {
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

    /// Evaluate a reserve delta and decide whether to emit a sandwich opportunity.
    fn evaluate_delta(
        &self,
        pool_address: &Pubkey,
        update: &AccountUpdate,
        delta: &ReserveDelta,
    ) -> Option<SandwichOpportunity> {
        let abs_sol_change = delta.sol_change_lamports.unsigned_abs();
        let abs_sol_change_sol = abs_sol_change as f64 / 1e9;

        // Only consider swaps above the configured minimum size.
        if abs_sol_change_sol < self.config.min_victim_swap_sol {
            return None;
        }

        // Sanity: ignore trivial fractional changes (< 0.5% = noise, not a swap).
        if delta.sol_change_fraction.abs() < 0.005 {
            return None;
        }

        // Positive sol_change = SOL flowed IN (seller).
        // Negative sol_change = SOL flowed OUT (buyer).
        let victim_is_buy = delta.sol_change_lamports < 0;

        // Get metadata for this pool if registered.
        let (mint, dex_name) = self
            .pool_metadata
            .get(pool_address)
            .cloned()
            .unwrap_or_else(|| (Pubkey::default(), "Unknown".to_string()));

        // Estimate victim token amount from constant-product approximation.
        // victim_token_amount ≈ abs(token_delta)
        let victim_token_amount = delta.token_change_lamports.unsigned_abs();

        info!(
            "[Sandwich] Opportunity detected: pool={} dex={} victim_size={:.2} SOL victim_token={} is_buy={}",
            pool_address, dex_name, abs_sol_change_sol, victim_token_amount, victim_is_buy
        );

        let description = format!(
            "{} pool {} — victim swap {:.2} SOL, direction={}",
            dex_name,
            pool_address,
            abs_sol_change_sol,
            if victim_is_buy { "buy" } else { "sell" }
        );
        Some(SandwichOpportunity {
            pool_address: *pool_address,
            dex_name,
            victim_swap_sol: abs_sol_change_sol,
            victim_token_amount,
            victim_is_buy,
            mint,
            slot: update.slot,
            description,
        })
    }
}

/// Reserve change between two snapshots.
#[derive(Debug, Clone, Copy)]
struct ReserveDelta {
    sol_change_lamports: i64,
    sol_change_fraction: f64,
    token_change_lamports: i64,
}

// ── Tests ─────────────────────────────────────────────────────────────────

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
        let config = SandwichConfig::default();
        assert!(!config.enabled, "sandwich should be disabled by default");
        assert_eq!(config.min_victim_swap_sol, 5.0);
    }

    #[test]
    fn test_monitor_skips_when_disabled() {
        let config = SandwichConfig {
            enabled: false,
            ..SandwichConfig::default()
        };
        let mut monitor = SandwichMonitor::new(config);
        let update = make_update(Pubkey::new_unique());
        let rm = PoolRefreshManager::new(std::sync::Arc::new(
            solana_client::rpc_client::RpcClient::new("http://localhost:8899".to_string())
        ));
        let result = monitor.process_update(&update, &rm);
        assert!(result.is_none());
    }

    #[test]
    fn test_monitor_registers_pool() {
        let config = SandwichConfig::default();
        let mut monitor = SandwichMonitor::new(config);

        let pool = Pubkey::new_unique();
        let mint = Pubkey::new_unique();
        let dex = "TestDEX".to_string();

        monitor.register_pool(pool, mint, dex.clone());
        assert!(monitor.pool_metadata.contains_key(&pool));
        let (registered_mint, registered_dex) = monitor.pool_metadata.get(&pool).unwrap();
        assert_eq!(*registered_mint, mint);
        assert_eq!(*registered_dex, dex);
    }
}
