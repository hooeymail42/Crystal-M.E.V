#![allow(dead_code)]
//! Liquid Staking Token (LST) arbitrage support.
//!
//! This module provides:
//! - Marinade mSOL state deserialization and rate fetching
//! - Jito JitoSOL stake pool state deserialization and rate fetching
//! - Cross-DEX spread detection for mSOL, JitoSOL, bSOL, stSOL
//!
//! ## Usage
//!
//! ```rust,ignore
//! use solana_mev_bot::dex::lst::{LstArbitrageScanner, LstConfig};
//!
//! let config = LstConfig::from_env();
//! if config.enabled {
//!     let mut scanner = LstArbitrageScanner::new(rpc.clone(), config);
//!     let opps = scanner.scan()?;
//!     for opp in opps {
//!         info!("[LST] {} — spread={:.3}%", opp.description, opp.spread_pct);
//!     }
//! }
//! ```

pub mod jito_lst;
pub mod marinade;

use anyhow::Result;
use jito_lst::JitoStakePoolState;
use marinade::{detect_msol_spread, MarinadeState, LstArbitrageOpportunity};
use solana_client::rpc_client::RpcClient;
use std::sync::Arc;
use tracing::{info, warn};

// Re-export key types
pub use marinade::{BSOL_MINT, JITO_SOL_MINT, MSOL_MINT, STSOL_MINT};

// ── Configuration ─────────────────────────────────────────────────────────────

/// Configuration for LST arbitrage scanning.
#[derive(Debug, Clone)]
pub struct LstConfig {
    /// Enable/disable LST arb scanning entirely.
    pub enabled: bool,
    /// Minimum spread percentage to report an opportunity (default: 0.2%).
    pub min_spread_pct: f64,
    /// Fetch the Marinade state account on each scan cycle.
    pub scan_marinade: bool,
    /// Fetch the JitoSOL stake pool state on each scan cycle.
    pub scan_jito: bool,
}

impl Default for LstConfig {
    fn default() -> Self {
        Self {
            enabled: false, // disabled by default — enable via LST_ARB_ENABLED=true
            min_spread_pct: 0.2,
            scan_marinade: true,
            scan_jito: true,
        }
    }
}

impl LstConfig {
    /// Load from environment variables.
    pub fn from_env() -> Self {
        let enabled = std::env::var("LST_ARB_ENABLED")
            .ok()
            .and_then(|v| v.parse::<bool>().ok())
            .unwrap_or(false);

        let min_spread_pct = std::env::var("LST_MIN_SPREAD_PCT")
            .ok()
            .and_then(|v| v.parse::<f64>().ok())
            .unwrap_or(0.2);

        let scan_marinade = std::env::var("LST_SCAN_MARINADE")
            .ok()
            .and_then(|v| v.parse::<bool>().ok())
            .unwrap_or(true);

        let scan_jito = std::env::var("LST_SCAN_JITO")
            .ok()
            .and_then(|v| v.parse::<bool>().ok())
            .unwrap_or(true);

        Self {
            enabled,
            min_spread_pct,
            scan_marinade,
            scan_jito,
        }
    }
}

// ── Scanner ───────────────────────────────────────────────────────────────────

/// LST arbitrage scanner.
///
/// On each `scan()` call:
///  1. Fetches Marinade state (if enabled) to get the protocol SOL/mSOL rate.
///  2. Fetches Jito stake pool state (if enabled) to get the protocol SOL/JitoSOL rate.
///  3. Accepts external pool rates (from the calling DEX scanners) and computes
///     spreads between DEX prices and protocol redemption values.
///
/// The scanner does NOT submit any transactions — it only identifies opportunities.
/// The main loop is responsible for routing identified opps to the transaction builder.
pub struct LstArbitrageScanner {
    rpc: Arc<RpcClient>,
    config: LstConfig,
    /// Cached Marinade state to avoid an RPC call every loop iteration.
    marinade_state: Option<MarinadeState>,
    /// Cached Jito state.
    jito_state: Option<JitoStakePoolState>,
}

impl LstArbitrageScanner {
    /// Create a new scanner with the given RPC client and config.
    pub fn new(rpc: Arc<RpcClient>, config: LstConfig) -> Self {
        Self {
            rpc,
            config,
            marinade_state: None,
            jito_state: None,
        }
    }

    /// Refresh the on-chain state caches.  Call this periodically (e.g. every 10
    /// main-loop iterations) rather than on every scan to reduce RPC load.
    pub fn refresh_state(&mut self) {
        if self.config.scan_marinade {
            match MarinadeState::fetch(&self.rpc) {
                Ok(state) => {
                    info!(
                        "[LST] Marinade rate updated: {:.6} SOL/mSOL",
                        state.sol_per_msol
                    );
                    self.marinade_state = Some(state);
                }
                Err(e) => {
                    warn!("[LST] Failed to fetch Marinade state: {}", e);
                }
            }
        }

        if self.config.scan_jito {
            match JitoStakePoolState::fetch(&self.rpc) {
                Ok(state) => {
                    info!(
                        "[LST] JitoSOL rate updated: {:.6} SOL/JitoSOL",
                        state.sol_per_jito_sol
                    );
                    self.jito_state = Some(state);
                }
                Err(e) => {
                    warn!("[LST] Failed to fetch Jito stake pool state: {}", e);
                }
            }
        }
    }

    /// Scan for LST spread opportunities given a set of pool rates.
    ///
    /// `msol_pool_rates` — slice of `(dex_name, sol_per_msol)` from live DEX pools.
    /// `jito_pool_rates` — slice of `(dex_name, sol_per_jito)` from live DEX pools.
    ///
    /// Returns all detected opportunities above `config.min_spread_pct`.
    pub fn scan(
        &self,
        msol_pool_rates: &[(&str, f64)],
        jito_pool_rates: &[(&str, f64)],
    ) -> Vec<LstArbitrageOpportunity> {
        if !self.config.enabled {
            return Vec::new();
        }

        let mut opps = Vec::new();

        // mSOL: compare DEX rates against Marinade protocol rate
        if let Some(ref state) = self.marinade_state {
            let detected = detect_msol_spread(state, msol_pool_rates, self.config.min_spread_pct);
            opps.extend(detected);
        }

        // mSOL: cross-DEX (Raydium vs Orca, etc.)
        if msol_pool_rates.len() >= 2 {
            let cross = marinade::detect_lst_cross_dex_spread(
                msol_pool_rates,
                MSOL_MINT,
                self.config.min_spread_pct,
            );
            opps.extend(cross);
        }

        // JitoSOL: compare DEX rates against protocol rate
        if let Some(ref state) = self.jito_state {
            let protocol_rate = state.sol_per_jito_sol;
            for &(dex_name, dex_rate) in jito_pool_rates {
                if dex_rate <= 0.0 {
                    continue;
                }
                let spread_pct = (protocol_rate - dex_rate) / dex_rate * 100.0;
                if spread_pct >= self.config.min_spread_pct {
                    let profit_10 = 10.0 * spread_pct / 100.0;
                    info!(
                        "[LST] JitoSOL spread: protocol={:.6} DEX({})={:.6} spread={:.3}%",
                        protocol_rate, dex_name, dex_rate, spread_pct
                    );
                    opps.push(LstArbitrageOpportunity {
                        description: format!(
                            "Buy JitoSOL on {} at {:.6}, redeem at {:.6}",
                            dex_name, dex_rate, protocol_rate
                        ),
                        lst_mint: JITO_SOL_MINT.to_string(),
                        buy_dex: dex_name.to_string(),
                        buy_rate: dex_rate,
                        sell_dex: "Jito Stake Pool".to_string(),
                        sell_rate: protocol_rate,
                        spread_pct,
                        estimated_profit_sol_10: profit_10,
                    });
                }
            }

            // JitoSOL: cross-DEX
            if jito_pool_rates.len() >= 2 {
                let cross = marinade::detect_lst_cross_dex_spread(
                    jito_pool_rates,
                    JITO_SOL_MINT,
                    self.config.min_spread_pct,
                );
                opps.extend(cross);
            }
        }

        if !opps.is_empty() {
            info!("[LST] Found {} LST arbitrage opportunities", opps.len());
        }

        opps
    }

    /// Returns the current cached Marinade rate, if available.
    pub fn marinade_rate(&self) -> Option<f64> {
        self.marinade_state.as_ref().map(|s| s.sol_per_msol)
    }

    /// Returns the current cached JitoSOL rate, if available.
    pub fn jito_rate(&self) -> Option<f64> {
        self.jito_state.as_ref().map(|s| s.sol_per_jito_sol)
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_config_defaults_are_disabled() {
        let config = LstConfig::default();
        assert!(!config.enabled, "LST arb should be disabled by default");
        assert!(config.scan_marinade);
        assert!(config.scan_jito);
    }

    #[test]
    fn test_scan_returns_empty_when_disabled() {
        // Use a fake RpcClient url — will never be called since enabled=false
        let rpc = Arc::new(RpcClient::new("http://localhost:8899".to_string()));
        let config = LstConfig {
            enabled: false,
            ..LstConfig::default()
        };
        let scanner = LstArbitrageScanner::new(rpc, config);
        let opps = scanner.scan(&[("Raydium", 1.04)], &[("Orca", 1.03)]);
        assert!(opps.is_empty());
    }

    #[test]
    fn test_scan_with_state_injected() {
        let rpc = Arc::new(RpcClient::new("http://localhost:8899".to_string()));
        let config = LstConfig {
            enabled: true,
            min_spread_pct: 0.5,
            ..LstConfig::default()
        };

        let mut scanner = LstArbitrageScanner::new(rpc, config);
        // Inject a fake state directly (bypasses RPC call)
        scanner.marinade_state = Some(MarinadeState {
            total_sol_lamports: 105_000_000_000,
            msol_supply: 100_000_000_000,
            sol_per_msol: 1.05,
            slot_fetched: 0,
        });

        // DEX offering mSOL at 1.02 — spread = 2.9% > 0.5% threshold
        let opps = scanner.scan(&[("Raydium", 1.02)], &[]);
        assert!(!opps.is_empty());
        assert!(opps[0].spread_pct > 2.0);
    }
}
