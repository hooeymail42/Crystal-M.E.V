use crate::chain::{
    pools::{MintPoolData, Pool, PoolData},
    trading_graph::TradingGraph,
};
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
            min_liquidity_sol: 10.0,
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

    fn get_pool_reserves(&self, pool: &dyn PoolData) -> anyhow::Result<(u64, u64)> {
        let token_vault_balance = self.rpc_client.get_token_account_balance(pool.token_vault())?;
        let sol_vault_balance = self.rpc_client.get_token_account_balance(pool.sol_vault())?;

        Ok((
            token_vault_balance.ui_amount.unwrap_or(0.0) as u64,
            sol_vault_balance.ui_amount.unwrap_or(0.0) as u64,
        ))
    }

    pub fn find_opportunities(
        &mut self,
        pool_data: &MintPoolData,
    ) -> Vec<ArbitrageOpportunity> {
        let mut opportunities = Vec::new();
        let graph = self.build_trading_graph(pool_data);

        // Find 2-DEX opportunities
        opportunities.extend(self.find_two_dex_opportunities(pool_data));
        
        // Find multi-hop opportunities
        opportunities.extend(self.find_multi_hop_opportunities(pool_data));

        // Sort by profit (highest first)
        opportunities.sort_by(|a, b| {
            b.profit_percent
                .partial_cmp(&a.profit_percent)
                .unwrap_or(Ordering::Equal)
        });

        // Filter and return
        opportunities.retain(|opp| opp.profit_percent > self.config.min_profit_percent);
        opportunities
    }

    fn build_trading_graph(&self, pool_data: &MintPoolData) -> TradingGraph {
        let mut graph = TradingGraph::new();
        for pool in &pool_data.pools {
            graph.add_pool(pool);
        }
        graph
    }

    fn get_pool_price<'b>(&self, pool: &'b Pool) -> anyhow::Result<(f64, &'b str)> {
        let (token_reserves, sol_reserves) = self.get_pool_reserves(pool)?;
        if sol_reserves == 0 { return Err(anyhow::anyhow!("SOL reserves are zero")); }
        Ok((token_reserves as f64 / sol_reserves as f64, pool.get_dex_name()))
    }

    fn find_two_dex_opportunities(
        &self,
        pool_data: &MintPoolData,
    ) -> Vec<ArbitrageOpportunity> {
        let mut opps = Vec::new();
        let pools = &pool_data.pools;

        for (i, pool1) in pools.iter().enumerate() {
            for (j, pool2) in pools.iter().enumerate() {
                if i == j { continue; }

                if let (Ok((price1, dex1)), Ok((price2, dex2))) = (self.get_pool_price(pool1), self.get_pool_price(pool2)) {
                    let profit = price2 - price1;
                    if profit > 0.0 {
                        opps.push(self.create_opp(dex1, dex2, profit));
                    }
                }
            }
        }

        opps
    }

    fn find_multi_hop_opportunities(
        &self,
        pool_data: &MintPoolData,
    ) -> Vec<ArbitrageOpportunity> {
        let mut opps = Vec::new();
        let graph = self.build_trading_graph(pool_data);

        for start_node in &graph.nodes {
            let cycles = graph.find_cycles(*start_node, 3); // Find 3-hop cycles
            
            for cycle in cycles {
                if cycle.len() != 3 { continue; } // Ensure it's a 3-hop cycle

                let mut path_steps: Vec<PathStep> = Vec::new();
                let mut current_amount = 1.0; // Starting with 1 SOL for simplicity
                let mut current_token_in_pubkey = *start_node;
                let mut all_pool_addresses: Vec<String> = Vec::new();

                let mut profitable_cycle = true;

                for (step_from_token, step_to_token, pool) in cycle {
                    if step_from_token != current_token_in_pubkey {
                        profitable_cycle = false;
                        break;
                    }
                    
                    let (token_reserves, sol_reserves) = match self.get_pool_reserves(&pool) {
                        Ok(res) => res,
                        Err(_) => {
                            profitable_cycle = false;
                            break;
                        }
                    };

                    if sol_reserves == 0 || token_reserves == 0 {
                        profitable_cycle = false;
                        break;
                    }

                    let price: f64;
                    let action: String;
                    let token_in_str: String;
                    let token_out_str: String;

                    if pool.token_mint() == &step_from_token { // Trading from Token (token_mint) to SOL (base_mint)
                        price = token_reserves as f64 / sol_reserves as f64;
                        action = "sell".to_string();
                        token_in_str = pool.token_mint().to_string();
                        token_out_str = pool.base_mint().to_string();
                    } else if pool.base_mint() == &step_from_token { // Trading from SOL (base_mint) to Token (token_mint)
                        price = sol_reserves as f64 / token_reserves as f64;
                        action = "buy".to_string();
                        token_in_str = pool.base_mint().to_string();
                        token_out_str = pool.token_mint().to_string();
                    } else {
                        profitable_cycle = false;
                        break;
                    }

                    let amount_out = current_amount * price; // Simplified, actual calculation needs to consider slippage etc.

                    path_steps.push(PathStep {
                        dex: pool.get_dex_name().to_string(),
                        pool_address: pool.pool_address().to_string(),
                        action: action,
                        price: price,
                        token_in: token_in_str,
                        token_out: token_out_str,
                        amount_in: current_amount,
                        amount_out: amount_out,
                    });
                    all_pool_addresses.push(pool.pool_address().to_string());
                    current_amount = amount_out;
                    current_token_in_pubkey = step_to_token;
                }

                if profitable_cycle && path_steps.len() == 3 {
                    // Assuming the cycle ends with the initial token (e.g., SOL)
                    let profit = current_amount - 1.0; // Compare final amount with initial 1 SOL
                    if profit > self.config.min_profit_percent { // Check against min_profit_percent, not just > 0.0
                        opps.push(ArbitrageOpportunity {
                            token_mint: start_node.to_string(), // Starting token of the cycle
                            path: path_steps,
                            gross_profit_sol: profit,
                            profit_percent: profit * 100.0,
                            input_amount_sol: 1.0,
                            expected_output_sol: current_amount,
                            pool_addresses: all_pool_addresses,
                            risk_score: 50, // Placeholder
                            confidence_score: 80, // Placeholder
                            estimated_execution_ms: 150, // Placeholder
                        });
                    }
                }
            }
        }

        opps
    }

    fn create_opp(&self, dex1: &str, dex2: &str, profit: f64) -> ArbitrageOpportunity {
        let profit_pct = profit * 100.0;
        
        ArbitrageOpportunity {
            token_mint: "test".to_string(),
            path: vec![
                PathStep {
                    dex: dex1.to_string(),
                    pool_address: "pool1".to_string(),
                    action: "buy".to_string(),
                    price: 1.0,
                    token_in: "SOL".to_string(),
                    token_out: "TOKEN".to_string(),
                    amount_in: 1.0,
                    amount_out: 1.0,
                },
                PathStep {
                    dex: dex2.to_string(),
                    pool_address: "pool2".to_string(),
                    action: "sell".to_string(),
                    price: 1.0 + profit,
                    token_in: "TOKEN".to_string(),
                    token_out: "SOL".to_string(),
                    amount_in: 1.0,
                    amount_out: 1.0 + profit,
                },
            ],
            gross_profit_sol: profit,
            profit_percent: profit_pct,
            input_amount_sol: 1.0,
            expected_output_sol: 1.0 + profit,
            pool_addresses: vec!["pool1".to_string(), "pool2".to_string()],
            risk_score: 45,
            confidence_score: 85,
            estimated_execution_ms: 100,
        }
    }
}

/*
#[cfg(test)]
mod tests {
    use super::*;
    use crate::chain::pools::{PumpPool, RaydiumPool}; // Keep these imports for now, even if not directly used in new structure
    use solana_client::rpc_client::RpcClient;
    use solana_sdk::pubkey::Pubkey;
    use std::str::FromStr;

    #[test]
    fn test_find_opportunities() {
        let rpc_client = RpcClient::new_mock("succeeds");

        let mut config = OpportunityConfig::default();
        config.min_profit_percent = 0.0; // Set min profit to 0 for testing purposes
        let mut detector = OpportunityDetector::new(config, &rpc_client);

        let mut pool_data = MintPoolData::default();
        let token_mint_sol = Pubkey::from_str("So11111111111111111111111111111111111111112").unwrap();
        let token_mint_usdc = Pubkey::from_str("EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v").unwrap();
        let token_mint_usdt = Pubkey::from_str("Es9vMFrzaCERmJfrF4H2cpdgBnGsmvPXsCjQQxW7GgqA").unwrap();

        // Add a Raydium Pool (SOL-USDC)
        pool_data.add_raydium_pool(
            "J2F2sL4K4H8G5J8K3L2M1N0O6Q7P3L7F4H8G5J8K3", // Valid dummy pool address
            "AK4R2M1N0O6Q7P3L7F4H8G5J8K3L2M1N0O6Q7P", // Valid dummy token_vault
            "BK4R2M1N0O6Q7P3L7F4H8G5J8K3L2M1N0O6Q7P", // Valid dummy sol_vault
            &token_mint_usdc.to_string(),
            &token_mint_sol.to_string(),
        ).unwrap();

        // Add a Pump Pool (SOL-USDC, for demonstration, assuming it functions like Raydium for reserves)
        pool_data.add_pump_pool(
            "C2F2sL4K4H8G5J8K3L2M1N0O6Q7P3L7F4H8G5J8K3", // Valid dummy pool address
            "DK4R2M1N0O6Q7P3L7F4H8G5J8K3L2M1N0O6Q7P", // Valid dummy token_vault
            "EK4R2M1N0O6Q7P3L7F4H8G5J8K3L2M1N0O6Q7P", // Valid dummy sol_vault
            "fee_wallet",
            "creator_ata",
            "creator_authority",
            &token_mint_usdc.to_string(),
            &token_mint_sol.to_string(),
        ).unwrap();

        // Add a DLMM Pool (USDC-USDT)
        pool_data.add_dlmm_pool(
            "F2F2sL4K4H8G5J8K3L2M1N0O6Q7P3L7F4H8G5J8K3", // Valid dummy pool address
            "GK4R2M1N0O6Q7P3L7F4H8G5J8K3L2M1N0O6Q7P", // Valid dummy token_vault (USDC)
            "HK4R2M1N0O6Q7P3L7F4H8G5J8K3L2M1N0O6Q7P", // Valid dummy sol_vault (USDT)
            "oracle", // Dummy oracle
            vec!["bin_array1", "bin_array2"], // Dummy bin_arrays
            None, // No memo program
            &token_mint_usdt.to_string(),
            &token_mint_usdc.to_string(),
        ).unwrap();

        let opps = detector.find_opportunities(&pool_data);

        // Assert that some opportunities are found, without checking specific profit values yet
        assert!(!opps.is_empty(), "Should find some arbitrage opportunities");
        // Optionally, print found opportunities for inspection during development
        for opp in opps {
            println!("{:?}", opp);
        }
    }
}
*/