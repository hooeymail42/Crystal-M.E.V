//! Collateral → liability swap routing through the existing DEX layer.
//!
//! After a liquidation seizes collateral, the liquidator must swap it back into
//! the flash-borrowed liability asset to repay the loan. This router does not
//! implement any swap math or instruction encoding itself — it picks pools from
//! the bot's already-loaded pool set, expresses the swap as [`PathStep`]s, and
//! hands them to `TransactionBuilder::build_instructions_from_opportunity`, the
//! same per-DEX builder path the arbitrage engine uses.
//!
//! # Direction convention
//!
//! The per-DEX builders key swap direction off `PathStep::action` relative to
//! the pool's `base_mint` (usually WSOL):
//! - `"buy"`  = base → token (source is the base mint)
//! - `"sell"` = token → base (source is the token mint)
//!
//! # Routes
//!
//! - **Direct**: one pool whose two mints are exactly {collateral, liability}.
//! - **Two-hop via SOL**: collateral → SOL on a collateral/SOL pool, then
//!   SOL → liability on a liability/SOL pool. The intermediate SOL flows through
//!   the shared WSOL ATA.
//!
//! Two-hop legs are exact-in: the second leg's `amount_in` is fixed at build time
//! from the first leg's *expected* output. If the first leg under-delivers, the
//! second leg fails and the whole transaction (including the flash loan) reverts
//! — you lose fees, not principal. Size conservatively.

use anyhow::{anyhow, Result};
use solana_sdk::instruction::Instruction;
use solana_sdk::pubkey::Pubkey;
use std::str::FromStr;

use crate::chain::constants::SOL_MINT;
use crate::chain::opportunity_detector::{ArbitrageOpportunity, PathStep};
use crate::chain::pools::{MintPoolData, PoolData};
use crate::chain::refresh::PoolRefreshManager;
use crate::chain::transaction::TransactionBuilder;

/// DEX names that `TransactionBuilder::build_instructions_from_opportunity` can
/// actually build. Pools on any other DEX are ignored by the router, since the
/// builder would silently skip them (`Unsupported DEX for IX building`).
pub const SUPPORTED_DEXES: &[&str] = &[
    "Raydium",
    "Pump",
    "DLMM",
    "MeteoraDAmmV2",
    "Whirlpool",
    "RaydiumClmm",
    "Phoenix",
    "Lifinity",
    "Heaven",
];

/// A pool the router may use, reduced to what routing needs.
#[derive(Debug, Clone, PartialEq)]
pub struct PoolCandidate {
    pub dex: String,
    pub pool_address: Pubkey,
    pub token_mint: Pubkey,
    pub base_mint: Pubkey,
}

impl PoolCandidate {
    /// True if this pool trades exactly the unordered pair {a, b}.
    pub fn trades(&self, a: &Pubkey, b: &Pubkey) -> bool {
        (self.token_mint == *a && self.base_mint == *b)
            || (self.token_mint == *b && self.base_mint == *a)
    }

    /// The builder `action` for swapping `from` → `to` on this pool, or `None`
    /// if the pool doesn't trade that pair.
    pub fn action_for(&self, from: &Pubkey, to: &Pubkey) -> Option<&'static str> {
        if self.base_mint == *from && self.token_mint == *to {
            Some("buy")
        } else if self.token_mint == *from && self.base_mint == *to {
            Some("sell")
        } else {
            None
        }
    }
}

/// One leg of a route: swap `from_mint` → `to_mint` on `pool`.
#[derive(Debug, Clone, PartialEq)]
pub struct RouteLeg {
    pub pool: PoolCandidate,
    pub from_mint: Pubkey,
    pub to_mint: Pubkey,
}

/// Amounts for one leg, in native (smallest-unit) token amounts.
#[derive(Debug, Clone, Copy)]
pub struct LegAmounts {
    pub amount_in: u64,
    /// Expected output before slippage.
    pub expected_out: u64,
    /// Per-leg slippage tolerance in bps (applied by the builder to derive
    /// `min_amount_out`).
    pub slippage_bps: u16,
}

/// Picks pools and builds swap instructions for collateral → liability.
#[derive(Debug, Clone, Default)]
pub struct SwapRouter {
    candidates: Vec<PoolCandidate>,
}

impl SwapRouter {
    pub fn new(candidates: Vec<PoolCandidate>) -> Self {
        let candidates = candidates
            .into_iter()
            .filter(|c| SUPPORTED_DEXES.contains(&c.dex.as_str()))
            .collect();
        Self { candidates }
    }

    /// Build a router from the bot's loaded pools. Unsupported DEXes are
    /// dropped.
    pub fn from_mint_pool_datas(mpds: &[MintPoolData]) -> Self {
        let candidates = mpds
            .iter()
            .flat_map(|mpd| mpd.pools.iter())
            .map(|p| PoolCandidate {
                dex: p.get_dex_name().to_string(),
                pool_address: *p.pool_address(),
                token_mint: *p.token_mint(),
                base_mint: *p.base_mint(),
            })
            .collect();
        Self::new(candidates)
    }

    pub fn candidate_count(&self) -> usize {
        self.candidates.len()
    }

    fn find_pool(&self, a: &Pubkey, b: &Pubkey) -> Option<&PoolCandidate> {
        self.candidates.iter().find(|c| c.trades(a, b))
    }

    /// Find a route from `collateral` to `liability`: direct if a pool trades
    /// the pair, otherwise two hops through SOL. Returns `None` if neither
    /// exists, or an empty route if the mints are equal (no swap needed).
    pub fn find_route(&self, collateral: &Pubkey, liability: &Pubkey) -> Option<Vec<RouteLeg>> {
        if collateral == liability {
            return Some(vec![]);
        }
        if let Some(pool) = self.find_pool(collateral, liability) {
            return Some(vec![RouteLeg {
                pool: pool.clone(),
                from_mint: *collateral,
                to_mint: *liability,
            }]);
        }
        let sol = Pubkey::from_str(SOL_MINT).ok()?;
        let first = self.find_pool(collateral, &sol)?;
        let second = self.find_pool(&sol, liability)?;
        Some(vec![
            RouteLeg {
                pool: first.clone(),
                from_mint: *collateral,
                to_mint: sol,
            },
            RouteLeg {
                pool: second.clone(),
                from_mint: sol,
                to_mint: *liability,
            },
        ])
    }

    /// Express a route as the `PathStep`s the DEX builders expect.
    pub fn build_path_steps(route: &[RouteLeg], amounts: &[LegAmounts]) -> Result<Vec<PathStep>> {
        if route.len() != amounts.len() {
            return Err(anyhow!(
                "route has {} legs but {} amount entries",
                route.len(),
                amounts.len()
            ));
        }
        route
            .iter()
            .zip(amounts)
            .map(|(leg, amt)| {
                let action = leg
                    .pool
                    .action_for(&leg.from_mint, &leg.to_mint)
                    .ok_or_else(|| {
                        anyhow!(
                            "pool {} does not trade {} → {}",
                            leg.pool.pool_address,
                            leg.from_mint,
                            leg.to_mint
                        )
                    })?;
                let price = if amt.amount_in > 0 {
                    amt.expected_out as f64 / amt.amount_in as f64
                } else {
                    0.0
                };
                Ok(PathStep {
                    dex: leg.pool.dex.clone(),
                    pool_address: leg.pool.pool_address.to_string(),
                    action: action.to_string(),
                    price,
                    token_in: leg.from_mint.to_string(),
                    token_out: leg.to_mint.to_string(),
                    amount_in: amt.amount_in as f64,
                    amount_out: amt.expected_out as f64,
                    slippage_bps: amt.slippage_bps,
                })
            })
            .collect()
    }

    /// Build the swap instructions for `route` via the arbitrage engine's
    /// per-DEX builders. `refresh_manager` must hold deserialized state for
    /// every pool in the route (it's the same manager the main loop refreshes).
    pub fn build_swap_instructions(
        tx_builder: &TransactionBuilder,
        refresh_manager: &PoolRefreshManager,
        route: &[RouteLeg],
        amounts: &[LegAmounts],
    ) -> Result<Vec<Instruction>> {
        let steps = Self::build_path_steps(route, amounts)?;
        if steps.is_empty() {
            return Ok(vec![]);
        }
        let token_mint = steps[0].token_in.clone();
        let pool_addresses = steps.iter().map(|s| s.pool_address.clone()).collect();
        // Only `path` is read by the builder; the rest is bookkeeping.
        let opportunity = ArbitrageOpportunity {
            token_mint,
            path: steps,
            gross_profit_sol: 0.0,
            profit_percent: 0.0,
            input_amount_sol: 0.0,
            expected_output_sol: 0.0,
            pool_addresses,
            risk_score: 0,
            confidence_score: 0,
            estimated_execution_ms: 0,
        };
        let ixs = tx_builder.build_instructions_from_opportunity(
            &opportunity,
            refresh_manager,
            &std::collections::HashMap::new(),
            0, // per-leg slippage_bps is always set on each step
        )?;
        // The builder skips legs it can't build rather than failing; a partial
        // route would leave the flash loan unrepaid, so treat it as an error.
        if ixs.len() != route.len() {
            return Err(anyhow!(
                "built {} of {} swap legs; refusing partial route",
                ixs.len(),
                route.len()
            ));
        }
        Ok(ixs)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sol() -> Pubkey {
        Pubkey::from_str(SOL_MINT).unwrap()
    }

    fn cand(dex: &str, token: Pubkey, base: Pubkey) -> PoolCandidate {
        PoolCandidate {
            dex: dex.to_string(),
            pool_address: Pubkey::new_unique(),
            token_mint: token,
            base_mint: base,
        }
    }

    #[test]
    fn action_follows_base_mint_convention() {
        let token = Pubkey::new_unique();
        let c = cand("Raydium", token, sol());
        assert_eq!(c.action_for(&sol(), &token), Some("buy"));
        assert_eq!(c.action_for(&token, &sol()), Some("sell"));
        assert_eq!(c.action_for(&token, &Pubkey::new_unique()), None);
    }

    #[test]
    fn unsupported_dexes_are_filtered() {
        let token = Pubkey::new_unique();
        let router = SwapRouter::new(vec![
            cand("RaydiumCp", token, sol()),
            cand("Solfi", token, sol()),
            cand("Whirlpool", token, sol()),
        ]);
        assert_eq!(router.candidate_count(), 1);
    }

    #[test]
    fn direct_route_preferred() {
        let collateral = Pubkey::new_unique();
        let router = SwapRouter::new(vec![cand("Raydium", collateral, sol())]);
        // Collateral token → SOL liability: one "sell" leg.
        let route = router.find_route(&collateral, &sol()).unwrap();
        assert_eq!(route.len(), 1);
        assert_eq!(route[0].pool.action_for(&collateral, &sol()), Some("sell"));
    }

    #[test]
    fn two_hop_via_sol_when_no_direct_pool() {
        let collateral = Pubkey::new_unique();
        let liability = Pubkey::new_unique();
        let router = SwapRouter::new(vec![
            cand("Raydium", collateral, sol()),
            cand("Whirlpool", liability, sol()),
        ]);
        let route = router.find_route(&collateral, &liability).unwrap();
        assert_eq!(route.len(), 2);
        assert_eq!(route[0].from_mint, collateral);
        assert_eq!(route[0].to_mint, sol());
        assert_eq!(route[1].from_mint, sol());
        assert_eq!(route[1].to_mint, liability);
        // First leg sells collateral for SOL, second buys liability with SOL.
        assert_eq!(route[0].pool.action_for(&route[0].from_mint, &route[0].to_mint), Some("sell"));
        assert_eq!(route[1].pool.action_for(&route[1].from_mint, &route[1].to_mint), Some("buy"));
    }

    #[test]
    fn no_route_when_pools_missing() {
        let collateral = Pubkey::new_unique();
        let liability = Pubkey::new_unique();
        let router = SwapRouter::new(vec![cand("Raydium", collateral, sol())]);
        assert!(router.find_route(&collateral, &liability).is_none());
    }

    #[test]
    fn same_mint_needs_no_swap() {
        let m = Pubkey::new_unique();
        let router = SwapRouter::default();
        assert_eq!(router.find_route(&m, &m).unwrap().len(), 0);
    }

    #[test]
    fn path_steps_carry_amounts_and_direction() {
        let collateral = Pubkey::new_unique();
        let router = SwapRouter::new(vec![cand("Raydium", collateral, sol())]);
        let route = router.find_route(&collateral, &sol()).unwrap();
        let steps = SwapRouter::build_path_steps(
            &route,
            &[LegAmounts {
                amount_in: 1_000_000,
                expected_out: 2_000_000,
                slippage_bps: 50,
            }],
        )
        .unwrap();
        assert_eq!(steps.len(), 1);
        let s = &steps[0];
        assert_eq!(s.dex, "Raydium");
        assert_eq!(s.action, "sell");
        assert_eq!(s.amount_in, 1_000_000.0);
        assert_eq!(s.amount_out, 2_000_000.0);
        assert_eq!(s.slippage_bps, 50);
        assert_eq!(s.token_in, collateral.to_string());
        assert_eq!(s.token_out, sol().to_string());
    }

    #[test]
    fn mismatched_amounts_rejected() {
        let collateral = Pubkey::new_unique();
        let router = SwapRouter::new(vec![cand("Raydium", collateral, sol())]);
        let route = router.find_route(&collateral, &sol()).unwrap();
        assert!(SwapRouter::build_path_steps(&route, &[]).is_err());
    }

    #[test]
    fn build_ixs_fails_without_pool_state() {
        // No deserialized state in a fresh refresh manager → the builder errors
        // rather than emitting an instruction with guessed accounts.
        let rpc = std::sync::Arc::new(solana_client::rpc_client::RpcClient::new(
            "http://127.0.0.1:1".to_string(),
        ));
        let tb = TransactionBuilder::new(
            rpc.clone(),
            std::sync::Arc::new(solana_sdk::signature::Keypair::new()),
            400_000,
            10_000,
            vec![],
            false,
        );
        let rm = PoolRefreshManager::new(rpc);
        let collateral = Pubkey::new_unique();
        let router = SwapRouter::new(vec![cand("Raydium", collateral, sol())]);
        let route = router.find_route(&collateral, &sol()).unwrap();
        let res = SwapRouter::build_swap_instructions(
            &tb,
            &rm,
            &route,
            &[LegAmounts {
                amount_in: 1,
                expected_out: 1,
                slippage_bps: 50,
            }],
        );
        assert!(res.is_err());
    }
}
