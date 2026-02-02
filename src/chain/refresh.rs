use anyhow::Result;
use solana_client::rpc_client::RpcClient;
use solana_sdk::pubkey::Pubkey;
use std::sync::Arc;
use std::collections::HashMap;
use tracing::{info, warn};

use crate::chain::pools::{MintPoolData, Pool, PoolData};
use crate::dex::raydium::amm_info::RaydiumAmmInfo;
use crate::dex::raydium::cp_amm_info::RaydiumCpAmmInfo;
use crate::dex::raydium::clmm_info::RaydiumClmmInfo;
use crate::dex::pump::amm_info::PumpAmmInfo;
use crate::dex::meteora::dlmm_info::MeteoraDlmmInfo;
use crate::dex::meteora::dammv2_info::MeteoraDAmmV2Info;
use crate::dex::whirlpool::state::Whirlpool;

/// Cached reserve data for a pool
#[derive(Debug, Clone)]
pub struct PoolReserves {
    pub token_reserve: u64,
    pub sol_reserve: u64,
    pub last_updated_slot: u64,
}

/// Parsed Serum/OpenBook DEX market state
#[derive(Debug, Clone)]
pub struct SerumMarketState {
    pub bids: Pubkey,
    pub asks: Pubkey,
    pub event_queue: Pubkey,
    pub coin_vault: Pubkey,
    pub pc_vault: Pubkey,
    pub vault_signer: Pubkey,
}

impl SerumMarketState {
    /// Parse OpenBook DEX v3 market account data.
    /// Layout: 5 bytes padding + 8 byte account flags = 13 byte header, then:
    ///   own_address(32), vault_signer_nonce(8), coin_mint(32), pc_mint(32),
    ///   coin_vault(32), coin_deposits(8), coin_fees(8),
    ///   pc_vault(32), pc_deposits(8), pc_fees(8),
    ///   vault_signer_nonce is at offset 45,
    ///   coin_vault at offset 117, pc_vault at offset 165,
    ///   req_queue at offset 213, event_queue at offset 245,
    ///   bids at offset 277, asks at offset 309
    pub fn try_parse(data: &[u8], serum_program: &Pubkey, market: &Pubkey) -> Option<Self> {
        if data.len() < 341 {
            return None;
        }

        let d = data;

        macro_rules! read_pubkey_at {
            ($offset:expr) => {{
                let mut bytes = [0u8; 32];
                bytes.copy_from_slice(&d[$offset..$offset + 32]);
                Pubkey::new_from_array(bytes)
            }};
        }

        // vault_signer_nonce at offset 45 (u64 LE)
        let vault_signer_nonce = u64::from_le_bytes(
            d[45..53].try_into().ok()?
        );

        let coin_vault = read_pubkey_at!(117);
        let pc_vault = read_pubkey_at!(165);
        let event_queue = read_pubkey_at!(245);
        let bids = read_pubkey_at!(277);
        let asks = read_pubkey_at!(309);

        // Derive vault signer PDA
        let vault_signer = Pubkey::create_program_address(
            &[market.as_ref(), &vault_signer_nonce.to_le_bytes()],
            serum_program,
        ).ok()?;

        Some(Self {
            bids,
            asks,
            event_queue,
            coin_vault,
            pc_vault,
            vault_signer,
        })
    }
}

/// Deserialized pool state (DEX-specific data extracted from on-chain accounts)
#[derive(Debug, Clone)]
pub enum DeserializedPoolState {
    RaydiumAmm {
        coin_vault: Pubkey,
        pc_vault: Pubkey,
        coin_mint: Pubkey,
        pc_mint: Pubkey,
        trade_fee_numerator: u64,
        trade_fee_denominator: u64,
        nonce: u64,
        amm_open_orders: Pubkey,
        amm_target_orders: Pubkey,
        serum_market: Pubkey,
        serum_program_id: Pubkey,
    },
    RaydiumCpAmm {
        vault_0: Pubkey,
        vault_1: Pubkey,
        mint_0: Pubkey,
        mint_1: Pubkey,
    },
    RaydiumClmm {
        vault_0: Pubkey,
        vault_1: Pubkey,
        mint_0: Pubkey,
        mint_1: Pubkey,
        sqrt_price_x64: u128,
        liquidity: u128,
        tick_current: i32,
        tick_spacing: u16,
        fee_rate: u32,
        amm_config: Pubkey,
        observation_state: Pubkey,
    },
    Pump {
        virtual_sol_reserves: u64,
        virtual_token_reserves: u64,
        real_sol_reserves: u64,
        real_token_reserves: u64,
        complete: bool,
        mint: Pubkey,
    },
    MeteoraDlmm {
        reserve_x_vault: Pubkey,
        reserve_y_vault: Pubkey,
        token_x_mint: Pubkey,
        token_y_mint: Pubkey,
        active_id: i32,
        bin_step: u16,
        base_factor: u16,
    },
    MeteoraDAmmV2 {
        a_vault: Pubkey,
        b_vault: Pubkey,
        token_a_mint: Pubkey,
        token_b_mint: Pubkey,
        enabled: bool,
    },
    WhirlpoolState {
        vault_a: Pubkey,
        vault_b: Pubkey,
        mint_a: Pubkey,
        mint_b: Pubkey,
        sqrt_price: u128,
        liquidity: u128,
        fee_rate: u16,
        tick_current_index: i32,
        tick_spacing: u16,
    },
    Unknown,
}

/// Manages pool state refresh via RPC
pub struct PoolRefreshManager {
    rpc: Arc<RpcClient>,
    reserves: HashMap<Pubkey, PoolReserves>,
    pool_states: HashMap<Pubkey, DeserializedPoolState>,
    serum_markets: HashMap<Pubkey, SerumMarketState>,
}

impl PoolRefreshManager {
    pub fn new(rpc: Arc<RpcClient>) -> Self {
        Self {
            rpc,
            reserves: HashMap::new(),
            pool_states: HashMap::new(),
            serum_markets: HashMap::new(),
        }
    }

    /// Get cached reserves for a pool
    pub fn get_reserves(&self, pool_address: &Pubkey) -> Option<&PoolReserves> {
        self.reserves.get(pool_address)
    }

    /// Get deserialized pool state
    pub fn get_pool_state(&self, pool_address: &Pubkey) -> Option<&DeserializedPoolState> {
        self.pool_states.get(pool_address)
    }

    /// Get parsed Serum market state
    pub fn get_serum_market(&self, market_address: &Pubkey) -> Option<&SerumMarketState> {
        self.serum_markets.get(market_address)
    }

    /// Full refresh: deserialize pool accounts, fetch serum markets, then fetch vault balances
    pub fn refresh_all(&mut self, pool_data: &MintPoolData) -> Result<usize> {
        self.refresh_pool_states(pool_data)?;
        self.refresh_serum_markets();
        self.refresh_vault_balances(pool_data)
    }

    /// Deserialize pool accounts to extract vault addresses and on-chain state
    pub fn refresh_pool_states(&mut self, pool_data: &MintPoolData) -> Result<usize> {
        let pool_addresses: Vec<(Pubkey, String)> = pool_data.pools.iter()
            .map(|p| (*p.pool_address(), p.get_dex_name().to_string()))
            .collect();

        if pool_addresses.is_empty() {
            return Ok(0);
        }

        let mut refreshed = 0;
        let addrs: Vec<Pubkey> = pool_addresses.iter().map(|(a, _)| *a).collect();

        for chunk_start in (0..addrs.len()).step_by(100) {
            let chunk_end = (chunk_start + 100).min(addrs.len());
            let chunk = &addrs[chunk_start..chunk_end];

            match self.rpc.get_multiple_accounts(chunk) {
                Ok(accounts) => {
                    for (i, maybe_account) in accounts.iter().enumerate() {
                        let global_idx = chunk_start + i;
                        let (pool_addr, ref dex_name) = pool_addresses[global_idx];

                        if let Some(account) = maybe_account {
                            if account.data.len() <= 8 {
                                continue;
                            }

                            let state = self.deserialize_pool_account(
                                dex_name,
                                &account.data,
                            );

                            self.pool_states.insert(pool_addr, state);
                            refreshed += 1;
                        }
                    }
                }
                Err(e) => {
                    warn!("Failed to fetch pool states batch: {}", e);
                }
            }
        }

        Ok(refreshed)
    }

    /// Fetch and parse Serum market accounts for all Raydium AMM pools
    fn refresh_serum_markets(&mut self) {
        let mut markets_to_fetch: Vec<(Pubkey, Pubkey)> = Vec::new(); // (market, serum_program)

        for state in self.pool_states.values() {
            if let DeserializedPoolState::RaydiumAmm { serum_market, serum_program_id, .. } = state {
                if !self.serum_markets.contains_key(serum_market)
                    && *serum_market != Pubkey::default()
                {
                    markets_to_fetch.push((*serum_market, *serum_program_id));
                }
            }
        }

        if markets_to_fetch.is_empty() {
            return;
        }

        let addrs: Vec<Pubkey> = markets_to_fetch.iter().map(|(m, _)| *m).collect();

        for chunk in addrs.chunks(100) {
            match self.rpc.get_multiple_accounts(chunk) {
                Ok(accounts) => {
                    for (i, maybe_account) in accounts.iter().enumerate() {
                        if let Some(account) = maybe_account {
                            let (market_addr, serum_program) = markets_to_fetch[i];
                            if let Some(parsed) = SerumMarketState::try_parse(
                                &account.data,
                                &serum_program,
                                &market_addr,
                            ) {
                                self.serum_markets.insert(market_addr, parsed);
                            }
                        }
                    }
                }
                Err(e) => {
                    warn!("Failed to fetch serum market accounts: {}", e);
                }
            }
        }
    }

    /// Deserialize a single pool account based on DEX type
    fn deserialize_pool_account(&self, dex_name: &str, data: &[u8]) -> DeserializedPoolState {
        match dex_name {
            "Raydium" => {
                match RaydiumAmmInfo::try_deserialize(data) {
                    Ok(info) => DeserializedPoolState::RaydiumAmm {
                        coin_vault: info.pool_coin_token_account,
                        pc_vault: info.pool_pc_token_account,
                        coin_mint: info.coin_mint_address,
                        pc_mint: info.pc_mint_address,
                        trade_fee_numerator: info.trade_fee_numerator,
                        trade_fee_denominator: info.trade_fee_denominator,
                        nonce: info.nonce,
                        amm_open_orders: info.amm_open_orders,
                        amm_target_orders: info.amm_target_orders,
                        serum_market: info.serum_market,
                        serum_program_id: info.serum_program_id,
                    },
                    Err(_) => DeserializedPoolState::Unknown,
                }
            }
            "RaydiumCp" => {
                match RaydiumCpAmmInfo::try_deserialize(data) {
                    Ok(info) => DeserializedPoolState::RaydiumCpAmm {
                        vault_0: info.token_0_vault,
                        vault_1: info.token_1_vault,
                        mint_0: info.token_0_mint,
                        mint_1: info.token_1_mint,
                    },
                    Err(_) => DeserializedPoolState::Unknown,
                }
            }
            "RaydiumClmm" => {
                match RaydiumClmmInfo::try_deserialize(data) {
                    Ok(info) => DeserializedPoolState::RaydiumClmm {
                        vault_0: info.token_vault_0,
                        vault_1: info.token_vault_1,
                        mint_0: info.token_mint_0,
                        mint_1: info.token_mint_1,
                        sqrt_price_x64: info.sqrt_price_x64,
                        liquidity: info.liquidity,
                        tick_current: info.tick_current,
                        tick_spacing: info.tick_spacing,
                        fee_rate: info.trade_fee_rate,
                        amm_config: info.amm_config,
                        observation_state: info.observation_key,
                    },
                    Err(_) => DeserializedPoolState::Unknown,
                }
            }
            "Pump" => {
                match PumpAmmInfo::try_deserialize(data) {
                    Ok(info) => DeserializedPoolState::Pump {
                        virtual_sol_reserves: info.virtual_sol_reserves,
                        virtual_token_reserves: info.virtual_token_reserves,
                        real_sol_reserves: info.real_sol_reserves,
                        real_token_reserves: info.real_token_reserves,
                        complete: info.complete,
                        mint: info.mint,
                    },
                    Err(_) => DeserializedPoolState::Unknown,
                }
            }
            "DLMM" => {
                match MeteoraDlmmInfo::try_deserialize(data) {
                    Ok(info) => DeserializedPoolState::MeteoraDlmm {
                        reserve_x_vault: info.reserve_x,
                        reserve_y_vault: info.reserve_y,
                        token_x_mint: info.token_x_mint,
                        token_y_mint: info.token_y_mint,
                        active_id: info.active_id,
                        bin_step: info.bin_step,
                        base_factor: info.parameters.base_factor,
                    },
                    Err(_) => DeserializedPoolState::Unknown,
                }
            }
            "MeteoraDAmmV2" => {
                match MeteoraDAmmV2Info::try_deserialize(data) {
                    Ok(info) => DeserializedPoolState::MeteoraDAmmV2 {
                        a_vault: info.a_vault,
                        b_vault: info.b_vault,
                        token_a_mint: info.token_a_mint,
                        token_b_mint: info.token_b_mint,
                        enabled: info.enabled,
                    },
                    Err(_) => DeserializedPoolState::Unknown,
                }
            }
            "Whirlpool" => {
                match Whirlpool::try_deserialize(data) {
                    Ok(info) => DeserializedPoolState::WhirlpoolState {
                        vault_a: info.token_vault_a,
                        vault_b: info.token_vault_b,
                        mint_a: info.token_mint_a,
                        mint_b: info.token_mint_b,
                        sqrt_price: info.sqrt_price,
                        liquidity: info.liquidity,
                        fee_rate: info.fee_rate,
                        tick_current_index: info.tick_current_index,
                        tick_spacing: info.tick_spacing,
                    },
                    Err(_) => DeserializedPoolState::Unknown,
                }
            }
            _ => DeserializedPoolState::Unknown,
        }
    }

    /// Refresh vault token balances for all pools
    pub fn refresh_vault_balances(&mut self, pool_data: &MintPoolData) -> Result<usize> {
        let mut vault_addresses: Vec<Pubkey> = Vec::new();
        let mut vault_to_pool: Vec<(Pubkey, bool)> = Vec::new();

        for pool in &pool_data.pools {
            let pool_addr = *pool.pool_address();

            // Use deserialized state for vault addresses if available, otherwise use pool struct
            let (token_vault, sol_vault) = match self.pool_states.get(&pool_addr) {
                Some(DeserializedPoolState::RaydiumAmm { coin_vault, pc_vault, .. }) => {
                    (*coin_vault, *pc_vault)
                }
                Some(DeserializedPoolState::RaydiumCpAmm { vault_0, vault_1, .. }) => {
                    (*vault_0, *vault_1)
                }
                Some(DeserializedPoolState::RaydiumClmm { vault_0, vault_1, .. }) => {
                    (*vault_0, *vault_1)
                }
                Some(DeserializedPoolState::MeteoraDlmm { reserve_x_vault, reserve_y_vault, .. }) => {
                    (*reserve_x_vault, *reserve_y_vault)
                }
                Some(DeserializedPoolState::MeteoraDAmmV2 { a_vault, b_vault, .. }) => {
                    (*a_vault, *b_vault)
                }
                Some(DeserializedPoolState::WhirlpoolState { vault_a, vault_b, .. }) => {
                    (*vault_a, *vault_b)
                }
                _ => {
                    // Fallback to pool struct vault addresses
                    (*pool.token_vault(), *pool.sol_vault())
                }
            };

            vault_addresses.push(token_vault);
            vault_to_pool.push((pool_addr, true));

            vault_addresses.push(sol_vault);
            vault_to_pool.push((pool_addr, false));
        }

        if vault_addresses.is_empty() {
            return Ok(0);
        }

        let mut balances: HashMap<Pubkey, u64> = HashMap::new();

        for chunk in vault_addresses.chunks(100) {
            match self.rpc.get_multiple_accounts(chunk) {
                Ok(accounts) => {
                    for (i, maybe_account) in accounts.iter().enumerate() {
                        if let Some(account) = maybe_account {
                            // Parse SPL token account balance (offset 64, 8 bytes LE u64)
                            if account.data.len() >= 72 {
                                let amount = u64::from_le_bytes(
                                    account.data[64..72].try_into().unwrap_or([0u8; 8])
                                );
                                balances.insert(chunk[i], amount);
                            }
                        }
                    }
                }
                Err(e) => {
                    warn!("Failed to fetch vault accounts batch: {}", e);
                }
            }
        }

        // Map balances back to pool reserves
        for (i, (pool_addr, is_token)) in vault_to_pool.iter().enumerate() {
            let vault_addr = vault_addresses[i];
            if let Some(&balance) = balances.get(&vault_addr) {
                let entry = self.reserves.entry(*pool_addr).or_insert(PoolReserves {
                    token_reserve: 0,
                    sol_reserve: 0,
                    last_updated_slot: 0,
                });

                if *is_token {
                    entry.token_reserve = balance;
                } else {
                    entry.sol_reserve = balance;
                }
            }
        }

        // For Pump pools, override reserves from deserialized state (uses virtual reserves)
        for pool in &pool_data.pools {
            let pool_addr = *pool.pool_address();
            if let Some(DeserializedPoolState::Pump {
                real_sol_reserves, real_token_reserves, complete, ..
            }) = self.pool_states.get(&pool_addr) {
                if !complete {
                    let entry = self.reserves.entry(pool_addr).or_insert(PoolReserves {
                        token_reserve: 0,
                        sol_reserve: 0,
                        last_updated_slot: 0,
                    });
                    entry.token_reserve = *real_token_reserves;
                    entry.sol_reserve = *real_sol_reserves;
                }
            }
        }

        // Update slot
        if let Ok(slot) = self.rpc.get_slot() {
            for reserve in self.reserves.values_mut() {
                reserve.last_updated_slot = slot;
            }
        }

        Ok(self.reserves.len())
    }

    /// Apply a raw WebSocket account update to the pool state cache.
    /// This reuses the same DEX-specific deserialization as `refresh_pool_states`.
    pub fn apply_account_update(&mut self, pool_address: Pubkey, dex_name: &str, data: &[u8]) {
        if data.len() <= 8 {
            return;
        }
        let state = self.deserialize_pool_account(dex_name, data);
        self.pool_states.insert(pool_address, state);
    }

    /// Start an async refresh loop
    pub async fn start_refresh_loop(
        rpc: Arc<RpcClient>,
        pool_data: Arc<tokio::sync::RwLock<MintPoolData>>,
        interval_ms: u64,
    ) {
        let mut manager = PoolRefreshManager::new(rpc);
        let mut interval = tokio::time::interval(
            tokio::time::Duration::from_millis(interval_ms)
        );

        loop {
            interval.tick().await;

            let data = pool_data.read().await;
            match manager.refresh_all(&data) {
                Ok(count) => {
                    if count > 0 {
                        info!("Refreshed {} pool reserves", count);
                    }
                }
                Err(e) => {
                    warn!("Pool refresh error: {}", e);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_pool_refresh_manager_creation() {
        let rpc = Arc::new(RpcClient::new("https://api.mainnet-beta.solana.com".to_string()));
        let manager = PoolRefreshManager::new(rpc);
        assert!(manager.reserves.is_empty());
        assert!(manager.pool_states.is_empty());
        assert!(manager.serum_markets.is_empty());
    }

    #[test]
    fn test_pool_reserves_default() {
        let reserves = PoolReserves {
            token_reserve: 1000,
            sol_reserve: 500,
            last_updated_slot: 12345,
        };
        assert_eq!(reserves.token_reserve, 1000);
        assert_eq!(reserves.sol_reserve, 500);
    }

    #[test]
    fn test_deserialized_pool_state_variants() {
        let state = DeserializedPoolState::Pump {
            virtual_sol_reserves: 30_000_000_000,
            virtual_token_reserves: 1_000_000_000_000,
            real_sol_reserves: 10_000_000_000,
            real_token_reserves: 500_000_000_000,
            complete: false,
            mint: Pubkey::default(),
        };
        match state {
            DeserializedPoolState::Pump { complete, .. } => assert!(!complete),
            _ => panic!("Wrong variant"),
        }
    }

    #[test]
    fn test_raydium_amm_state_has_serum_fields() {
        let state = DeserializedPoolState::RaydiumAmm {
            coin_vault: Pubkey::new_unique(),
            pc_vault: Pubkey::new_unique(),
            coin_mint: Pubkey::new_unique(),
            pc_mint: Pubkey::new_unique(),
            trade_fee_numerator: 25,
            trade_fee_denominator: 10000,
            nonce: 254,
            amm_open_orders: Pubkey::new_unique(),
            amm_target_orders: Pubkey::new_unique(),
            serum_market: Pubkey::new_unique(),
            serum_program_id: Pubkey::new_unique(),
        };
        match state {
            DeserializedPoolState::RaydiumAmm { nonce, .. } => assert_eq!(nonce, 254),
            _ => panic!("Wrong variant"),
        }
    }

    #[test]
    fn test_whirlpool_state_has_tick_info() {
        let state = DeserializedPoolState::WhirlpoolState {
            vault_a: Pubkey::new_unique(),
            vault_b: Pubkey::new_unique(),
            mint_a: Pubkey::new_unique(),
            mint_b: Pubkey::new_unique(),
            sqrt_price: 1u128 << 64,
            liquidity: 1_000_000,
            fee_rate: 3000,
            tick_current_index: 0,
            tick_spacing: 64,
        };
        match state {
            DeserializedPoolState::WhirlpoolState { tick_spacing, .. } => assert_eq!(tick_spacing, 64),
            _ => panic!("Wrong variant"),
        }
    }
}
