use crate::chain::{
    pools::{MintPoolData, Pool, PoolData},
    refresh::{DeserializedPoolState, PoolRefreshManager},
    trading_graph::TradingGraph,
};
use tracing::{info, warn};
use solana_client::rpc_client::RpcClient;
use solana_sdk::pubkey::Pubkey;
use std::cmp::Ordering;
use serde::{Deserialize, Serialize};

/// Represents a single arbitrage opportunity
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArbitrageOpportunity {
    pub token_mint: String,
    pub path: Vec<PathStep>,
    pub gross_profit_sol: f64,
    pub profit_percent: f64,
    pub input_amount_sol: f64,
    pub expected_output_sol: f64,
    pub pool_addresses: Vec<String>,
    pub risk_score: u8,
    pub confidence_score: u8,
    pub estimated_execution_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PathStep {
    pub dex: String,
    pub pool_address: String,
    pub action: String,
    pub price: f64,
    pub token_in: String,
    pub token_out: String,
    pub amount_in: f64,
    pub amount_out: f64,
    /// Per-leg slippage tolerance in basis points. 0 means "fall back to global slippage_bps".
    #[serde(default)]
    pub slippage_bps: u16,
}

#[derive(Debug, Clone)]
pub struct OpportunityConfig {
    pub min_profit_percent: f64,
    pub min_liquidity_sol: f64,
    pub max_slippage_percent: f64,
    #[allow(dead_code)]
    pub max_volatility_percent: f64,
}

impl Default for OpportunityConfig {
    fn default() -> Self {
        Self {
            // 0.1% gross profit threshold — deliberately below fee cost (0.25-0.55% round-trip)
            // so that nearly-break-even opportunities are logged and we can observe real spreads.
            // The real execution gate is MIN_PROFIT_SOL in config + gas_fee_config.should_execute().
            // Setting this to 0.3% was silently filtering out all real arbs since typical
            // Raydium/DLMM spread is 0.1-0.4% before fees.
            min_profit_percent: -1.0, // TEMP: lowered for DLMM fix testing
            min_liquidity_sol: 1.0,
            max_slippage_percent: 1.0,
            max_volatility_percent: 10.0,
        }
    }
}

pub struct OpportunityDetector<'a> {
    config: OpportunityConfig,
    #[allow(dead_code)]
    rpc_client: &'a RpcClient,
}

impl<'a> OpportunityDetector<'a> {
    pub fn new(config: OpportunityConfig, rpc_client: &'a RpcClient) -> Self {
        Self { config, rpc_client }
    }

    /// Main entry point: find profitable arbitrage opportunities using cached pool state.
    pub fn find_opportunities(
        &mut self,
        pool_data: &MintPoolData,
        refresh_manager: &PoolRefreshManager,
    ) -> Vec<ArbitrageOpportunity> {
        let mut opportunities = Vec::new();

        opportunities.extend(self.find_two_dex_opportunities(pool_data, refresh_manager));
        opportunities.extend(self.find_multi_hop_opportunities(pool_data, refresh_manager));

        // Sort by absolute profit descending
        opportunities.sort_by(|a, b| {
            b.gross_profit_sol
                .partial_cmp(&a.gross_profit_sol)
                .unwrap_or(Ordering::Equal)
        });

        opportunities.retain(|opp| opp.profit_percent > self.config.min_profit_percent);
        opportunities
    }

    /// Get cached reserves for a pool.
    fn get_cached_reserves(
        &self,
        pool: &Pool,
        refresh_manager: &PoolRefreshManager,
    ) -> Option<(u64, u64)> {
        let reserves = refresh_manager.get_reserves(pool.pool_address())?;
        if reserves.token_reserve == 0 || reserves.sol_reserve == 0 {
            return None;
        }
        Some((reserves.token_reserve, reserves.sol_reserve))
    }

    /// Calculate buy output (SOL -> Token) using DEX-specific math from deserialized state,
    /// falling back to constant-product with fee.
    fn calculate_buy_quote(
        &self,
        pool: &Pool,
        sol_amount_in: u64,
        refresh_manager: &PoolRefreshManager,
    ) -> Option<u64> {
        let state = refresh_manager.get_pool_state(pool.pool_address());

        match state {
            Some(DeserializedPoolState::Pump {
                virtual_sol_reserves, virtual_token_reserves, complete, ..
            }) => {
                if *complete || *virtual_sol_reserves == 0 || *virtual_token_reserves == 0 {
                    return None;
                }
                let fee = sol_amount_in / 100;
                let sol_after_fee = sol_amount_in.saturating_sub(fee);
                let new_sol = *virtual_sol_reserves as u128 + sol_after_fee as u128;
                let new_token = (*virtual_sol_reserves as u128)
                    * (*virtual_token_reserves as u128)
                    / new_sol;
                let out = (*virtual_token_reserves as u128).saturating_sub(new_token);
                Some(out as u64)
            }
            Some(DeserializedPoolState::Heaven {
                virtual_sol_reserves, virtual_token_reserves, complete, ..
            }) => {
                if *complete || *virtual_sol_reserves == 0 || *virtual_token_reserves == 0 {
                    return None;
                }
                let fee = sol_amount_in / 100;
                let sol_after_fee = sol_amount_in.saturating_sub(fee);
                let new_sol = *virtual_sol_reserves as u128 + sol_after_fee as u128;
                let new_token = (*virtual_sol_reserves as u128)
                    * (*virtual_token_reserves as u128)
                    / new_sol;
                let out = (*virtual_token_reserves as u128).saturating_sub(new_token);
                Some(out as u64)
            }
            Some(DeserializedPoolState::RaydiumClmm { fee_rate, .. }) => {
                // Use constant-product approximation with vault balances + exact CLMM fee.
                // The theoretical CLMM tick-based math requires accurate per-tick liquidity
                // which we don't reconstruct from on-chain data. Constant-product with real
                // vault balances is a reliable approximation within a tick range.
                let (token_reserve, sol_reserve) =
                    self.get_cached_reserves(pool, refresh_manager)?;
                // fee_rate is per-million; convert to bps (fee_rate/100)
                let fee_bps = (*fee_rate / 100).min(9999) as u64;
                let sol_after_fee =
                    sol_amount_in as u128 * (10000 - fee_bps) as u128 / 10000;
                let new_sol = sol_reserve as u128 + sol_after_fee;
                if new_sol == 0 { return None; }
                let new_token = (sol_reserve as u128) * (token_reserve as u128) / new_sol;
                Some((token_reserve as u128).saturating_sub(new_token) as u64)
            }
            Some(DeserializedPoolState::WhirlpoolState { fee_rate, .. }) => {
                // Same constant-product approximation for Whirlpool CLMM.
                let (token_reserve, sol_reserve) =
                    self.get_cached_reserves(pool, refresh_manager)?;
                let fee_bps = (*fee_rate / 100).min(9999) as u64;
                let sol_after_fee =
                    sol_amount_in as u128 * (10000 - fee_bps) as u128 / 10000;
                let new_sol = sol_reserve as u128 + sol_after_fee;
                if new_sol == 0 { return None; }
                let new_token = (sol_reserve as u128) * (token_reserve as u128) / new_sol;
                Some((token_reserve as u128).saturating_sub(new_token) as u64)
            }
            Some(DeserializedPoolState::MeteoraDlmm { active_id, bin_step, base_factor, token_x_mint, .. }) => {
                // Direct active-bin price formula — replaces the uniform-20-bin approximation
                // which overstates per-bin liquidity and causes simulation failures.
                // Price = (1 + bin_step/10000)^active_id (tokens per SOL if X=SOL, else SOL per token).
                let (token_reserve, sol_reserve) = self.get_cached_reserves(pool, refresh_manager)?;
                let sol_mint = Pubkey::try_from("So11111111111111111111111111111111111111112").unwrap();
                let x_is_sol = *token_x_mint == sol_mint;

                let price = (1.0_f64 + *bin_step as f64 / 10_000.0).powi(*active_id);
                if price <= 0.0 { return None; }

                // Sanity: reject if active_id price deviates >10× from reserve ratio.
                // Catches v2-format DLMM pools whose different layout produces garbage active_id.
                let reserve_price = if x_is_sol {
                    token_reserve as f64 / sol_reserve.max(1) as f64
                } else {
                    sol_reserve as f64 / token_reserve.max(1) as f64
                };
                let deviation = (price / reserve_price.max(1e-30)).max(reserve_price.max(1e-30) / price);
                if deviation > 10.0 { return None; }

                let fee_bps = (*bin_step as u64 * *base_factor as u64 / 10_000).max(1).min(1000);
                let sol_after_fee = sol_amount_in as u128 * (10_000 - fee_bps) as u128 / 10_000;

                // Buy = SOL in → token out
                // X=SOL: price = Y/X = token/SOL → token_out = sol_in * price
                // Y=SOL: price = Y/X = SOL/token → token_out = sol_in / price
                let token_out = if x_is_sol {
                    (sol_after_fee as f64 * price) as u64
                } else {
                    (sol_after_fee as f64 / price) as u64
                };
                if token_out == 0 { return None; }
                // Conservative depth cap: assume active bin holds ~0.5% of total reserves.
                let depth_cap = (token_reserve / 200).max(1);
                Some(token_out.min(depth_cap))
            }
            _ => {
                // Generic constant-product with fee
                let (token_reserve, sol_reserve) =
                    self.get_cached_reserves(pool, refresh_manager)?;
                let fee_bps = self.get_dex_fee_bps(pool.get_dex_name());
                let sol_after_fee =
                    sol_amount_in as u128 * (10000 - fee_bps) as u128 / 10000;
                let new_sol = sol_reserve as u128 + sol_after_fee;
                if new_sol == 0 {
                    return None;
                }
                let new_token =
                    (sol_reserve as u128) * (token_reserve as u128) / new_sol;
                let out = (token_reserve as u128).saturating_sub(new_token);
                Some(out as u64)
            }
        }
    }

    /// Calculate sell output (Token -> SOL) using DEX-specific math.
    fn calculate_sell_quote(
        &self,
        pool: &Pool,
        token_amount_in: u64,
        refresh_manager: &PoolRefreshManager,
    ) -> Option<u64> {
        let state = refresh_manager.get_pool_state(pool.pool_address());

        match state {
            Some(DeserializedPoolState::Pump {
                virtual_sol_reserves, virtual_token_reserves, complete, ..
            }) => {
                if *complete || *virtual_sol_reserves == 0 || *virtual_token_reserves == 0 {
                    return None;
                }
                let new_token = *virtual_token_reserves as u128 + token_amount_in as u128;
                let new_sol = (*virtual_sol_reserves as u128)
                    * (*virtual_token_reserves as u128)
                    / new_token;
                let sol_out = (*virtual_sol_reserves as u128).saturating_sub(new_sol);
                let fee = sol_out / 100;
                Some(sol_out.saturating_sub(fee) as u64)
            }
            Some(DeserializedPoolState::Heaven {
                virtual_sol_reserves, virtual_token_reserves, complete, ..
            }) => {
                if *complete || *virtual_sol_reserves == 0 || *virtual_token_reserves == 0 {
                    return None;
                }
                let new_token = *virtual_token_reserves as u128 + token_amount_in as u128;
                let new_sol = (*virtual_sol_reserves as u128)
                    * (*virtual_token_reserves as u128)
                    / new_token;
                let sol_out = (*virtual_sol_reserves as u128).saturating_sub(new_sol);
                let fee = sol_out / 100;
                Some(sol_out.saturating_sub(fee) as u64)
            }
            Some(DeserializedPoolState::RaydiumClmm { fee_rate, .. }) => {
                // Constant-product approximation with vault balances + exact CLMM fee.
                let (token_reserve, sol_reserve) =
                    self.get_cached_reserves(pool, refresh_manager)?;
                let fee_bps = (*fee_rate / 100).min(9999) as u64;
                let token_after_fee =
                    token_amount_in as u128 * (10000 - fee_bps) as u128 / 10000;
                let new_token = token_reserve as u128 + token_after_fee;
                if new_token == 0 { return None; }
                let new_sol = (token_reserve as u128) * (sol_reserve as u128) / new_token;
                Some((sol_reserve as u128).saturating_sub(new_sol) as u64)
            }
            Some(DeserializedPoolState::WhirlpoolState { fee_rate, .. }) => {
                // Constant-product approximation with vault balances + exact Whirlpool fee.
                let (token_reserve, sol_reserve) =
                    self.get_cached_reserves(pool, refresh_manager)?;
                let fee_bps = (*fee_rate / 100).min(9999) as u64;
                let token_after_fee =
                    token_amount_in as u128 * (10000 - fee_bps) as u128 / 10000;
                let new_token = token_reserve as u128 + token_after_fee;
                if new_token == 0 { return None; }
                let new_sol = (token_reserve as u128) * (sol_reserve as u128) / new_token;
                Some((sol_reserve as u128).saturating_sub(new_sol) as u64)
            }
            Some(DeserializedPoolState::MeteoraDlmm { active_id, bin_step, base_factor, token_x_mint, .. }) => {
                let (token_reserve, sol_reserve) = self.get_cached_reserves(pool, refresh_manager)?;
                let sol_mint = Pubkey::try_from("So11111111111111111111111111111111111111112").unwrap();
                let x_is_sol = *token_x_mint == sol_mint;

                let price = (1.0_f64 + *bin_step as f64 / 10_000.0).powi(*active_id);
                if price <= 0.0 { return None; }

                let reserve_price = if x_is_sol {
                    token_reserve as f64 / sol_reserve.max(1) as f64
                } else {
                    sol_reserve as f64 / token_reserve.max(1) as f64
                };
                let deviation = (price / reserve_price.max(1e-30)).max(reserve_price.max(1e-30) / price);
                if deviation > 10.0 { return None; }

                let fee_bps = (*bin_step as u64 * *base_factor as u64 / 10_000).max(1).min(1000);
                let token_after_fee = token_amount_in as u128 * (10_000 - fee_bps) as u128 / 10_000;

                // Sell = token in → SOL out
                // X=SOL: price = Y/X = token/SOL → sol_out = token_in / price
                // Y=SOL: price = Y/X = SOL/token → sol_out = token_in * price
                let sol_out = if x_is_sol {
                    (token_after_fee as f64 / price) as u64
                } else {
                    (token_after_fee as f64 * price) as u64
                };
                if sol_out == 0 { return None; }
                let depth_cap = (sol_reserve / 200).max(1);
                Some(sol_out.min(depth_cap))
            }
            _ => {
                let (token_reserve, sol_reserve) =
                    self.get_cached_reserves(pool, refresh_manager)?;
                let fee_bps = self.get_dex_fee_bps(pool.get_dex_name());
                let token_after_fee =
                    token_amount_in as u128 * (10000 - fee_bps) as u128 / 10000;
                let new_token = token_reserve as u128 + token_after_fee;
                if new_token == 0 {
                    return None;
                }
                let new_sol =
                    (token_reserve as u128) * (sol_reserve as u128) / new_token;
                let out = (sol_reserve as u128).saturating_sub(new_sol);
                Some(out as u64)
            }
        }
    }

/// Fee in basis points per DEX
    fn get_dex_fee_bps(&self, dex_name: &str) -> u64 {
        match dex_name {
            "Raydium" | "RaydiumCp" | "MeteoraDAmm" | "MeteoraDAmmV2" => 25,
            "RaydiumClmm" => 25,
            "Pump" | "Heaven" => 100,
            "DLMM" | "Whirlpool" | "Solfi" | "Vertigo" | "Lifinity" => 30,
            "Phoenix" => 10,
            _ => 30,
        }
    }

    /// Optimal input: 2% of the smaller pool's SOL reserve, clamped.
    fn optimal_input_lamports(
        &self,
        pool1: &Pool,
        pool2: &Pool,
        refresh_manager: &PoolRefreshManager,
    ) -> Option<u64> {
        let (_, sol1) = self.get_cached_reserves(pool1, refresh_manager)?;
        let (_, sol2) = self.get_cached_reserves(pool2, refresh_manager)?;

        let smaller = sol1.min(sol2);
        let min_liq = (self.config.min_liquidity_sol * 1e9) as u64;
        if smaller < min_liq {
            return None;
        }

        let target = smaller / 50; // 2%
        let min_trade = 10_000_000u64; // 0.01 SOL
        let max_trade = 5_000_000_000u64; // 5 SOL hard cap — prevents CLMM vault sizes producing absurd inputs
        Some(target.max(min_trade).min(max_trade))
    }

    /// Confidence score (0-100) based on pool characteristics.
    fn compute_confidence(
        &self,
        pool1: &Pool,
        pool2: &Pool,
        refresh_manager: &PoolRefreshManager,
    ) -> u8 {
        let mut score: f64 = 50.0;

        if let (Some((_, sol1)), Some((_, sol2))) = (
            self.get_cached_reserves(pool1, refresh_manager),
            self.get_cached_reserves(pool2, refresh_manager),
        ) {
            let min_sol = sol1.min(sol2) as f64 / 1e9;
            if min_sol > 100.0 {
                score += 30.0;
            } else if min_sol > 10.0 {
                score += 20.0;
            } else if min_sol > 1.0 {
                score += 5.0;
            } else {
                score -= 10.0;
            }
        }

        for dex in [pool1.get_dex_name(), pool2.get_dex_name()] {
            match dex {
                "Raydium" | "DLMM" | "Whirlpool" | "Phoenix" => score += 5.0,
                "Pump" | "Heaven" => score -= 5.0,
                _ => {}
            }
        }

        score.clamp(0.0, 100.0) as u8
    }

    /// Whether a pool can produce reliable constant-product quotes.
    /// CLMM/Whirlpool pools require tick-based math and their vault balances
    /// are often incorrectly mapped, causing garbage quotes. Exclude them
    /// until proper on-chain tick data is available.
    fn is_scannable(pool: &Pool) -> bool {
        !matches!(pool.get_dex_name(), "RaydiumClmm" | "Whirlpool")
    }

    fn find_two_dex_opportunities(
        &self,
        pool_data: &MintPoolData,
        refresh_manager: &PoolRefreshManager,
    ) -> Vec<ArbitrageOpportunity> {
        let mut opps = Vec::new();
        let pools = &pool_data.pools;
        let token_mint = pool_data.mint.to_string();

        for (i, buy_pool) in pools.iter().enumerate() {
            if !Self::is_scannable(buy_pool) { continue; }
            for (j, sell_pool) in pools.iter().enumerate() {
                if i == j {
                    continue;
                }
                if !Self::is_scannable(sell_pool) { continue; }

                let input_lamports = match self.optimal_input_lamports(
                    buy_pool, sell_pool, refresh_manager,
                ) {
                    Some(v) => v,
                    None => continue,
                };

                let tokens_received = match self.calculate_buy_quote(
                    buy_pool, input_lamports, refresh_manager,
                ) {
                    Some(v) if v > 0 => v,
                    _ => continue,
                };

                let sol_output = match self.calculate_sell_quote(
                    sell_pool, tokens_received, refresh_manager,
                ) {
                    Some(v) if v > 0 => v,
                    _ => continue,
                };

                let input_sol = input_lamports as f64 / 1e9;
                let output_sol = sol_output as f64 / 1e9;
                let pnl_sol = output_sol - input_sol;
                let pnl_pct = (pnl_sol / input_sol) * 100.0;

                // Always log pairs that produce a real quote (positive or negative)
                info!("[Scan] {} -> {}: in={:.4} out={:.4} pnl={:+.4} SOL ({:+.2}%)",
                    buy_pool.get_dex_name(), sell_pool.get_dex_name(),
                    input_sol, output_sol, pnl_sol, pnl_pct);

                // Use f64 profit (pnl_sol/pnl_pct already computed) so negative-PnL
                // opportunities pass through when min_profit_percent is negative (test mode).
                let profit_sol = pnl_sol;
                let profit_pct = pnl_pct;

                if profit_pct < self.config.min_profit_percent {
                    continue;
                }

                // Sanity cap: real on-chain arb never exceeds ~50% in a single tx.
                // Anything higher is a garbage reserve read (e.g. non-SPL vault accounts
                // misread as SPL token balances). Reject and log so the bug is visible.
                if profit_pct > 50.0 {
                    warn!("[Arb] {} -> {}: REJECTED — unrealistic profit {:.2}% (likely corrupt reserves)",
                        buy_pool.get_dex_name(), sell_pool.get_dex_name(), profit_pct);
                    continue;
                }

                let confidence =
                    self.compute_confidence(buy_pool, sell_pool, refresh_manager);

                opps.push(ArbitrageOpportunity {
                    token_mint: token_mint.clone(),
                    path: vec![
                        PathStep {
                            dex: buy_pool.get_dex_name().to_string(),
                            pool_address: buy_pool.pool_address().to_string(),
                            action: "buy".to_string(),
                            price: tokens_received as f64 / input_lamports as f64,
                            token_in: pool_data.wallet_wsol_account.to_string(),
                            token_out: token_mint.clone(),
                            amount_in: input_lamports as f64,
                            amount_out: tokens_received as f64,
                            slippage_bps: 30,
                        },
                        PathStep {
                            dex: sell_pool.get_dex_name().to_string(),
                            pool_address: sell_pool.pool_address().to_string(),
                            action: "sell".to_string(),
                            price: sol_output as f64 / tokens_received as f64,
                            token_in: token_mint.clone(),
                            token_out: pool_data.wallet_wsol_account.to_string(),
                            amount_in: tokens_received as f64,
                            amount_out: sol_output as f64,
                            slippage_bps: 30,
                        },
                    ],
                    gross_profit_sol: profit_sol,
                    profit_percent: profit_pct,
                    input_amount_sol: input_sol,
                    expected_output_sol: output_sol,
                    pool_addresses: vec![
                        buy_pool.pool_address().to_string(),
                        sell_pool.pool_address().to_string(),
                    ],
                    risk_score: (100 - confidence).min(100),
                    confidence_score: confidence,
                    estimated_execution_ms: 100,
                });
            }
        }

        opps
    }

    fn find_multi_hop_opportunities(
        &self,
        pool_data: &MintPoolData,
        refresh_manager: &PoolRefreshManager,
    ) -> Vec<ArbitrageOpportunity> {
        let mut opps = Vec::new();
        let mut graph = TradingGraph::new();
        for pool in &pool_data.pools {
            graph.add_pool(pool);
        }

        for start_node in &graph.nodes {
            let cycles = graph.find_cycles(*start_node, 3);

            for cycle in cycles {
                if cycle.len() != 3 {
                    continue;
                }

                let first_pool = &cycle[0].2;
                let (_, first_sol) = match self.get_cached_reserves(first_pool, refresh_manager) {
                    Some(v) => v,
                    None => continue,
                };

                let input_lamports = (first_sol / 100).max(10_000_000);

                let mut current_amount = input_lamports;
                let mut current_token_in = *start_node;
                let mut path_steps = Vec::new();
                let mut all_pool_addresses = Vec::new();
                let mut valid = true;

                let cycle_len = cycle.len();
                for (leg_idx, (from_token, to_token, pool)) in cycle.iter().enumerate() {
                    if *from_token != current_token_in {
                        valid = false;
                        break;
                    }

                    let (action, amount_out) = if pool.base_mint() == from_token {
                        match self.calculate_buy_quote(pool, current_amount, refresh_manager) {
                            Some(out) if out > 0 => ("buy".to_string(), out),
                            _ => {
                                valid = false;
                                break;
                            }
                        }
                    } else if pool.token_mint() == from_token {
                        match self.calculate_sell_quote(pool, current_amount, refresh_manager) {
                            Some(out) if out > 0 => ("sell".to_string(), out),
                            _ => {
                                valid = false;
                                break;
                            }
                        }
                    } else {
                        valid = false;
                        break;
                    };

                    // Final leg gets extra slippage tolerance for close-out flexibility
                    let leg_slippage_bps = if leg_idx + 1 == cycle_len { 50 } else { 30 };

                    path_steps.push(PathStep {
                        dex: pool.get_dex_name().to_string(),
                        pool_address: pool.pool_address().to_string(),
                        action,
                        price: amount_out as f64 / current_amount as f64,
                        token_in: from_token.to_string(),
                        token_out: to_token.to_string(),
                        amount_in: current_amount as f64,
                        amount_out: amount_out as f64,
                        slippage_bps: leg_slippage_bps,
                    });
                    all_pool_addresses.push(pool.pool_address().to_string());
                    current_amount = amount_out;
                    current_token_in = *to_token;
                }

                if !valid || path_steps.len() != 3 {
                    continue;
                }

                let input_sol = input_lamports as f64 / 1e9;
                let output_sol = current_amount as f64 / 1e9;
                let profit_sol = output_sol - input_sol;
                let profit_pct = (profit_sol / input_sol) * 100.0;

                if profit_pct < self.config.min_profit_percent {
                    continue;
                }

                opps.push(ArbitrageOpportunity {
                    token_mint: start_node.to_string(),
                    path: path_steps,
                    gross_profit_sol: profit_sol,
                    profit_percent: profit_pct,
                    input_amount_sol: input_sol,
                    expected_output_sol: output_sol,
                    pool_addresses: all_pool_addresses,
                    risk_score: 60,
                    confidence_score: 70,
                    estimated_execution_ms: 200,
                });
            }
        }

        opps
    }
}
