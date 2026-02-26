use anyhow::{anyhow, Result};
use solana_client::rpc_client::RpcClient;
use solana_sdk::pubkey::Pubkey;
use std::{
    collections::HashMap,
    sync::Arc,
    time::{Duration, Instant},
};
use tracing::{debug, info, warn};

use crate::dex::{
    meteora::constants::dlmm_program_id,
    pump::constants::pump_program_id,
    raydium::constants::{raydium_cp_program_id, raydium_program_id},
    whirlpool::constants::whirlpool_program_id,
};

// ── Raydium V4 layout (non-Anchor) ───────────────────────────────────────────
// Offsets verified against dex/raydium/amm_info.rs
const RAY_V4_COIN_MINT_OFFSET: usize = 400;
const RAY_V4_PC_MINT_OFFSET: usize = 432;
const RAY_V4_MIN_LEN: usize = RAY_V4_PC_MINT_OFFSET + 32; // 464

// ── Raydium CP layout (Anchor, 8-byte discriminator at 0) ────────────────────
// Offsets verified against dex/raydium/cp_amm_info.rs
const RAY_CP_TOKEN0_MINT_OFFSET: usize = 168;
const RAY_CP_TOKEN1_MINT_OFFSET: usize = 200;
const RAY_CP_MIN_LEN: usize = RAY_CP_TOKEN1_MINT_OFFSET + 32; // 232

// ── Whirlpool layout (Anchor, LEN = 8 + 261 + 384 = 653) ────────────────────
// Computed from dex/whirlpool/state.rs Whirlpool struct (fields in order):
//  discriminator(8) + whirlpools_config(32) + bump(1) + tick_spacing(2)
//  + tick_spacing_seed(2) + fee_rate(2) + protocol_fee_rate(2)
//  + liquidity(16) + sqrt_price(16) + tick_current_index(4)
//  + protocol_fee_owed_a(8) + protocol_fee_owed_b(8)
//  → token_mint_a at 101, token_mint_b at 181
const WP_TOKEN_MINT_A_OFFSET: usize = 101;
const WP_TOKEN_MINT_B_OFFSET: usize = 181;
const WP_EXACT_LEN: usize = 653;

// ── Meteora DLMM LbPair layout (Anchor) ─────────────────────────────────────
// Computed from dex/meteora/dlmm_info.rs LbPair struct:
//  discriminator(8) + StaticParameters(32) + VariableParameters(32)
//  + bump_seed(1) + bin_step_seed(2) + pair_type(1) + active_id(4)
//  + bin_step(2) + status(1) + require_base_factor_seed(1)
//  + base_factor_seed(2) + activation_type(1) + _padding_0(1)
//  → token_x_mint at 88, token_y_mint at 120
const DLMM_TOKEN_X_OFFSET: usize = 88;
const DLMM_TOKEN_Y_OFFSET: usize = 120;
const DLMM_MIN_LEN: usize = DLMM_TOKEN_Y_OFFSET + 32; // 152

// ── Pump AMM layout (Anchor) ─────────────────────────────────────────────────
// Verified against dex/pump/amm_info.rs:
//  data = &data[8 + 1 + 2 + 32..] → base_mint at 0, quote_mint at 32 (relative)
const PUMP_BASE_MINT_OFFSET: usize = 43; // 8+1+2+32
const PUMP_QUOTE_MINT_OFFSET: usize = 75; // 43+32
const PUMP_MIN_LEN: usize = PUMP_QUOTE_MINT_OFFSET + 32; // 107

/// Information about a discovered pool.
#[derive(Debug, Clone)]
pub struct PoolInfo {
    pub address: String,
    pub token_a_mint: String,
    pub token_b_mint: String,
    pub liquidity_sol: f64,
    pub dex: String,
    pub discovered_at: Instant,
}

/// Configuration for pool discovery.
#[derive(Debug, Clone)]
pub struct PoolDiscoveryConfig {
    pub enabled: bool,
    /// Minimum liquidity to keep a discovered pool (SOL units).
    pub min_liquidity_sol: f64,
    pub refresh_interval_minutes: u64,
    pub max_pools_per_dex: usize,
    pub discovery_dexes: Vec<String>,
}

impl Default for PoolDiscoveryConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            min_liquidity_sol: 10.0,
            refresh_interval_minutes: 120,
            max_pools_per_dex: 1000,
            discovery_dexes: vec![
                "raydium_v4".to_string(),
                "raydium_cp".to_string(),
                "whirlpool".to_string(),
                "meteora_dlmm".to_string(),
                "pump".to_string(),
            ],
        }
    }
}

/// Pool discovery engine – scans program accounts once per refresh interval.
pub struct PoolDiscovery {
    rpc_client: Arc<RpcClient>,
    pub config: PoolDiscoveryConfig,
    discovered_pools: HashMap<String, Vec<PoolInfo>>,
    last_discovery: Instant,
}

impl PoolDiscovery {
    pub fn new(rpc_client: Arc<RpcClient>, config: PoolDiscoveryConfig) -> Self {
        Self {
            rpc_client,
            config,
            discovered_pools: HashMap::new(),
            // Trigger immediate discovery on first should_refresh() call.
            last_discovery: Instant::now() - Duration::from_secs(999_999),
        }
    }

    /// Discover all pools across every configured DEX.
    pub async fn discover_all_pools(&mut self) -> Result<HashMap<String, Vec<PoolInfo>>> {
        info!("Starting pool discovery across all DEXs...");
        let start = Instant::now();
        let mut all_pools: HashMap<String, Vec<PoolInfo>> = HashMap::new();

        for dex in self.config.discovery_dexes.clone() {
            match self.discover_dex_pools(&dex).await {
                Ok(pools) => {
                    info!(
                        "Discovered {} pools on {} (elapsed: {:?})",
                        pools.len(),
                        dex,
                        start.elapsed()
                    );
                    all_pools.insert(dex, pools);
                }
                Err(e) => warn!("Failed to discover pools on {}: {}", dex, e),
            }
        }

        self.discovered_pools = all_pools.clone();
        self.last_discovery = Instant::now();

        info!(
            "Pool discovery complete: {} total pools in {:?}",
            self.discovered_pools
                .values()
                .map(|p| p.len())
                .sum::<usize>(),
            start.elapsed()
        );

        Ok(all_pools)
    }

    async fn discover_dex_pools(&self, dex: &str) -> Result<Vec<PoolInfo>> {
        match dex {
            "raydium_v4" => self.discover_raydium_v4_pools().await,
            "raydium_cp" => self.discover_raydium_cp_pools().await,
            "whirlpool" => self.discover_whirlpool_pools().await,
            "meteora_dlmm" => self.discover_meteora_dlmm_pools().await,
            "pump" => self.discover_pump_pools().await,
            other => {
                warn!("Unknown DEX: {}", other);
                Ok(Vec::new())
            }
        }
    }

    // ── Raydium V4 ───────────────────────────────────────────────────────────

    async fn discover_raydium_v4_pools(&self) -> Result<Vec<PoolInfo>> {
        info!("Discovering Raydium V4 pools...");
        let program_id = raydium_program_id();

        let raw = self
            .rpc_client
            .get_program_accounts(&program_id)
            .map_err(|e| anyhow!("RPC error (Raydium V4): {}", e))?;

        // Raydium V4 is non-Anchor; filter by minimum account size only.
        let accounts: Vec<_> = raw
            .into_iter()
            .filter(|(_, acc)| acc.data.len() >= RAY_V4_MIN_LEN)
            .collect();

        info!("Found {} Raydium V4 accounts after size filter", accounts.len());

        let mut pools = Vec::new();
        for (address, account) in accounts.iter().take(self.config.max_pools_per_dex) {
            match self.parse_raydium_v4_pool(address, &account.data) {
                Ok(pool) if pool.liquidity_sol >= self.config.min_liquidity_sol => {
                    debug!("Raydium V4 pool: {}", address);
                    pools.push(pool);
                }
                Ok(_) => {}
                Err(e) => debug!("Skip Raydium V4 {}: {}", address, e),
            }
        }

        Ok(pools)
    }

    fn parse_raydium_v4_pool(&self, address: &Pubkey, data: &[u8]) -> Result<PoolInfo> {
        if data.len() < RAY_V4_MIN_LEN {
            return Err(anyhow!("data too short"));
        }

        let coin_mint = Pubkey::new_from_array(
            data[RAY_V4_COIN_MINT_OFFSET..RAY_V4_COIN_MINT_OFFSET + 32]
                .try_into()
                .map_err(|_| anyhow!("slice error"))?,
        );
        let pc_mint = Pubkey::new_from_array(
            data[RAY_V4_PC_MINT_OFFSET..RAY_V4_PC_MINT_OFFSET + 32]
                .try_into()
                .map_err(|_| anyhow!("slice error"))?,
        );

        // Reject accounts with all-zero mints (uninitialized / wrong type).
        if coin_mint == Pubkey::default() || pc_mint == Pubkey::default() {
            return Err(anyhow!("zero mints – likely not a pool account"));
        }

        Ok(PoolInfo {
            address: address.to_string(),
            token_a_mint: coin_mint.to_string(),
            token_b_mint: pc_mint.to_string(),
            liquidity_sol: 100.0, // TODO: fetch vault balances for real liquidity
            dex: "raydium_v4".to_string(),
            discovered_at: Instant::now(),
        })
    }

    // ── Raydium CP ───────────────────────────────────────────────────────────

    async fn discover_raydium_cp_pools(&self) -> Result<Vec<PoolInfo>> {
        info!("Discovering Raydium CP pools...");
        let program_id = raydium_cp_program_id();

        let raw = self
            .rpc_client
            .get_program_accounts(&program_id)
            .map_err(|e| anyhow!("RPC error (Raydium CP): {}", e))?;

        // Filter by minimum size required to read token mints (Anchor accounts).
        let accounts: Vec<_> = raw
            .into_iter()
            .filter(|(_, acc)| acc.data.len() >= RAY_CP_MIN_LEN)
            .collect();

        info!("Found {} Raydium CP accounts after size filter", accounts.len());

        let mut pools = Vec::new();
        for (address, account) in accounts.iter().take(self.config.max_pools_per_dex) {
            match self.parse_raydium_cp_pool(address, &account.data) {
                Ok(pool) if pool.liquidity_sol >= self.config.min_liquidity_sol => {
                    debug!("Raydium CP pool: {}", address);
                    pools.push(pool);
                }
                Ok(_) => {}
                Err(e) => debug!("Skip Raydium CP {}: {}", address, e),
            }
        }

        Ok(pools)
    }

    fn parse_raydium_cp_pool(&self, address: &Pubkey, data: &[u8]) -> Result<PoolInfo> {
        if data.len() < RAY_CP_MIN_LEN {
            return Err(anyhow!("data too short"));
        }

        let token_0_mint = Pubkey::new_from_array(
            data[RAY_CP_TOKEN0_MINT_OFFSET..RAY_CP_TOKEN0_MINT_OFFSET + 32]
                .try_into()
                .map_err(|_| anyhow!("slice error"))?,
        );
        let token_1_mint = Pubkey::new_from_array(
            data[RAY_CP_TOKEN1_MINT_OFFSET..RAY_CP_TOKEN1_MINT_OFFSET + 32]
                .try_into()
                .map_err(|_| anyhow!("slice error"))?,
        );

        if token_0_mint == Pubkey::default() || token_1_mint == Pubkey::default() {
            return Err(anyhow!("zero mints"));
        }

        Ok(PoolInfo {
            address: address.to_string(),
            token_a_mint: token_0_mint.to_string(),
            token_b_mint: token_1_mint.to_string(),
            liquidity_sol: 100.0,
            dex: "raydium_cp".to_string(),
            discovered_at: Instant::now(),
        })
    }

    // ── Whirlpool ────────────────────────────────────────────────────────────

    async fn discover_whirlpool_pools(&self) -> Result<Vec<PoolInfo>> {
        info!("Discovering Whirlpool pools...");
        let program_id = whirlpool_program_id();

        let raw = self
            .rpc_client
            .get_program_accounts(&program_id)
            .map_err(|e| anyhow!("RPC error (Whirlpool): {}", e))?;

        // Whirlpool::LEN = 653; only pool-state accounts have this exact size.
        let accounts: Vec<_> = raw
            .into_iter()
            .filter(|(_, acc)| acc.data.len() == WP_EXACT_LEN)
            .collect();

        info!("Found {} Whirlpool accounts (exact size {})", accounts.len(), WP_EXACT_LEN);

        let mut pools = Vec::new();
        for (address, account) in accounts.iter().take(self.config.max_pools_per_dex) {
            match self.parse_whirlpool_pool(address, &account.data) {
                Ok(pool) if pool.liquidity_sol >= self.config.min_liquidity_sol => {
                    debug!("Whirlpool pool: {}", address);
                    pools.push(pool);
                }
                Ok(_) => {}
                Err(e) => debug!("Skip Whirlpool {}: {}", address, e),
            }
        }

        Ok(pools)
    }

    fn parse_whirlpool_pool(&self, address: &Pubkey, data: &[u8]) -> Result<PoolInfo> {
        if data.len() < WP_TOKEN_MINT_B_OFFSET + 32 {
            return Err(anyhow!("data too short"));
        }

        let token_mint_a = Pubkey::new_from_array(
            data[WP_TOKEN_MINT_A_OFFSET..WP_TOKEN_MINT_A_OFFSET + 32]
                .try_into()
                .map_err(|_| anyhow!("slice error"))?,
        );
        let token_mint_b = Pubkey::new_from_array(
            data[WP_TOKEN_MINT_B_OFFSET..WP_TOKEN_MINT_B_OFFSET + 32]
                .try_into()
                .map_err(|_| anyhow!("slice error"))?,
        );

        if token_mint_a == Pubkey::default() || token_mint_b == Pubkey::default() {
            return Err(anyhow!("zero mints"));
        }

        Ok(PoolInfo {
            address: address.to_string(),
            token_a_mint: token_mint_a.to_string(),
            token_b_mint: token_mint_b.to_string(),
            liquidity_sol: 100.0,
            dex: "whirlpool".to_string(),
            discovered_at: Instant::now(),
        })
    }

    // ── Meteora DLMM ─────────────────────────────────────────────────────────

    async fn discover_meteora_dlmm_pools(&self) -> Result<Vec<PoolInfo>> {
        info!("Discovering Meteora DLMM pools...");
        let program_id = dlmm_program_id();

        let raw = self
            .rpc_client
            .get_program_accounts(&program_id)
            .map_err(|e| anyhow!("RPC error (Meteora DLMM): {}", e))?;

        let accounts: Vec<_> = raw
            .into_iter()
            .filter(|(_, acc)| acc.data.len() >= DLMM_MIN_LEN)
            .collect();

        info!("Found {} Meteora DLMM accounts after size filter", accounts.len());

        let mut pools = Vec::new();
        for (address, account) in accounts.iter().take(self.config.max_pools_per_dex) {
            match self.parse_meteora_dlmm_pool(address, &account.data) {
                Ok(pool) if pool.liquidity_sol >= self.config.min_liquidity_sol => {
                    debug!("Meteora DLMM pool: {}", address);
                    pools.push(pool);
                }
                Ok(_) => {}
                Err(e) => debug!("Skip Meteora DLMM {}: {}", address, e),
            }
        }

        Ok(pools)
    }

    fn parse_meteora_dlmm_pool(&self, address: &Pubkey, data: &[u8]) -> Result<PoolInfo> {
        if data.len() < DLMM_MIN_LEN {
            return Err(anyhow!("data too short"));
        }

        let token_x_mint = Pubkey::new_from_array(
            data[DLMM_TOKEN_X_OFFSET..DLMM_TOKEN_X_OFFSET + 32]
                .try_into()
                .map_err(|_| anyhow!("slice error"))?,
        );
        let token_y_mint = Pubkey::new_from_array(
            data[DLMM_TOKEN_Y_OFFSET..DLMM_TOKEN_Y_OFFSET + 32]
                .try_into()
                .map_err(|_| anyhow!("slice error"))?,
        );

        if token_x_mint == Pubkey::default() || token_y_mint == Pubkey::default() {
            return Err(anyhow!("zero mints"));
        }

        Ok(PoolInfo {
            address: address.to_string(),
            token_a_mint: token_x_mint.to_string(),
            token_b_mint: token_y_mint.to_string(),
            liquidity_sol: 100.0,
            dex: "meteora_dlmm".to_string(),
            discovered_at: Instant::now(),
        })
    }

    // ── Pump AMM ─────────────────────────────────────────────────────────────

    async fn discover_pump_pools(&self) -> Result<Vec<PoolInfo>> {
        info!("Discovering Pump AMM pools...");
        let program_id = pump_program_id();

        let raw = self
            .rpc_client
            .get_program_accounts(&program_id)
            .map_err(|e| anyhow!("RPC error (Pump): {}", e))?;

        let accounts: Vec<_> = raw
            .into_iter()
            .filter(|(_, acc)| acc.data.len() >= PUMP_MIN_LEN)
            .collect();

        info!("Found {} Pump accounts after size filter", accounts.len());

        let mut pools = Vec::new();
        for (address, account) in accounts.iter().take(self.config.max_pools_per_dex) {
            match self.parse_pump_pool(address, &account.data) {
                Ok(pool) if pool.liquidity_sol >= self.config.min_liquidity_sol => {
                    debug!("Pump pool: {}", address);
                    pools.push(pool);
                }
                Ok(_) => {}
                Err(e) => debug!("Skip Pump {}: {}", address, e),
            }
        }

        Ok(pools)
    }

    fn parse_pump_pool(&self, address: &Pubkey, data: &[u8]) -> Result<PoolInfo> {
        if data.len() < PUMP_MIN_LEN {
            return Err(anyhow!("data too short"));
        }

        let base_mint = Pubkey::new_from_array(
            data[PUMP_BASE_MINT_OFFSET..PUMP_BASE_MINT_OFFSET + 32]
                .try_into()
                .map_err(|_| anyhow!("slice error"))?,
        );
        let quote_mint = Pubkey::new_from_array(
            data[PUMP_QUOTE_MINT_OFFSET..PUMP_QUOTE_MINT_OFFSET + 32]
                .try_into()
                .map_err(|_| anyhow!("slice error"))?,
        );

        if base_mint == Pubkey::default() || quote_mint == Pubkey::default() {
            return Err(anyhow!("zero mints"));
        }

        Ok(PoolInfo {
            address: address.to_string(),
            token_a_mint: base_mint.to_string(),
            token_b_mint: quote_mint.to_string(),
            liquidity_sol: 100.0,
            dex: "pump".to_string(),
            discovered_at: Instant::now(),
        })
    }

    // ── Public helpers ───────────────────────────────────────────────────────

    pub fn should_refresh(&self) -> bool {
        self.last_discovery.elapsed().as_secs()
            > self.config.refresh_interval_minutes * 60
    }

    pub fn get_all_pools(&self) -> Vec<PoolInfo> {
        self.discovered_pools
            .values()
            .flat_map(|v| v.iter().cloned())
            .collect()
    }

    /// Return all discovered pools that contain `token_mint` as either token.
    pub fn get_pools_for_token(&self, token_mint: &str) -> Vec<PoolInfo> {
        self.discovered_pools
            .values()
            .flat_map(|v| v.iter().cloned())
            .filter(|p| p.token_a_mint == token_mint || p.token_b_mint == token_mint)
            .collect()
    }

    pub fn get_pool_counts(&self) -> HashMap<String, usize> {
        self.discovered_pools
            .iter()
            .map(|(dex, pools)| (dex.clone(), pools.len()))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_pool_info_creation() {
        let pool = PoolInfo {
            address: "test".to_string(),
            token_a_mint: "TokenA".to_string(),
            token_b_mint: "TokenB".to_string(),
            liquidity_sol: 100.0,
            dex: "raydium_v4".to_string(),
            discovered_at: Instant::now(),
        };
        assert_eq!(pool.liquidity_sol, 100.0);
    }

    #[test]
    fn test_discovery_config_default() {
        let config = PoolDiscoveryConfig::default();
        assert!(config.enabled);
        assert_eq!(config.min_liquidity_sol, 10.0);
        assert_eq!(config.max_pools_per_dex, 1000);
    }

    #[test]
    fn test_offset_sanity() {
        // Confirm min-length constants are derived consistently.
        assert_eq!(RAY_V4_MIN_LEN, 464);
        assert_eq!(RAY_CP_MIN_LEN, 232);
        assert_eq!(WP_EXACT_LEN, 653);
        assert_eq!(DLMM_MIN_LEN, 152);
        assert_eq!(PUMP_MIN_LEN, 107);
    }
}
