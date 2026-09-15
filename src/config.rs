#![allow(dead_code)]
use anyhow::{Result, anyhow};
use solana_sdk::pubkey::Pubkey;
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
    pub dynamic_cu_enabled: bool,
    pub cu_buffer_pct: f64,
    pub dynamic_fee_enabled: bool,
    pub fee_percentile: usize,
    pub alt_addresses: Vec<String>,
    pub mints: Vec<MintConfig>,
    pub per_leg_slippage_bps: u16,
    // --- MarginFi liquidator ---
    pub marginfi_liquidator_enabled: bool,
    pub marginfi_group: String,
    pub marginfi_liquidator_account: String,
    pub marginfi_min_profit_usd: f64,
    pub marginfi_max_position_sol: f64,
    pub marginfi_scan_interval_ms: u64,
    pub marginfi_health_buffer: f64,
    pub marginfi_liquidation_bonus: f64,
    pub marginfi_max_oracle_conf_pct: f64,
    pub marginfi_max_oracle_age_secs: i64,
    pub marginfi_swap_slippage_pct: f64,
    pub marginfi_swap_fee_pct: f64,
    pub marginfi_fixed_cost_usd: f64,
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

        let dynamic_cu_enabled = std::env::var("DYNAMIC_CU_ENABLED")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(false);

        let cu_buffer_pct = std::env::var("CU_BUFFER_PCT")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(0.15f64);

        let dynamic_fee_enabled = std::env::var("DYNAMIC_FEE_ENABLED")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(false);

        let fee_percentile = std::env::var("FEE_PERCENTILE")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(75usize);

        let alt_addresses: Vec<String> = std::env::var("ALT_ADDRESSES")
            .unwrap_or_default()
            .split(',')
            .filter(|s| !s.is_empty())
            .map(|s| s.trim().to_string())
            .collect();

        let mints = Self::load_mints()?;

        let per_leg_slippage_bps = std::env::var("PER_LEG_SLIPPAGE_BPS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(30u16);

        // --- MarginFi liquidator ---
        let env_f64 = |k: &str, d: f64| -> f64 {
            std::env::var(k).ok().and_then(|v| v.parse().ok()).unwrap_or(d)
        };
        let marginfi_liquidator_enabled = std::env::var("MARGINFI_LIQUIDATOR_ENABLED")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(false);
        let marginfi_group = std::env::var("MARGINFI_GROUP").unwrap_or_default();
        let marginfi_liquidator_account =
            std::env::var("MARGINFI_LIQUIDATOR_ACCOUNT").unwrap_or_default();
        let marginfi_min_profit_usd = env_f64("MARGINFI_MIN_PROFIT_USD", 5.0);
        let marginfi_max_position_sol = env_f64("MARGINFI_MAX_POSITION_SOL", 50.0);
        let marginfi_scan_interval_ms = std::env::var("MARGINFI_SCAN_INTERVAL_MS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(400u64);
        let marginfi_health_buffer = env_f64("MARGINFI_HEALTH_BUFFER", 0.0);
        let marginfi_liquidation_bonus = env_f64("MARGINFI_LIQUIDATION_BONUS", 0.05);
        let marginfi_max_oracle_conf_pct = env_f64("MARGINFI_MAX_ORACLE_CONF_PCT", 0.02);
        let marginfi_max_oracle_age_secs = std::env::var("MARGINFI_MAX_ORACLE_AGE_SECS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(60i64);
        let marginfi_swap_slippage_pct = env_f64("MARGINFI_SWAP_SLIPPAGE_PCT", 0.005);
        let marginfi_swap_fee_pct = env_f64("MARGINFI_SWAP_FEE_PCT", 0.003);
        let marginfi_fixed_cost_usd = env_f64("MARGINFI_FIXED_COST_USD", 0.5);

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
            dynamic_cu_enabled,
            cu_buffer_pct,
            dynamic_fee_enabled,
            fee_percentile,
            alt_addresses,
            mints,
            per_leg_slippage_bps,
            marginfi_liquidator_enabled,
            marginfi_group,
            marginfi_liquidator_account,
            marginfi_min_profit_usd,
            marginfi_max_position_sol,
            marginfi_scan_interval_ms,
            marginfi_health_buffer,
            marginfi_liquidation_bonus,
            marginfi_max_oracle_conf_pct,
            marginfi_max_oracle_age_secs,
            marginfi_swap_slippage_pct,
            marginfi_swap_fee_pct,
            marginfi_fixed_cost_usd,
        })
    }

    fn load_mints() -> Result<Vec<MintConfig>> {
        let mut mints = Vec::new();

        for i in 1..=20 {
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
        // Config loads from .env if present; verify parsing works and values are reasonable
        let config = BotConfig::from_env().unwrap();
        assert!(!config.enable_real_execution);
        assert_eq!(config.loop_interval_ms, 500);
        // compute_unit_limit defaults to 400k, but .env may override to 600k
        assert!(config.compute_unit_limit == 400_000 || config.compute_unit_limit == 600_000);
        assert_eq!(config.cu_buffer_pct, 0.15);
        assert_eq!(config.fee_percentile, 75);
    }
}
