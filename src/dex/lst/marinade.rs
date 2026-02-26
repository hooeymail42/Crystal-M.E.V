#![allow(dead_code)]
//! Marinade Finance mSOL state deserialization and exchange-rate calculation.
//!
//! Marinade maintains a single on-chain state account that stores the SOL/mSOL
//! exchange rate and other protocol parameters. This module reads that account
//! via the RPC client and exposes the current rate for arbitrage detection.
//!
//! ## Key addresses
//! - State: `8szGkuLTAux9XMgZ2vtY39jVSowEcpBfFfD8hXSEqdGC` (Marinade canonical state)
//! - mSOL mint: `mSoLzYCxHdYgdzU16g5QSh3i5K3z3KZK7ytfqcJm7So`
//!
//! ## State layout (simplified, Anchor-generated)
//! The fields we care about sit at fixed byte offsets after the 8-byte discriminator:
//!   - msol_supply (u64)  — offset  72
//!   - sol_leg_reserve (u64) — offset 80  (total staked + deposited SOL)
//!
//! Exchange rate:  1 mSOL = sol_leg_reserve / msol_supply  SOL

use anyhow::{anyhow, Result};
use solana_client::rpc_client::RpcClient;
use solana_sdk::pubkey::Pubkey;
use std::str::FromStr;
use tracing::{debug, info, warn};

// ── Constants ─────────────────────────────────────────────────────────────────

/// Marinade state account address (canonical mainnet).
pub const MARINADE_STATE: &str = "8szGkuLTAux9XMgZ2vtY39jVSowEcpBfFfD8hXSEqdGC";

/// mSOL mint address.
pub const MSOL_MINT: &str = "mSoLzYCxHdYgdzU16g5QSh3i5K3z3KZK7ytfqcJm7So";

/// JitoSOL mint address.
pub const JITO_SOL_MINT: &str = "J1toso1uCk3RLmjorhTtrVwY9HJ7X8V9yYac6Y7kGCPn";

/// bSOL (BlazeStake) mint address.
pub const BSOL_MINT: &str = "bSo13r4TkiE4KumL71LsHTPpL2euBYLFx6h9HP3piy1";

/// stSOL (Lido) mint address.
pub const STSOL_MINT: &str = "7dHbWXmci3dT8UFYWYZweBLXgycu7Y3iL6trKn1Y7ARj";

/// Marinade: staked SOL total at offset 352 in the state account (u64).
/// This together with msol_supply gives the exchange rate.
const MARINADE_SOL_OFFSET: usize = 352;

/// Marinade: mSOL supply at offset 344 (u64).
const MARINADE_MSOL_SUPPLY_OFFSET: usize = 344;

// ── Data structures ────────────────────────────────────────────────────────────

/// Lightweight snapshot of the Marinade protocol state.
#[derive(Debug, Clone)]
pub struct MarinadeState {
    /// Total SOL managed by Marinade (staked + buffered).
    pub total_sol_lamports: u64,
    /// Total mSOL in circulation.
    pub msol_supply: u64,
    /// Exchange rate: SOL per mSOL.  E.g. 1.05 means 1 mSOL redeems for 1.05 SOL.
    pub sol_per_msol: f64,
    /// Epoch when this was last refreshed on-chain (informational only).
    pub slot_fetched: u64,
}

impl MarinadeState {
    /// Fetch the Marinade state account from the RPC and deserialize.
    pub fn fetch(rpc: &RpcClient) -> Result<Self> {
        let state_key = Pubkey::from_str(MARINADE_STATE)
            .map_err(|e| anyhow!("Invalid Marinade state address: {}", e))?;

        let account = rpc
            .get_account(&state_key)
            .map_err(|e| anyhow!("Failed to fetch Marinade state: {}", e))?;

        let data = &account.data;

        // We need at least MARINADE_SOL_OFFSET + 8 bytes
        let required = MARINADE_SOL_OFFSET + 8;
        if data.len() < required {
            return Err(anyhow!(
                "Marinade state account too small: {} < {}",
                data.len(),
                required
            ));
        }

        let msol_supply = u64::from_le_bytes(
            data[MARINADE_MSOL_SUPPLY_OFFSET..MARINADE_MSOL_SUPPLY_OFFSET + 8]
                .try_into()
                .map_err(|_| anyhow!("Failed to read msol_supply bytes"))?,
        );
        let total_sol = u64::from_le_bytes(
            data[MARINADE_SOL_OFFSET..MARINADE_SOL_OFFSET + 8]
                .try_into()
                .map_err(|_| anyhow!("Failed to read total_sol bytes"))?,
        );

        if msol_supply == 0 {
            return Err(anyhow!("Marinade mSOL supply is zero"));
        }

        let sol_per_msol = total_sol as f64 / msol_supply as f64;

        debug!(
            "[Marinade] total_sol={} lamports ({:.4} SOL), msol_supply={} ({:.4} mSOL), rate={:.6}",
            total_sol,
            total_sol as f64 / 1e9,
            msol_supply,
            msol_supply as f64 / 1e9,
            sol_per_msol,
        );

        Ok(MarinadeState {
            total_sol_lamports: total_sol,
            msol_supply,
            sol_per_msol,
            slot_fetched: 0,
        })
    }

    /// The implied staking APY based on the exchange rate (rough estimate).
    /// This compares against the expected initial rate of 1.0.
    pub fn implied_discount_from_par(&self) -> f64 {
        // A rate > 1.0 means mSOL has accrued staking rewards.
        // "Discount from par" in trading terms = (market_rate - protocol_rate) / protocol_rate
        self.sol_per_msol - 1.0
    }
}

// ── LST Arbitrage Opportunity ─────────────────────────────────────────────────

/// A detected LST spread opportunity.
#[derive(Debug, Clone)]
pub struct LstArbitrageOpportunity {
    /// Human-readable description of the arb.
    pub description: String,
    /// LST token being arbitraged (mSOL, JitoSOL, bSOL, stSOL).
    pub lst_mint: String,
    /// DEX to buy the LST on.
    pub buy_dex: String,
    /// Rate on the buy side: SOL per LST on the buying DEX.
    pub buy_rate: f64,
    /// DEX to sell the LST on (or protocol rate).
    pub sell_dex: String,
    /// Rate on the sell side: SOL per LST on the selling DEX / protocol.
    pub sell_rate: f64,
    /// Spread percentage ((sell_rate - buy_rate) / buy_rate * 100).
    pub spread_pct: f64,
    /// Estimated profit in SOL for a 10 SOL position.
    pub estimated_profit_sol_10: f64,
}

// ── Cross-DEX LST Spread Detector ─────────────────────────────────────────────

/// Check for cross-DEX mSOL spread opportunities.
///
/// Compares on-chain protocol exchange rates with reported DEX pool rates.
/// A positive spread means the DEX is pricing the LST cheaper than its
/// redemption value — a classic arbitrage setup.
///
/// `pool_rates` is a slice of `(dex_name, sol_per_lst_rate)` tuples gathered
/// by the caller from live pool quotes.
pub fn detect_msol_spread(
    marinade: &MarinadeState,
    pool_rates: &[(&str, f64)],
    min_spread_pct: f64,
) -> Vec<LstArbitrageOpportunity> {
    let protocol_rate = marinade.sol_per_msol;
    let mut opps = Vec::new();

    for &(dex_name, dex_rate) in pool_rates {
        if dex_rate <= 0.0 {
            continue;
        }

        // If protocol_rate > dex_rate: we can buy mSOL cheap on DEX and
        // redeem at the higher on-chain rate (delayed unstake / Jupiter route).
        let spread_pct = (protocol_rate - dex_rate) / dex_rate * 100.0;

        if spread_pct >= min_spread_pct {
            let profit_10 = 10.0 * spread_pct / 100.0;
            info!(
                "[LST] mSOL cross-DEX spread: protocol={:.6} DEX({})={:.6} spread={:.3}% est_profit_10sol={:.4}",
                protocol_rate, dex_name, dex_rate, spread_pct, profit_10
            );
            opps.push(LstArbitrageOpportunity {
                description: format!(
                    "Buy mSOL on {} at {:.6} SOL/mSOL, protocol redeems at {:.6}",
                    dex_name, dex_rate, protocol_rate
                ),
                lst_mint: MSOL_MINT.to_string(),
                buy_dex: dex_name.to_string(),
                buy_rate: dex_rate,
                sell_dex: "Marinade Protocol".to_string(),
                sell_rate: protocol_rate,
                spread_pct,
                estimated_profit_sol_10: profit_10,
            });
        }
    }

    opps
}

/// Check for cross-DEX spread between two pool rates for the same LST.
/// Useful when comparing Raydium vs Orca pool rates.
pub fn detect_lst_cross_dex_spread(
    pool_rates: &[(&str, f64)],
    lst_mint: &str,
    min_spread_pct: f64,
) -> Vec<LstArbitrageOpportunity> {
    let mut opps = Vec::new();

    for i in 0..pool_rates.len() {
        for j in 0..pool_rates.len() {
            if i == j {
                continue;
            }
            let (buy_dex, buy_rate) = pool_rates[i];
            let (sell_dex, sell_rate) = pool_rates[j];

            if buy_rate <= 0.0 || sell_rate <= 0.0 {
                continue;
            }

            let spread_pct = (sell_rate - buy_rate) / buy_rate * 100.0;
            if spread_pct >= min_spread_pct {
                let profit_10 = 10.0 * spread_pct / 100.0;
                info!(
                    "[LST] Cross-DEX spread: buy {} at {:.6}, sell {} at {:.6}, spread={:.3}%",
                    buy_dex, buy_rate, sell_dex, sell_rate, spread_pct
                );
                opps.push(LstArbitrageOpportunity {
                    description: format!(
                        "Buy LST on {} at {:.6}, sell on {} at {:.6}",
                        buy_dex, buy_rate, sell_dex, sell_rate
                    ),
                    lst_mint: lst_mint.to_string(),
                    buy_dex: buy_dex.to_string(),
                    buy_rate,
                    sell_dex: sell_dex.to_string(),
                    sell_rate,
                    spread_pct,
                    estimated_profit_sol_10: profit_10,
                });
            }
        }
    }

    opps
}

// ── LST Pool Addresses for Manual Configuration ───────────────────────────────

/// Well-known LST pool addresses for `.env.example` reference.
/// These are mainnet addresses; add them to the corresponding MINT_N_* lists.
pub const LST_POOL_REFERENCE: &[(&str, &str, &str)] = &[
    ("mSOL/SOL Raydium V4",    MSOL_MINT, "EGZ7tiLeH62TPV1gL8WwbXGzEPa9zmcpVnnkPKKnrE2U"),
    ("mSOL/SOL Orca Whirlpool", MSOL_MINT, "7qbRF6YsyGuLUVs6Y1q64bdVrfe4ZcV3bMkW6JhFJSfn"),
    ("JitoSOL/SOL Raydium",    JITO_SOL_MINT, "4fuUiYxTQ6QCrdSq9ouBYcTM7bqSwYTSyLueGZLTy4T4"),
    ("bSOL/SOL Raydium",       BSOL_MINT, "ARwi1S4DaiTG5DX7S4M4ZkL2tiPGmDoQtHpQHGH6KFWe"),
];

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_marinade_spread_detection_above_threshold() {
        let state = MarinadeState {
            total_sol_lamports: 105_000_000_000, // 105 SOL
            msol_supply: 100_000_000_000,        // 100 mSOL
            sol_per_msol: 1.05,
            slot_fetched: 0,
        };

        // DEX offering mSOL at only 1.02 SOL/mSOL — protocol rate is 1.05
        let pool_rates = vec![("Raydium", 1.02f64), ("Orca", 1.04f64)];
        let opps = detect_msol_spread(&state, &pool_rates, 0.2);

        // Both should show positive spread (1.05 - 1.02 = 2.8%, 1.05 - 1.04 = 0.96%)
        // Only Raydium exceeds 0.2% minimum
        assert!(!opps.is_empty());
        assert_eq!(opps[0].buy_dex, "Raydium");
        assert!(opps[0].spread_pct > 2.0);
    }

    #[test]
    fn test_cross_dex_spread_detection() {
        let pool_rates = vec![("Raydium", 1.020f64), ("Orca", 1.035f64)];
        let opps = detect_lst_cross_dex_spread(&pool_rates, MSOL_MINT, 0.5);

        // Buy on Raydium (1.020), sell on Orca (1.035): spread = ~1.47%
        assert!(!opps.is_empty());
        let opp = opps.iter().find(|o| o.buy_dex == "Raydium").unwrap();
        assert!(opp.spread_pct > 1.0);
    }

    #[test]
    fn test_no_spread_below_threshold() {
        let state = MarinadeState {
            total_sol_lamports: 100_500_000_000,
            msol_supply: 100_000_000_000,
            sol_per_msol: 1.005,
            slot_fetched: 0,
        };
        // DEX rate very close to protocol rate
        let pool_rates = vec![("Raydium", 1.004f64)];
        let opps = detect_msol_spread(&state, &pool_rates, 0.2);
        // Spread is only 0.099% — below threshold
        assert!(opps.is_empty());
    }

    #[test]
    fn test_lst_mint_constants_are_valid_base58() {
        // Each constant should be parseable as a Pubkey
        assert!(Pubkey::from_str(MSOL_MINT).is_ok());
        assert!(Pubkey::from_str(JITO_SOL_MINT).is_ok());
        assert!(Pubkey::from_str(BSOL_MINT).is_ok());
        assert!(Pubkey::from_str(STSOL_MINT).is_ok());
        assert!(Pubkey::from_str(MARINADE_STATE).is_ok());
    }
}
