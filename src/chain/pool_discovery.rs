#![allow(dead_code)]
//! Dynamic pool discovery from Solana blockchain via getProgramAccounts.
//!
//! Uses on-chain account filters (discriminator + data-size) to discover
//! pools across Raydium V4, Raydium CP, Whirlpool, Meteora DLMM, and Pump.fun.
//! Results are cached for `refresh_interval_minutes` and can be merged with
//! hardcoded .env pools as a backward-compatible fallback.

use anyhow::{anyhow, Result};
use solana_client::rpc_client::RpcClient;
use solana_client::rpc_config::{RpcProgramAccountsConfig, RpcAccountInfoConfig};
use solana_client::rpc_filter::{RpcFilterType, Memcmp, MemcmpEncodedBytes};
use solana_account_decoder::UiDataSliceConfig;
use solana_sdk::pubkey::Pubkey;
use solana_sdk::commitment_config::CommitmentConfig;
use solana_sdk::account::Account;
use std::{
    collections::HashMap,
    str::FromStr,
    sync::Arc,
    time::{Duration, Instant},
};
use tracing::{debug, info, warn};
use serde::{Deserialize, Serialize};

// ── Constants ────────────────────────────────────────────────────────────────

pub const SOL_MINT_STR: &str = "So11111111111111111111111111111111111111112";

// Verified discriminators (sha256("account:<TypeName>")[0..8])
// Raydium V4: sha256("account:AmmInfo")[0..8] = 6a4cd153bc1940d8
pub const RAYDIUM_V4_DISCRIMINATOR: [u8; 8] = [0x6a, 0x4c, 0xd1, 0x53, 0xbc, 0x19, 0x40, 0xd8];
pub const RAYDIUM_V4_ACCOUNT_SIZE: u64 = 1664;

// Raydium CP: PoolState account from cp-swap program
// sha256("account:PoolState")[0..8]
pub const RAYDIUM_CP_DISCRIMINATOR: [u8; 8] = [0xd8, 0x52, 0x75, 0x0a, 0x61, 0x77, 0x27, 0x0f];
pub const RAYDIUM_CP_ACCOUNT_SIZE: u64 = 637;

// Whirlpool: sha256("account:Whirlpool")[0..8]
pub const WHIRLPOOL_DISCRIMINATOR: [u8; 8] = [0x63, 0xad, 0xb5, 0x9c, 0x66, 0x74, 0x07, 0x5e];
pub const WHIRLPOOL_ACCOUNT_SIZE: u64 = 653;

// Meteora DLMM LbPair: sha256("account:LbPair")[0..8]
pub const METEORA_DLMM_DISCRIMINATOR: [u8; 8] = [0x33, 0x51, 0x43, 0x37, 0x48, 0xe5, 0x81, 0x75];

// Pump.fun AMM: bonding curve account — fixed 300 byte size, no discriminator
pub const PUMP_ACCOUNT_SIZE: u64 = 300;

// Program IDs
pub const RAYDIUM_V4_PROGRAM: &str = "675kPX9MHTjS2zt1qfr1NYHuzeLXFQM5p84CmjZrtsm";
pub const RAYDIUM_CP_PROGRAM: &str = "CPMMoo8L3F4NbTegBCKVNunggL7H1ZpdTHKxQB5qKP1C";
pub const WHIRLPOOL_PROGRAM: &str = "whirLbMiicVdio4KfQ7QV1mKpQ2dB6A8mEy93gVe5t";
pub const METEORA_DLMM_PROGRAM: &str = "LBUZKhRxPF3XUpBCjp4YzTKgLccjZhTSDM9YuVaPwxo";
pub const PUMP_PROGRAM: &str = "6EF8rQNwhS2q7s7D3F7p4CevG5vQTGSwbDVefyxE7tE";

// ── Data Structures ──────────────────────────────────────────────────────────

/// Discovered pool with enough data to add to MintPoolData
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiscoveredPool {
    pub address: Pubkey,
    pub dex: String,
    pub token_mint: Pubkey,
    pub quote_mint: Pubkey,
    /// Estimated SOL-side liquidity (0 = unknown, filter will use min threshold)
    pub liquidity_sol: f64,
    pub fee_percent: f64,
    /// Additional accounts needed for swap instructions
    pub token_vault: Pubkey,
    pub sol_vault: Pubkey,
    pub discovered_at: u64, // unix timestamp (seconds)
}

/// Lightweight cache indexed by token mint string
#[derive(Debug, Clone, Default)]
pub struct PoolDiscoveryCache {
    pub pools_by_token: HashMap<String, Vec<DiscoveredPool>>,
    pub last_update_ts: u64,
}

impl PoolDiscoveryCache {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add_pool(&mut self, token_mint: &str, pool: DiscoveredPool) {
        self.pools_by_token
            .entry(token_mint.to_string())
            .or_default()
            .push(pool);
    }

    pub fn get_pools(&self, token_mint: &str) -> Option<Vec<DiscoveredPool>> {
        self.pools_by_token.get(token_mint).cloned()
    }

    pub fn get_liquid_pools(
        &self,
        token_mint: &str,
        min_liquidity_sol: f64,
    ) -> Vec<DiscoveredPool> {
        self.pools_by_token
            .get(token_mint)
            .map(|pools| {
                pools
                    .iter()
                    .filter(|p| p.liquidity_sol >= min_liquidity_sol || p.liquidity_sol == 0.0)
                    .cloned()
                    .collect()
            })
            .unwrap_or_default()
    }

    pub fn get_top_pools(&self, token_mint: &str, limit: usize) -> Vec<DiscoveredPool> {
        let mut pools = self
            .pools_by_token
            .get(token_mint)
            .cloned()
            .unwrap_or_default();
        pools.sort_by(|a, b| b.liquidity_sol.partial_cmp(&a.liquidity_sol).unwrap_or(std::cmp::Ordering::Equal));
        pools.into_iter().take(limit).collect()
    }

    pub fn total_pools(&self) -> usize {
        self.pools_by_token.values().map(|v| v.len()).sum()
    }

    pub fn clear(&mut self) {
        self.pools_by_token.clear();
        self.last_update_ts = 0;
    }

    pub fn find_arbitrage_candidates(
        &self,
        token_mint: &str,
        min_liquidity_sol: f64,
        min_pools: usize,
    ) -> Option<Vec<DiscoveredPool>> {
        let pools = self.get_liquid_pools(token_mint, min_liquidity_sol);
        if pools.len() >= min_pools {
            Some(pools)
        } else {
            None
        }
    }
}

/// Configuration for pool discovery engine
#[derive(Debug, Clone)]
pub struct PoolDiscoveryConfig {
    /// Enable or disable discovery (use only hardcoded pools when false)
    pub enabled: bool,
    /// Minimum SOL-side liquidity to include a pool (0 = include all)
    pub min_liquidity_sol: f64,
    /// How often to re-run discovery (minutes)
    pub refresh_interval_minutes: u64,
    /// Cap on pools returned per DEX program
    pub max_pools_per_dex: usize,
    /// Which DEXs to scan
    pub discovery_dexes: Vec<String>,
}

impl Default for PoolDiscoveryConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            min_liquidity_sol: 5.0,
            refresh_interval_minutes: 120,
            // 2000 per DEX gives a much larger token universe than 500.
            // getProgramAccounts returns accounts in arbitrary order so a larger cap
            // captures more unique token mints. At ~1ms per account parse this adds
            // roughly 1-3s to startup discovery time per DEX — acceptable at launch.
            max_pools_per_dex: 2000,
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

impl PoolDiscoveryConfig {
    /// Load from environment variables with defaults
    pub fn from_env() -> Self {
        let enabled = std::env::var("POOL_DISCOVERY_ENABLED")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(true);
        let min_liquidity_sol = std::env::var("POOL_DISCOVERY_MIN_LIQUIDITY_SOL")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(5.0);
        let refresh_interval_minutes = std::env::var("POOL_DISCOVERY_REFRESH_MINUTES")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(120u64);
        let max_pools_per_dex = std::env::var("POOL_DISCOVERY_MAX_PER_DEX")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(2000usize);

        let dexes_str = std::env::var("POOL_DISCOVERY_DEXES")
            .unwrap_or_else(|_| "raydium_v4,raydium_cp,whirlpool,meteora_dlmm,pump".to_string());
        let discovery_dexes = dexes_str
            .split(',')
            .filter(|s| !s.is_empty())
            .map(|s| s.trim().to_string())
            .collect();

        Self {
            enabled,
            min_liquidity_sol,
            refresh_interval_minutes,
            max_pools_per_dex,
            discovery_dexes,
        }
    }
}

// ── Pool Discovery Engine ─────────────────────────────────────────────────────

/// Active pool discovery engine backed by Solana RPC.
pub struct PoolDiscovery {
    rpc_client: Arc<RpcClient>,
    pub config: PoolDiscoveryConfig,
    pub cache: PoolDiscoveryCache,
    last_discovery: Option<Instant>,
}

impl PoolDiscovery {
    pub fn new(rpc_client: Arc<RpcClient>, config: PoolDiscoveryConfig) -> Self {
        Self {
            rpc_client,
            config,
            cache: PoolDiscoveryCache::new(),
            last_discovery: None,
        }
    }

    /// Returns true if a re-discovery run is due.
    pub fn should_refresh(&self) -> bool {
        match self.last_discovery {
            None => true,
            Some(t) => t.elapsed() > Duration::from_secs(self.config.refresh_interval_minutes * 60),
        }
    }

    /// Discover all pools across all configured DEXs.
    /// This is the main entry point — call once at startup and periodically.
    pub fn discover_all_pools(&mut self) -> Result<&PoolDiscoveryCache> {
        if !self.config.enabled {
            info!("[PoolDiscovery] Disabled via config, skipping");
            return Ok(&self.cache);
        }

        info!("[PoolDiscovery] Starting discovery across DEXs: {:?}", self.config.discovery_dexes);
        let start = Instant::now();
        self.cache.clear();

        let dexes = self.config.discovery_dexes.clone();
        for dex in &dexes {
            match self.discover_dex(dex) {
                Ok(count) => {
                    info!("[PoolDiscovery] {} → {} pools ({:.1}s elapsed)", dex, count, start.elapsed().as_secs_f32());
                }
                Err(e) => {
                    warn!("[PoolDiscovery] {} failed: {}", dex, e);
                }
            }
        }

        let now_ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        self.cache.last_update_ts = now_ts;
        self.last_discovery = Some(Instant::now());

        info!(
            "[PoolDiscovery] Complete: {} total pools across {} DEXs in {:.1}s",
            self.cache.total_pools(),
            dexes.len(),
            start.elapsed().as_secs_f32()
        );

        Ok(&self.cache)
    }

    /// Discover pools for a single named DEX.
    fn discover_dex(&mut self, dex: &str) -> Result<usize> {
        match dex {
            "raydium_v4" => self.discover_raydium_v4(),
            "raydium_cp" => self.discover_raydium_cp(),
            "whirlpool" => self.discover_whirlpool(),
            "meteora_dlmm" => self.discover_meteora_dlmm(),
            "pump" => self.discover_pump(),
            other => Err(anyhow!("Unknown DEX name: {}", other)),
        }
    }

    // ── Raydium V4 ────────────────────────────────────────────────────────────

    /// Discover Raydium V4 AMM pools via data-size-only filter.
    ///
    /// IMPORTANT: Raydium V4 is NOT Anchor-based and has NO 8-byte discriminator.
    /// The account data starts directly at offset 0 with `status: u64`.
    /// Using a memcmp discriminator filter returns zero results because the first
    /// 8 bytes are the pool status field (a small integer like 1, 2, 6, etc.),
    /// not a sha256-derived discriminator. We filter by exact account size only.
    fn discover_raydium_v4(&mut self) -> Result<usize> {
        let program_id = Pubkey::from_str(RAYDIUM_V4_PROGRAM)?;
        // We only need bytes 0..464 to read coin_vault(336), pc_vault(368),
        // coin_mint(400), pc_mint(432). Full account is 1664 bytes — fetching
        // only 464 bytes reduces payload by 72% and avoids provider size limits.
        let accounts = self.get_program_accounts_size_only(
            &program_id,
            RAYDIUM_V4_ACCOUNT_SIZE,
            464, // coin_mint ends at 432+32=464
        )?;

        let max = self.config.max_pools_per_dex;
        let min_liq = self.config.min_liquidity_sol;
        let now_ts = current_ts();
        let _ = now_ts; // suppress unused warning
        let mut count = 0usize;
        let mut parse_errors = 0usize;

        for (address, account) in accounts.into_iter().take(max) {
            match self.parse_raydium_v4(&address, &account) {
                Ok(pool) => {
                    if pool.liquidity_sol >= min_liq || pool.liquidity_sol == 0.0 {
                        let token_mint_str = pool.token_mint.to_string();
                        let quote_mint_str = pool.quote_mint.to_string();
                        debug!("[RaydiumV4] pool={} token={} quote={}", address, token_mint_str, quote_mint_str);
                        // Index under the non-SOL side mint
                        let index_mint = if quote_mint_str == SOL_MINT_STR {
                            token_mint_str.clone()
                        } else if token_mint_str == SOL_MINT_STR {
                            quote_mint_str.clone()
                        } else {
                            // Neither side is SOL — still index under token_mint for cross-mint arb
                            token_mint_str.clone()
                        };
                        self.cache.add_pool(&index_mint, pool);
                        count += 1;
                    }
                }
                Err(e) => {
                    parse_errors += 1;
                    if parse_errors <= 3 {
                        warn!("[RaydiumV4] parse error for {}: {}", address, e);
                    }
                }
            }
        }
        info!("[RaydiumV4] parsed {} pools ({} parse errors)", count, parse_errors);
        Ok(count)
    }

    /// Parse a Raydium V4 AmmInfo account.
    ///
    /// Raydium V4 is NOT Anchor-based — there is NO 8-byte discriminator prefix.
    /// The struct starts at byte 0 with `status: u64`. Offsets are derived from
    /// the field order in `dex/raydium/amm_info.rs`:
    ///
    ///   u64 fields (8 bytes each, 32 total):  0..256  (8×4 groups)
    ///   u128 fields and mixed u64 (336 total after u128s + u64 gaps):
    ///     swap_coin_in_amount  u128  at 256
    ///     swap_pc_out_amount   u128  at 272
    ///     swap_coin2_pc_fee    u64   at 288
    ///     swap_pc_in_amount    u128  at 296
    ///     swap_coin_out_amount u128  at 312
    ///     swap_pc2_coin_fee    u64   at 328
    ///   Pubkey fields (32 bytes each) from 336:
    ///     pool_coin_token_account  at 336  (coin vault)
    ///     pool_pc_token_account    at 368  (pc vault)
    ///     coin_mint_address        at 400
    ///     pc_mint_address          at 432
    fn parse_raydium_v4(&self, address: &Pubkey, account: &Account) -> Result<DiscoveredPool> {
        let data = &account.data;
        if data.len() < 464 {
            return Err(anyhow!("account too short for V4 AmmInfo: {} bytes (need ≥464)", data.len()));
        }

        let coin_vault = read_pubkey(data, 336)?;
        let pc_vault   = read_pubkey(data, 368)?;
        let coin_mint  = read_pubkey(data, 400)?;
        let pc_mint    = read_pubkey(data, 432)?;

        let sol_mint = Pubkey::from_str(SOL_MINT_STR)?;
        let (token_mint, quote_mint, token_vault, sol_vault) = if coin_mint == sol_mint {
            (pc_mint, coin_mint, pc_vault, coin_vault)
        } else {
            (coin_mint, pc_mint, coin_vault, pc_vault)
        };

        Ok(DiscoveredPool {
            address: *address,
            dex: "raydium_v4".to_string(),
            token_mint,
            quote_mint,
            liquidity_sol: 0.0, // populated separately if needed
            fee_percent: 0.25,
            token_vault,
            sol_vault,
            discovered_at: current_ts(),
        })
    }

    // ── Raydium CP ────────────────────────────────────────────────────────────

    /// Discover Raydium CP (constant-product) pools.
    fn discover_raydium_cp(&mut self) -> Result<usize> {
        let program_id = Pubkey::from_str(RAYDIUM_CP_PROGRAM)?;
        // CP PoolState has discriminator at offset 0 and a known compact size.
        // We only need bytes 0..232 (vaults at 72/104, mints at 168/200).
        let accounts = self.get_program_accounts_filtered(
            &program_id,
            &RAYDIUM_CP_DISCRIMINATOR,
            Some(RAYDIUM_CP_ACCOUNT_SIZE),
            232, // token_1_mint ends at 200+32=232
        )?;

        let max = self.config.max_pools_per_dex;
        let min_liq = self.config.min_liquidity_sol;
        let mut count = 0usize;

        for (address, account) in accounts.into_iter().take(max) {
            match self.parse_raydium_cp(&address, &account) {
                Ok(pool) => {
                    if pool.liquidity_sol >= min_liq || pool.liquidity_sol == 0.0 {
                        let index_mint = if pool.quote_mint.to_string() == SOL_MINT_STR {
                            pool.token_mint.to_string()
                        } else {
                            pool.token_mint.to_string()
                        };
                        debug!("[RaydiumCP] pool={} token={}", address, index_mint);
                        self.cache.add_pool(&index_mint, pool);
                        count += 1;
                    }
                }
                Err(e) => {
                    debug!("[RaydiumCP] parse error for {}: {}", address, e);
                }
            }
        }
        Ok(count)
    }

    /// Parse a Raydium CP PoolState account.
    ///
    /// CP PoolState layout (after 8-byte discriminator):
    ///   8     amm_config (Pubkey, 32 bytes) — offset 8
    ///   40    pool_creator (Pubkey, 32 bytes)
    ///   72    token_0_vault (Pubkey, 32 bytes)
    ///   104   token_1_vault (Pubkey, 32 bytes)
    ///   136   lp_mint (Pubkey, 32 bytes)
    ///   168   token_0_mint (Pubkey, 32 bytes)
    ///   200   token_1_mint (Pubkey, 32 bytes)
    fn parse_raydium_cp(&self, address: &Pubkey, account: &Account) -> Result<DiscoveredPool> {
        let data = &account.data;
        if data.len() < 232 {
            return Err(anyhow!("CP account too short: {} bytes", data.len()));
        }

        let vault_0 = read_pubkey(data, 72)?;
        let vault_1 = read_pubkey(data, 104)?;
        let mint_0 = read_pubkey(data, 168)?;
        let mint_1 = read_pubkey(data, 200)?;

        let sol_mint = Pubkey::from_str(SOL_MINT_STR)?;
        let (token_mint, quote_mint, token_vault, sol_vault) = if mint_0 == sol_mint {
            (mint_1, mint_0, vault_1, vault_0)
        } else {
            (mint_0, mint_1, vault_0, vault_1)
        };

        Ok(DiscoveredPool {
            address: *address,
            dex: "raydium_cp".to_string(),
            token_mint,
            quote_mint,
            liquidity_sol: 0.0,
            fee_percent: 0.25,
            token_vault,
            sol_vault,
            discovered_at: current_ts(),
        })
    }

    // ── Whirlpool ─────────────────────────────────────────────────────────────

    /// Discover Orca Whirlpool CLMM pools.
    fn discover_whirlpool(&mut self) -> Result<usize> {
        let program_id = Pubkey::from_str(WHIRLPOOL_PROGRAM)?;
        // We need bytes 0..245 to read fee_rate(45), mint_a(101), vault_a(133),
        // mint_b(181), vault_b(213). token_vault_b ends at 213+32=245.
        let accounts = self.get_program_accounts_filtered(
            &program_id,
            &WHIRLPOOL_DISCRIMINATOR,
            Some(WHIRLPOOL_ACCOUNT_SIZE),
            245, // token_vault_b ends at 213+32=245
        )?;

        let max = self.config.max_pools_per_dex;
        let min_liq = self.config.min_liquidity_sol;
        let mut count = 0usize;

        for (address, account) in accounts.into_iter().take(max) {
            match self.parse_whirlpool(&address, &account) {
                Ok(pool) => {
                    if pool.liquidity_sol >= min_liq || pool.liquidity_sol == 0.0 {
                        let index_mint = if pool.quote_mint.to_string() == SOL_MINT_STR {
                            pool.token_mint.to_string()
                        } else {
                            pool.token_mint.to_string()
                        };
                        debug!("[Whirlpool] pool={} token={}", address, index_mint);
                        self.cache.add_pool(&index_mint, pool);
                        count += 1;
                    }
                }
                Err(e) => {
                    debug!("[Whirlpool] parse error for {}: {}", address, e);
                }
            }
        }
        Ok(count)
    }

    /// Parse a Whirlpool account.
    ///
    /// Whirlpool layout (after 8-byte discriminator):
    ///   8    whirlpools_config (Pubkey, 32)
    ///   40   whirlpool_bump ([u8; 1])
    ///   41   tick_spacing (u16, 2)
    ///   43   tick_spacing_seed ([u8; 2])
    ///   45   fee_rate (u16)
    ///   47   protocol_fee_rate (u16)
    ///   49   liquidity (u128, 16)
    ///   65   sqrt_price (u128, 16)
    ///   81   tick_current_index (i32, 4)
    ///   85   protocol_fee_owed_a (u64)
    ///   93   protocol_fee_owed_b (u64)
    ///   101  token_mint_a (Pubkey, 32) — offset 101
    ///   133  token_vault_a (Pubkey, 32)
    ///   165  fee_growth_global_a (u128, 16)
    ///   181  token_mint_b (Pubkey, 32) — offset 181
    ///   213  token_vault_b (Pubkey, 32)
    fn parse_whirlpool(&self, address: &Pubkey, account: &Account) -> Result<DiscoveredPool> {
        let data = &account.data;
        if data.len() < 245 {
            return Err(anyhow!("Whirlpool account too short: {} bytes", data.len()));
        }

        let mint_a = read_pubkey(data, 101)?;
        let vault_a = read_pubkey(data, 133)?;
        let mint_b = read_pubkey(data, 181)?;
        let vault_b = read_pubkey(data, 213)?;
        let fee_rate = u16::from_le_bytes([data[45], data[46]]);

        let sol_mint = Pubkey::from_str(SOL_MINT_STR)?;
        let (token_mint, quote_mint, token_vault, sol_vault) = if mint_a == sol_mint {
            (mint_b, mint_a, vault_b, vault_a)
        } else {
            (mint_a, mint_b, vault_a, vault_b)
        };

        Ok(DiscoveredPool {
            address: *address,
            dex: "whirlpool".to_string(),
            token_mint,
            quote_mint,
            liquidity_sol: 0.0,
            fee_percent: fee_rate as f64 / 10000.0,
            token_vault,
            sol_vault,
            discovered_at: current_ts(),
        })
    }

    // ── Meteora DLMM ──────────────────────────────────────────────────────────

    /// Discover Meteora DLMM LbPair accounts.
    fn discover_meteora_dlmm(&mut self) -> Result<usize> {
        let program_id = Pubkey::from_str(METEORA_DLMM_PROGRAM)?;
        // DLMM account sizes vary (no exact size filter).
        // Parse reads d=data[8..]; needs up to d[176+32]=d[208] → absolute byte 216.
        // Fetching 216 bytes is sufficient; 8-byte discriminator + 208 bytes of body.
        let accounts = self.get_program_accounts_filtered(
            &program_id,
            &METEORA_DLMM_DISCRIMINATOR,
            None, // DLMM account sizes vary
            216,  // 8-byte disc + 208 body bytes needed (reserve_y at d[176..208])
        )?;

        let max = self.config.max_pools_per_dex;
        let min_liq = self.config.min_liquidity_sol;
        let mut count = 0usize;

        for (address, account) in accounts.into_iter().take(max) {
            match self.parse_meteora_dlmm(&address, &account) {
                Ok(pool) => {
                    if pool.liquidity_sol >= min_liq || pool.liquidity_sol == 0.0 {
                        let index_mint = if pool.quote_mint.to_string() == SOL_MINT_STR {
                            pool.token_mint.to_string()
                        } else {
                            pool.token_mint.to_string()
                        };
                        debug!("[DLMM] pool={} token={}", address, index_mint);
                        self.cache.add_pool(&index_mint, pool);
                        count += 1;
                    }
                }
                Err(e) => {
                    debug!("[DLMM] parse error for {}: {}", address, e);
                }
            }
        }
        Ok(count)
    }

    /// Parse a Meteora DLMM LbPair account.
    ///
    /// LbPair layout (after 8-byte discriminator at offset 0):
    ///   8    parameters (various, 32 bytes approx)
    ///   The mints are at offsets documented in the DLMM program IDL:
    ///   Offset 80:  token_x_mint (Pubkey, 32)
    ///   Offset 112: token_y_mint (Pubkey, 32)
    ///   Offset 144: reserve_x (Pubkey, 32) — token X vault
    ///   Offset 176: reserve_y (Pubkey, 32) — token Y vault
    fn parse_meteora_dlmm(&self, address: &Pubkey, account: &Account) -> Result<DiscoveredPool> {
        let data = &account.data;
        if data.len() < 208 {
            return Err(anyhow!("DLMM account too short: {} bytes", data.len()));
        }

        let mint_x = read_pubkey(data, 80)?;
        let mint_y = read_pubkey(data, 112)?;
        let vault_x = read_pubkey(data, 144)?;
        let vault_y = read_pubkey(data, 176)?;

        let sol_mint = Pubkey::from_str(SOL_MINT_STR)?;
        let (token_mint, quote_mint, token_vault, sol_vault) = if mint_x == sol_mint {
            (mint_y, mint_x, vault_y, vault_x)
        } else {
            (mint_x, mint_y, vault_x, vault_y)
        };

        Ok(DiscoveredPool {
            address: *address,
            dex: "meteora_dlmm".to_string(),
            token_mint,
            quote_mint,
            liquidity_sol: 0.0,
            fee_percent: 0.3,
            token_vault,
            sol_vault,
            discovered_at: current_ts(),
        })
    }

    // ── Pump.fun ──────────────────────────────────────────────────────────────

    /// Discover Pump.fun bonding curve accounts.
    fn discover_pump(&mut self) -> Result<usize> {
        let program_id = Pubkey::from_str(PUMP_PROGRAM)?;
        // Pump.fun uses no Anchor discriminator — filter purely by account size (300 bytes).
        // Parse reads up to byte 97 (complete flag), so fetch 97 bytes.
        let accounts = self.get_program_accounts_size_only(
            &program_id,
            PUMP_ACCOUNT_SIZE,
            97, // complete flag at byte 96, so we need 97 bytes
        )?;

        let max = self.config.max_pools_per_dex;
        let min_liq = self.config.min_liquidity_sol;
        let mut count = 0usize;

        for (address, account) in accounts.into_iter().take(max) {
            match self.parse_pump(&address, &account) {
                Ok(pool) => {
                    if pool.liquidity_sol >= min_liq || pool.liquidity_sol == 0.0 {
                        let index_mint = pool.token_mint.to_string();
                        debug!("[Pump] pool={} token={}", address, index_mint);
                        self.cache.add_pool(&index_mint, pool);
                        count += 1;
                    }
                }
                Err(e) => {
                    debug!("[Pump] parse error for {}: {}", address, e);
                }
            }
        }
        Ok(count)
    }

    /// Parse a Pump.fun bonding curve account.
    ///
    /// BondingCurve layout (no discriminator):
    ///   0    mint (Pubkey, 32)
    ///   32   associated_bonding_curve (Pubkey, 32) — token vault
    ///   64   virtual_sol_reserves (u64, 8)
    ///   72   virtual_token_reserves (u64, 8)
    ///   80   real_sol_reserves (u64, 8)
    ///   88   real_token_reserves (u64, 8)
    ///   96   complete (bool, 1)
    ///   ...
    fn parse_pump(&self, address: &Pubkey, account: &Account) -> Result<DiscoveredPool> {
        let data = &account.data;
        if data.len() < 97 {
            return Err(anyhow!("Pump account too short: {} bytes", data.len()));
        }

        let mint = read_pubkey(data, 0)?;
        let token_vault = read_pubkey(data, 32)?;
        let real_sol_reserves = u64::from_le_bytes(data[80..88].try_into()?);
        let complete = data[96] != 0;

        if complete {
            return Err(anyhow!("Pump curve complete, skipping"));
        }

        if mint == Pubkey::default() {
            return Err(anyhow!("Pump account has zeroed mint, skipping"));
        }

        let sol_vault = *address; // Pump uses the bonding curve PDA as SOL vault
        let sol_mint = Pubkey::from_str(SOL_MINT_STR)?;
        let liquidity_sol = real_sol_reserves as f64 / 1e9;

        Ok(DiscoveredPool {
            address: *address,
            dex: "pump".to_string(),
            token_mint: mint,
            quote_mint: sol_mint,
            liquidity_sol,
            fee_percent: 1.0,
            token_vault,
            sol_vault,
            discovered_at: current_ts(),
        })
    }

    // ── RPC Helpers ───────────────────────────────────────────────────────────

    /// Fetch program accounts filtered by discriminator (first 8 bytes) and optional exact size.
    ///
    /// Uses `dataSlice` to fetch only the first `data_slice_length` bytes of each account.
    /// This is critical for performance: Raydium V4 has ~70K accounts at 1664 bytes each
    /// = ~100MB of data. With dataSlice we only fetch the bytes needed to parse mints and
    /// vaults (~128-250 bytes per account), keeping the response under 10MB.
    ///
    /// Most RPC providers (Helius, QuickNode) allow getProgramAccounts with dataSlice even
    /// when they restrict full-data responses for programs with many accounts.
    fn get_program_accounts_filtered(
        &self,
        program_id: &Pubkey,
        discriminator: &[u8; 8],
        account_size: Option<u64>,
        data_slice_length: usize,
    ) -> Result<Vec<(Pubkey, Account)>> {
        let mut filters = vec![
            RpcFilterType::Memcmp(Memcmp::new(
                0,
                MemcmpEncodedBytes::Bytes(discriminator.to_vec()),
            )),
        ];

        if let Some(size) = account_size {
            filters.push(RpcFilterType::DataSize(size));
        }

        info!(
            "[PoolDiscovery] getProgramAccounts program={} disc={:02x}{:02x}{:02x}{:02x}... size_filter={:?} data_slice={}",
            program_id,
            discriminator[0], discriminator[1], discriminator[2], discriminator[3],
            account_size, data_slice_length
        );

        let config = RpcProgramAccountsConfig {
            filters: Some(filters),
            account_config: RpcAccountInfoConfig {
                commitment: Some(CommitmentConfig::confirmed()),
                // Only fetch the bytes needed to parse mints and vault addresses.
                // This reduces response size by 80-95% and avoids provider size limits.
                data_slice: Some(UiDataSliceConfig {
                    offset: 0,
                    length: data_slice_length,
                }),
                ..Default::default()
            },
            ..Default::default()
        };

        match self.rpc_client.get_program_accounts_with_config(program_id, config) {
            Ok(result) => {
                info!("[PoolDiscovery] getProgramAccounts for {} returned {} raw accounts", program_id, result.len());
                Ok(result)
            }
            Err(e) => {
                // Log the FULL error including the RPC error message so we can diagnose
                // provider-specific restrictions (e.g. "getProgramAccounts is disabled").
                warn!("[PoolDiscovery] getProgramAccounts FAILED for {}: {}", program_id, e);
                Err(anyhow!("getProgramAccounts error for {}: {}", program_id, e))
            }
        }
    }

    /// Fetch program accounts filtered only by data size (for non-Anchor programs like Pump.fun).
    ///
    /// Uses `dataSlice` to fetch only the first `data_slice_length` bytes per account.
    fn get_program_accounts_size_only(
        &self,
        program_id: &Pubkey,
        account_size: u64,
        data_slice_length: usize,
    ) -> Result<Vec<(Pubkey, Account)>> {
        info!(
            "[PoolDiscovery] getProgramAccounts(size-only) program={} size={} data_slice={}",
            program_id, account_size, data_slice_length
        );

        let config = RpcProgramAccountsConfig {
            filters: Some(vec![RpcFilterType::DataSize(account_size)]),
            account_config: RpcAccountInfoConfig {
                commitment: Some(CommitmentConfig::confirmed()),
                data_slice: Some(UiDataSliceConfig {
                    offset: 0,
                    length: data_slice_length,
                }),
                ..Default::default()
            },
            ..Default::default()
        };

        match self.rpc_client.get_program_accounts_with_config(program_id, config) {
            Ok(result) => {
                info!("[PoolDiscovery] getProgramAccounts(size-only) for {} returned {} raw accounts", program_id, result.len());
                Ok(result)
            }
            Err(e) => {
                warn!("[PoolDiscovery] getProgramAccounts(size-only) FAILED for {}: {}", program_id, e);
                Err(anyhow!("getProgramAccounts(size-only) error for {}: {}", program_id, e))
            }
        }
    }

    // ── Public Query Methods ──────────────────────────────────────────────────

    /// Get all discovered pools for a specific token mint.
    pub fn get_pools_for_token(&self, token_mint: &str) -> Vec<&DiscoveredPool> {
        self.cache
            .pools_by_token
            .get(token_mint)
            .map(|v| v.iter().collect())
            .unwrap_or_default()
    }

    /// Get all discovered pools, all tokens.
    pub fn get_all_pools(&self) -> Vec<&DiscoveredPool> {
        self.cache
            .pools_by_token
            .values()
            .flat_map(|v| v.iter())
            .collect()
    }

    /// Summarise discovery results to logs.
    pub fn log_summary(&self) {
        let total = self.cache.total_pools();
        info!("[PoolDiscovery] Summary: {} total pools across {} tokens",
            total, self.cache.pools_by_token.len());
        for (mint, pools) in &self.cache.pools_by_token {
            let short = &mint[..mint.len().min(8)];
            let by_dex: HashMap<&str, usize> = pools.iter().fold(HashMap::new(), |mut m, p| {
                *m.entry(p.dex.as_str()).or_insert(0) += 1;
                m
            });
            info!("  {}... : {} pools {:?}", short, pools.len(), by_dex);
        }
    }
}

// ── Utility helpers ───────────────────────────────────────────────────────────

fn read_pubkey(data: &[u8], offset: usize) -> Result<Pubkey> {
    if data.len() < offset + 32 {
        return Err(anyhow!("data too short for pubkey at offset {}", offset));
    }
    let mut bytes = [0u8; 32];
    bytes.copy_from_slice(&data[offset..offset + 32]);
    Ok(Pubkey::new_from_array(bytes))
}

fn current_ts() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cache_add_and_retrieve() {
        let mut cache = PoolDiscoveryCache::new();
        let pool = DiscoveredPool {
            address: Pubkey::new_unique(),
            dex: "Raydium".to_string(),
            token_mint: Pubkey::new_unique(),
            quote_mint: Pubkey::new_unique(),
            liquidity_sol: 100.0,
            fee_percent: 0.25,
            token_vault: Pubkey::new_unique(),
            sol_vault: Pubkey::new_unique(),
            discovered_at: 100,
        };
        let token_mint = "TokenABC";
        cache.add_pool(token_mint, pool.clone());
        assert_eq!(cache.get_pools(token_mint).unwrap().len(), 1);
    }

    #[test]
    fn test_liquid_pools_filter() {
        let mut cache = PoolDiscoveryCache::new();
        let token = "TokenABC";
        for liq in [50.0, 100.0, 200.0] {
            cache.add_pool(token, DiscoveredPool {
                address: Pubkey::new_unique(),
                dex: "Raydium".to_string(),
                token_mint: Pubkey::new_unique(),
                quote_mint: Pubkey::new_unique(),
                liquidity_sol: liq,
                fee_percent: 0.25,
                token_vault: Pubkey::new_unique(),
                sol_vault: Pubkey::new_unique(),
                discovered_at: 0,
            });
        }
        let liquid = cache.get_liquid_pools(token, 100.0);
        assert_eq!(liquid.len(), 2); // 100.0 and 200.0
    }

    #[test]
    fn test_top_pools_ordering() {
        let mut cache = PoolDiscoveryCache::new();
        let token = "TokenABC";
        for liq in [50.0, 300.0, 100.0, 200.0] {
            cache.add_pool(token, DiscoveredPool {
                address: Pubkey::new_unique(),
                dex: "Raydium".to_string(),
                token_mint: Pubkey::new_unique(),
                quote_mint: Pubkey::new_unique(),
                liquidity_sol: liq,
                fee_percent: 0.25,
                token_vault: Pubkey::new_unique(),
                sol_vault: Pubkey::new_unique(),
                discovered_at: 0,
            });
        }
        let top = cache.get_top_pools(token, 2);
        assert_eq!(top[0].liquidity_sol, 300.0);
        assert_eq!(top[1].liquidity_sol, 200.0);
    }

    #[test]
    fn test_pool_discovery_config_defaults() {
        let config = PoolDiscoveryConfig::default();
        assert!(config.enabled);
        assert_eq!(config.min_liquidity_sol, 5.0);
        assert_eq!(config.refresh_interval_minutes, 120);
        assert_eq!(config.max_pools_per_dex, 2000);
    }

    #[test]
    fn test_read_pubkey_bounds() {
        let data = vec![0u8; 40];
        assert!(read_pubkey(&data, 0).is_ok());
        assert!(read_pubkey(&data, 8).is_ok());
        assert!(read_pubkey(&data, 9).is_err()); // 9 + 32 = 41 > 40
    }
}
