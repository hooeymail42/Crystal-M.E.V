use anyhow::{Result, anyhow};
use solana_sdk::pubkey::Pubkey;
use std::collections::HashMap;
use std::str::FromStr;

#[derive(Debug, Clone)]
pub struct BotConfig {
    pub rpc_url: String,
    pub ws_url: String,
    pub wallet_private_key: String,
    pub min_profit_sol: f64,
    pub max_slippage_pct: f64,
    pub compute_unit_limit: u32,
    pub priority_fee_lamports: u64,
    pub jito_enabled: bool,
    pub jito_tip_lamports: u64,
    pub jito_block_engine_url: String,
    pub spam_enabled: bool,
    pub spam_rpc_urls: Vec<String>,
    pub flashloan_enabled: bool,
    pub flashloan_reserve: String,
    pub flashloan_reserve_vault: String,
    pub flashloan_fee_receiver: String,
    pub enable_real_execution: bool,
    pub refresh_interval_ms: u64,
    pub loop_interval_ms: u64,
    pub mints: Vec<MintConfig>,
}

#[derive(Debug, Clone)]
pub struct MintConfig {
    pub mint: Pubkey,
    pub raydium_pools: Vec<String>,
    pub raydium_cp_pools: Vec<String>,
    pub pump_pools: Vec<String>,
    pub dlmm_pools: Vec<String>,
    pub whirlpool_pools: Vec<String>,
    pub raydium_clmm_pools: Vec<String>,
    pub meteora_damm_pools: Vec<String>,
    pub solfi_pools: Vec<String>,
    pub meteora_damm_v2_pools: Vec<String>,
    pub vertigo_pools: Vec<String>,
    pub phoenix_pools: Vec<String>,
    pub lifinity_pools: Vec<String>,
    pub heaven_pools: Vec<String>,
}

impl BotConfig {
    pub fn from_env() -> Result<Self> {
        dotenv::dotenv().ok();

        let rpc_url = std::env::var("RPC_URL")
            .unwrap_or_else(|_| "https://api.mainnet-beta.solana.com".to_string());

        let ws_url = std::env::var("WS_URL")
            .unwrap_or_else(|_| rpc_url.replace("https://", "wss://").replace("http://", "ws://"));

        let wallet_private_key = std::env::var("WALLET_PRIVATE_KEY")
            .or_else(|_| std::env::var("SOLANA_KEYPAIR"))
            .or_else(|_| std::env::var("SOLANA_KEYPAIR_BASE58"))
            .unwrap_or_default();

        let min_profit_sol = std::env::var("MIN_PROFIT_SOL")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(0.001);

        let max_slippage_pct = std::env::var("MAX_SLIPPAGE_PCT")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(1.0);

        let compute_unit_limit = std::env::var("BOT_COMPUTE_UNIT_LIMIT")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(400_000u32);

        let priority_fee_lamports = std::env::var("PRIORITY_FEE_LAMPORTS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(10_000u64);

        let jito_enabled = std::env::var("JITO_ENABLED")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(false);

        let jito_tip_lamports = std::env::var("JITO_TIP_LAMPORTS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(10_000u64);

        let jito_block_engine_url = std::env::var("JITO_BLOCK_ENGINE_URL")
            .unwrap_or_else(|_| "https://mainnet.block-engine.jito.wtf".to_string());

        let spam_enabled = std::env::var("SPAM_ENABLED")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(false);

        let spam_rpc_urls: Vec<String> = std::env::var("SPAM_SENDING_RPC_URLS")
            .unwrap_or_default()
            .split(',')
            .filter(|s| !s.is_empty())
            .map(|s| s.trim().to_string())
            .collect();

        let flashloan_enabled = std::env::var("FLASHLOAN_ENABLED")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(false);

        let flashloan_reserve = std::env::var("FLASHLOAN_RESERVE").unwrap_or_default();
        let flashloan_reserve_vault = std::env::var("FLASHLOAN_RESERVE_VAULT").unwrap_or_default();
        let flashloan_fee_receiver = std::env::var("FLASHLOAN_FEE_RECEIVER").unwrap_or_default();

        let enable_real_execution = std::env::var("ENABLE_REAL_EXECUTION")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(false);

        let refresh_interval_ms = std::env::var("REFRESH_INTERVAL_MS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(2000u64);

        let loop_interval_ms = std::env::var("LOOP_INTERVAL_MS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(500u64);

        let mints = Self::load_mints()?;

        Ok(Self {
            rpc_url,
            ws_url,
            wallet_private_key,
            min_profit_sol,
            max_slippage_pct,
            compute_unit_limit,
            priority_fee_lamports,
            jito_enabled,
            jito_tip_lamports,
            jito_block_engine_url,
            spam_enabled,
            spam_rpc_urls,
            flashloan_enabled,
            flashloan_reserve,
            flashloan_reserve_vault,
            flashloan_fee_receiver,
            enable_real_execution,
            refresh_interval_ms,
            loop_interval_ms,
            mints,
        })
    }

    fn load_mints() -> Result<Vec<MintConfig>> {
        let mut mints = Vec::new();

        for i in 1..=10 {
            let mint_key = format!("MINT_{}", i);
            if let Ok(mint_str) = std::env::var(&mint_key) {
                let mint = Pubkey::from_str(&mint_str)
                    .map_err(|e| anyhow!("Invalid pubkey for {}: {}", mint_key, e))?;

                let parse_pool_list = |suffix: &str| -> Vec<String> {
                    std::env::var(format!("MINT_{}_{}", i, suffix))
                        .unwrap_or_default()
                        .split(',')
                        .filter(|s| !s.is_empty())
                        .map(|s| s.trim().to_string())
                        .collect()
                };

                mints.push(MintConfig {
                    mint,
                    raydium_pools: parse_pool_list("RAYDIUM_POOL_LIST"),
                    raydium_cp_pools: parse_pool_list("RAYDIUM_CP_POOL_LIST"),
                    pump_pools: parse_pool_list("PUMP_POOL_LIST"),
                    dlmm_pools: parse_pool_list("DLMM_POOL_LIST"),
                    whirlpool_pools: parse_pool_list("WHIRLPOOL_POOL_LIST"),
                    raydium_clmm_pools: parse_pool_list("RAYDIUM_CLMM_POOL_LIST"),
                    meteora_damm_pools: parse_pool_list("METEORA_DAMM_POOL_LIST"),
                    solfi_pools: parse_pool_list("SOLFI_POOL_LIST"),
                    meteora_damm_v2_pools: parse_pool_list("METEORA_DAMM_V2_POOL_LIST"),
                    vertigo_pools: parse_pool_list("VERTIGO_POOL_LIST"),
                    phoenix_pools: parse_pool_list("PHOENIX_POOL_LIST"),
                    lifinity_pools: parse_pool_list("LIFINITY_POOL_LIST"),
                    heaven_pools: parse_pool_list("HEAVEN_POOL_LIST"),
                });
            }
        }

        Ok(mints)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config() {
        // Without any env vars set, should still produce a valid config
        let config = BotConfig::from_env().unwrap();
        assert!(!config.enable_real_execution);
        assert_eq!(config.loop_interval_ms, 500);
        assert_eq!(config.compute_unit_limit, 400_000);
    }
}
