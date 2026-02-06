use crate::chain::{
    pools::{MintPoolData, Pool, PoolData},
    refresh::{DeserializedPoolState, PoolRefreshManager},
    trading_graph::TradingGraph,
};
use tracing::{debug, info};
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
}

#[derive(Debug, Clone)]
pub struct OpportunityConfig {
    pub min_profit_percent: f64,
    pub min_liquidity_sol: f64,
    pub max_slippage_percent: f64,
    pub max_volatility_percent: f64,
}

impl Default for OpportunityConfig {
    fn default() -> Self {
        Self {
            min_profit_percent: 0.3,
            min_liquidity_sol: 1.0,
            max_slippage_percent: 1.0,
            max_volatility_percent: 10.0,
        }
    }
}

pub struct OpportunityDetector<'a> {
    config: OpportunityConfig,
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
            Some(DeserializedPoolState::RaydiumClmm {
                mint_0, sqrt_price_x64, liquidity, fee_rate, ..
            }) => {
                // CLMM tick-based math: determine direction based on which mint is SOL
                let sol_mint: Pubkey = "So11111111111111111111111111111111111111112".parse().ok()?;
                let is_sol_mint_0 = *mint_0 == sol_mint;

                if *liquidity == 0 || *sqrt_price_x64 == 0 {
                    return None;
                }

                // Apply fee (fee_rate is per million)
                let fee_amount = (sol_amount_in as u128) * (*fee_rate as u128) / 1_000_000;
                let amount_after_fee = (sol_amount_in as u128).saturating_sub(fee_amount);

                let l = *liquidity;
                let sqrt_p = *sqrt_price_x64;

                if is_sol_mint_0 {
                    // SOL is token_0, buying token_1: use 0_to_1 formula
                    // new_sqrt_price = L * sqrt_price / (L + delta_0 * sqrt_price / 2^64)
                    let denominator = l + (amount_after_fee * sqrt_p) / (1u128 << 64);
                    if denominator == 0 { return None; }
                    let new_sqrt_price = l * sqrt_p / denominator;
                    // delta_1 = L * (sqrt_price - new_sqrt_price) / 2^64
                    let delta_out = if sqrt_p > new_sqrt_price {
                        l * (sqrt_p - new_sqrt_price) / (1u128 << 64)
                    } else { 0 };
                    Some(delta_out as u64)
                } else {
                    // SOL is token_1, buying token_0: use 1_to_0 formula
                    // new_sqrt_price = sqrt_price + delta_1 * 2^64 / L
                    let new_sqrt_price = sqrt_p + (amount_after_fee * (1u128 << 64)) / l;
                    // delta_0 = L * (new_sqrt_price - sqrt_price) / (sqrt_price * new_sqrt_price / 2^64)
                    let numerator = l * (new_sqrt_price - sqrt_p);
                    let denom_factor = (sqrt_p / (1u128 << 32)) * (new_sqrt_price / (1u128 << 32));
                    let delta_out = if denom_factor > 0 { numerator / denom_factor } else { 0 };
                    Some(delta_out as u64)
                }
            }
            Some(DeserializedPoolState::WhirlpoolState {
                mint_a, sqrt_price, liquidity, fee_rate, ..
            }) => {
                // Whirlpool CLMM math using floats
                let sol_mint: Pubkey = "So11111111111111111111111111111111111111112".parse().ok()?;
                let is_sol_mint_a = *mint_a == sol_mint;

                if *liquidity == 0 || *sqrt_price == 0 {
                    return None;
                }

                let sqrt_price_f = *sqrt_price as f64 / (1u128 << 64) as f64;
                let liquidity_f = *liquidity as f64;

                // fee_rate is in hundredths of bps (parts per million)
                let fee_amount = (sol_amount_in as u128 * *fee_rate as u128 / 1_000_000) as u64;
                let amount_after_fee = sol_amount_in.saturating_sub(fee_amount);

                if is_sol_mint_a {
                    // SOL is token_a, buying token_b: a_to_b
                    let new_sqrt_price = liquidity_f * sqrt_price_f
                        / (liquidity_f + amount_after_fee as f64 * sqrt_price_f);
                    if new_sqrt_price <= 0.0 || new_sqrt_price >= sqrt_price_f {
                        return None;
                    }
                    let delta_b = liquidity_f * (sqrt_price_f - new_sqrt_price);
                    Some(delta_b as u64)
                } else {
                    // SOL is token_b, buying token_a: b_to_a
                    let new_sqrt_price = sqrt_price_f + amount_after_fee as f64 / liquidity_f;
                    let delta_a = liquidity_f * (1.0 / sqrt_price_f - 1.0 / new_sqrt_price);
                    Some(delta_a as u64)
                }
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
            Some(DeserializedPoolState::RaydiumClmm {
                mint_0, sqrt_price_x64, liquidity, fee_rate, ..
            }) => {
                // CLMM tick-based math: Token -> SOL (reverse direction)
                let sol_mint: Pubkey = "So11111111111111111111111111111111111111112".parse().ok()?;
                let is_sol_mint_0 = *mint_0 == sol_mint;

                if *liquidity == 0 || *sqrt_price_x64 == 0 {
                    return None;
                }

                let fee_amount = (token_amount_in as u128) * (*fee_rate as u128) / 1_000_000;
                let amount_after_fee = (token_amount_in as u128).saturating_sub(fee_amount);

                let l = *liquidity;
                let sqrt_p = *sqrt_price_x64;

                if is_sol_mint_0 {
                    // SOL is token_0, selling token_1 for SOL: use 1_to_0 formula
                    let new_sqrt_price = sqrt_p + (amount_after_fee * (1u128 << 64)) / l;
                    let numerator = l * (new_sqrt_price - sqrt_p);
                    let denom_factor = (sqrt_p / (1u128 << 32)) * (new_sqrt_price / (1u128 << 32));
                    let delta_out = if denom_factor > 0 { numerator / denom_factor } else { 0 };
                    Some(delta_out as u64)
                } else {
                    // SOL is token_1, selling token_0 for SOL: use 0_to_1 formula
                    let denominator = l + (amount_after_fee * sqrt_p) / (1u128 << 64);
                    if denominator == 0 { return None; }
                    let new_sqrt_price = l * sqrt_p / denominator;
                    let delta_out = if sqrt_p > new_sqrt_price {
                        l * (sqrt_p - new_sqrt_price) / (1u128 << 64)
                    } else { 0 };
                    Some(delta_out as u64)
                }
            }
            Some(DeserializedPoolState::WhirlpoolState {
                mint_a, sqrt_price, liquidity, fee_rate, ..
            }) => {
                // Whirlpool CLMM math: Token -> SOL (reverse direction)
                let sol_mint: Pubkey = "So11111111111111111111111111111111111111112".parse().ok()?;
                let is_sol_mint_a = *mint_a == sol_mint;

                if *liquidity == 0 || *sqrt_price == 0 {
                    return None;
                }

                let sqrt_price_f = *sqrt_price as f64 / (1u128 << 64) as f64;
                let liquidity_f = *liquidity as f64;

                let fee_amount = (token_amount_in as u128 * *fee_rate as u128 / 1_000_000) as u64;
                let amount_after_fee = token_amount_in.saturating_sub(fee_amount);

                if is_sol_mint_a {
                    // SOL is token_a, selling token_b for SOL: b_to_a
                    let new_sqrt_price = sqrt_price_f + amount_after_fee as f64 / liquidity_f;
                    let delta_a = liquidity_f * (1.0 / sqrt_price_f - 1.0 / new_sqrt_price);
                    Some(delta_a as u64)
                } else {
                    // SOL is token_b, selling token_a for SOL: a_to_b
                    let new_sqrt_price = liquidity_f * sqrt_price_f
                        / (liquidity_f + amount_after_fee as f64 * sqrt_price_f);
                    if new_sqrt_price <= 0.0 || new_sqrt_price >= sqrt_price_f {
                        return None;
                    }
                    let delta_b = liquidity_f * (sqrt_price_f - new_sqrt_price);
                    Some(delta_b as u64)
                }
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
        Some(target.max(min_trade))
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

    fn find_two_dex_opportunities(
        &self,
        pool_data: &MintPoolData,
        refresh_manager: &PoolRefreshManager,
    ) -> Vec<ArbitrageOpportunity> {
        let mut opps = Vec::new();
        let pools = &pool_data.pools;
        let token_mint = pool_data.mint.to_string();

        // Log price comparison every 50 iterations for debugging
        static CALL_COUNT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let call_num = CALL_COUNT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let should_log = call_num % 50 == 0;

        if should_log {
            info!("[Arb] Checking {} pools, call #{}", pools.len(), call_num);
        }

        for (i, buy_pool) in pools.iter().enumerate() {
            for (j, sell_pool) in pools.iter().enumerate() {
                if i == j {
                    continue;
                }

                let input_lamports = match self.optimal_input_lamports(
                    buy_pool, sell_pool, refresh_manager,
                ) {
                    Some(v) => v,
                    None => {
                        if should_log {
                            info!("[Arb] {} -> {}: SKIP (no liquidity/reserves)",
                                buy_pool.get_dex_name(), sell_pool.get_dex_name());
                        }
                        continue;
                    }
                };

                let tokens_received = match self.calculate_buy_quote(
                    buy_pool, input_lamports, refresh_manager,
                ) {
                    Some(v) if v > 0 => v,
                    _ => {
                        if should_log {
                            info!("[Arb] {} -> {}: SKIP (no buy quote)",
                                buy_pool.get_dex_name(), sell_pool.get_dex_name());
                        }
                        continue;
                    }
                };

                let sol_output = match self.calculate_sell_quote(
                    sell_pool, tokens_received, refresh_manager,
                ) {
                    Some(v) if v > 0 => v,
                    _ => {
                        if should_log {
                            info!("[Arb] {} -> {}: SKIP (no sell quote)",
                                buy_pool.get_dex_name(), sell_pool.get_dex_name());
                        }
                        continue;
                    }
                };

                let input_sol = input_lamports as f64 / 1e9;
                let output_sol = sol_output as f64 / 1e9;
                let pnl_sol = output_sol - input_sol;
                let pnl_pct = (pnl_sol / input_sol) * 100.0;

                if should_log {
                    info!("[Arb] {} -> {}: in={:.4} SOL, out={:.4} SOL, pnl={:.6} SOL ({:.4}%)",
                        buy_pool.get_dex_name(), sell_pool.get_dex_name(),
                        input_sol, output_sol, pnl_sol, pnl_pct);
                }

                if sol_output <= input_lamports {
                    continue;
                }

                let profit_lamports = sol_output - input_lamports;
                let profit_sol = profit_lamports as f64 / 1e9;
                let profit_pct = pnl_pct;

                if profit_pct < self.config.min_profit_percent {
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

                for (from_token, to_token, pool) in &cycle {
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

                    path_steps.push(PathStep {
                        dex: pool.get_dex_name().to_string(),
                        pool_address: pool.pool_address().to_string(),
                        action,
                        price: amount_out as f64 / current_amount as f64,
                        token_in: from_token.to_string(),
                        token_out: to_token.to_string(),
                        amount_in: current_amount as f64,
                        amount_out: amount_out as f64,
                    });
                    all_pool_addresses.push(pool.pool_address().to_string());
                    current_amount = amount_out;
                    current_token_in = *to_token;
                }

                if !valid || path_steps.len() != 3 || current_amount <= input_lamports {
                    continue;
                }

                let profit_lamports = current_amount - input_lamports;
                let input_sol = input_lamports as f64 / 1e9;
                let output_sol = current_amount as f64 / 1e9;
                let profit_sol = profit_lamports as f64 / 1e9;
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
