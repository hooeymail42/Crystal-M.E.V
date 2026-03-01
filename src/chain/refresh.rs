#![allow(dead_code)]
use anyhow::Result;
use solana_client::rpc_client::RpcClient;
use solana_sdk::pubkey::Pubkey;
use std::sync::Arc;
use std::collections::HashMap;
use tracing::{debug, info, warn};

use crate::chain::pools::{MintPoolData, PoolData};
use crate::dex::raydium::amm_info::RaydiumAmmInfo;
use crate::dex::raydium::cp_amm_info::RaydiumCpAmmInfo;
use crate::dex::raydium::clmm_info::RaydiumClmmInfo;
use crate::dex::pump::amm_info::PumpAmmInfo;
use crate::dex::meteora::dlmm_info::MeteoraDlmmInfo;
use crate::dex::meteora::dammv2_info::MeteoraDAmmV2Info;
use crate::dex::whirlpool::state::Whirlpool;
use crate::dex::phoenix::state::PhoenixMarketState;
use crate::dex::lifinity::amm_info::LifinityAmmInfo;
use crate::dex::heaven::amm_info::HeavenAmmInfo;

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
        /// Bitmap extension account key if the pool has one; None → pass program_id as placeholder.
        bitmap_extension: Option<Pubkey>,
    },
    MeteoraDAmmV2 {
        a_vault: Pubkey,
        b_vault: Pubkey,
        /// Pool's LP-token accounts inside each vault (SPL token accounts, balance at offset 64)
        a_vault_lp: Pubkey,
        b_vault_lp: Pubkey,
        token_a_mint: Pubkey,
        token_b_mint: Pubkey,
        /// Admin fee SPL token accounts (direction-dependent in swap instruction)
        admin_token_a_fee: Pubkey,
        admin_token_b_fee: Pubkey,
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
    Phoenix {
        base_vault: Pubkey,
        quote_vault: Pubkey,
        base_mint: Pubkey,
        quote_mint: Pubkey,
        taker_fee_bps: u16,
        best_bid_price: u64,
        best_ask_price: u64,
    },
    Lifinity {
        token_a_vault: Pubkey,
        token_b_vault: Pubkey,
        token_a_mint: Pubkey,
        token_b_mint: Pubkey,
    },
    Heaven {
        virtual_sol_reserves: u64,
        virtual_token_reserves: u64,
        real_sol_reserves: u64,
        real_token_reserves: u64,
        complete: bool,
        mint: Pubkey,
        launch_timestamp: i64,
    },
    Unknown,
}

/// Vault-internal accounts needed to build MeteoraDAmmV2 swap instructions.
/// Populated during `refresh_vault_balances` Step 1 (vault account fetch).
#[derive(Debug, Clone)]
pub struct DammV2SwapAccounts {
    /// SPL token account inside a_vault; at vault.data[19..51]
    pub a_token_vault: Pubkey,
    /// SPL token account inside b_vault; at vault.data[19..51]
    pub b_token_vault: Pubkey,
    /// LP mint for a_vault; at vault.data[115..147]
    pub a_vault_lp_mint: Pubkey,
    /// LP mint for b_vault; at vault.data[115..147]
    pub b_vault_lp_mint: Pubkey,
}

/// Manages pool state refresh via RPC
pub struct PoolRefreshManager {
    rpc: Arc<RpcClient>,
    reserves: HashMap<Pubkey, PoolReserves>,
    pool_states: HashMap<Pubkey, DeserializedPoolState>,
    serum_markets: HashMap<Pubkey, SerumMarketState>,
    /// Vault-internal accounts required to build MeteoraDAmmV2 swap instructions.
    damm_v2_swap_accounts: HashMap<Pubkey, DammV2SwapAccounts>,
    refresh_count: u64,
    /// Pools already warned as Unknown — suppress repeated log spam (warn once per pool).
    warned_unknown_pools: std::collections::HashSet<Pubkey>,
}

impl PoolRefreshManager {
    pub fn new(rpc: Arc<RpcClient>) -> Self {
        Self {
            rpc,
            reserves: HashMap::new(),
            pool_states: HashMap::new(),
            serum_markets: HashMap::new(),
            damm_v2_swap_accounts: HashMap::new(),
            refresh_count: 0,
            warned_unknown_pools: std::collections::HashSet::new(),
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

    /// Get vault-internal swap accounts for a MeteoraDAmmV2 pool
    pub fn get_damm_v2_swap_accounts(&self, pool_address: &Pubkey) -> Option<&DammV2SwapAccounts> {
        self.damm_v2_swap_accounts.get(pool_address)
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

                        match maybe_account {
                            None => {
                                // Account does not exist on-chain (bad address in .env or not yet on-chain)
                                debug!("[PoolState] {} pool {} → account NOT FOUND on-chain",
                                    dex_name, pool_addr);
                            }
                            Some(account) => {
                                if account.data.len() <= 8 {
                                    warn!("[PoolState] {} pool {} → data too short: {} bytes (expected >8)",
                                        dex_name, pool_addr, account.data.len());
                                } else {
                                    let state = self.deserialize_pool_account_logged(dex_name, &account.data, &pool_addr);
                                    let is_unknown = matches!(state, DeserializedPoolState::Unknown);
                                    self.pool_states.insert(pool_addr, state);
                                    if !is_unknown {
                                        refreshed += 1;
                                    }
                                }
                            }
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

    /// Deserialize a single pool account, logging detailed errors when it fails.
    /// Unknown-state warnings are emitted only once per pool to avoid log spam across refreshes.
    fn deserialize_pool_account_logged(&mut self, dex_name: &str, data: &[u8], pool_addr: &Pubkey) -> DeserializedPoolState {
        let state = self.deserialize_pool_account(dex_name, data);
        if matches!(state, DeserializedPoolState::Unknown) {
            // Warn only on the first encounter; subsequent refreshes are silent.
            if self.warned_unknown_pools.insert(*pool_addr) {
                warn!("[PoolState] {} pool {} → deserialization returned Unknown (data_len={}). \
                       This means the on-chain account layout did not match the expected struct. \
                       Vault reads will use placeholder addresses and reserves will be 0.",
                    dex_name, pool_addr, data.len());
                // Log first 32 bytes as hex to help diagnose layout mismatches
                let preview_len = data.len().min(32);
                let hex: String = data[..preview_len].iter().map(|b| format!("{:02x}", b)).collect::<Vec<_>>().join(" ");
                debug!("[PoolState] {} {} first {} bytes: {}", dex_name, pool_addr, preview_len, hex);
            }
        } else {
            debug!("[PoolState] {} pool {} → deserialized OK (data_len={})", dex_name, pool_addr, data.len());
        }
        state
    }

    /// Deserialize a single pool account based on DEX type
    fn deserialize_pool_account(&self, dex_name: &str, data: &[u8]) -> DeserializedPoolState {
        match dex_name {
            "Raydium" => {
                match RaydiumAmmInfo::try_deserialize(data) {
                    Ok(info) => {
                        DeserializedPoolState::RaydiumAmm {
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
                    }},
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
                        bitmap_extension: None, // populated lazily if pool needs extension
                    },
                    Err(_) => DeserializedPoolState::Unknown,
                }
            }
            "MeteoraDAmmV2" => {
                match MeteoraDAmmV2Info::try_deserialize(data) {
                    Ok(info) => DeserializedPoolState::MeteoraDAmmV2 {
                        a_vault: info.a_vault,
                        b_vault: info.b_vault,
                        a_vault_lp: info.a_vault_lp,
                        b_vault_lp: info.b_vault_lp,
                        token_a_mint: info.token_a_mint,
                        token_b_mint: info.token_b_mint,
                        admin_token_a_fee: info.admin_token_a_fee,
                        admin_token_b_fee: info.admin_token_b_fee,
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
            "Phoenix" => {
                match PhoenixMarketState::try_deserialize(data) {
                    Ok(info) => DeserializedPoolState::Phoenix {
                        base_vault: info.base_vault,
                        quote_vault: info.quote_vault,
                        base_mint: info.base_mint,
                        quote_mint: info.quote_mint,
                        taker_fee_bps: info.taker_fee_bps,
                        best_bid_price: info.best_bid_price,
                        best_ask_price: info.best_ask_price,
                    },
                    Err(_) => DeserializedPoolState::Unknown,
                }
            }
            "Lifinity" => {
                match LifinityAmmInfo::try_deserialize(data) {
                    Ok(info) => DeserializedPoolState::Lifinity {
                        token_a_vault: info.token_a_vault,
                        token_b_vault: info.token_b_vault,
                        token_a_mint: info.token_a_mint,
                        token_b_mint: info.token_b_mint,
                    },
                    Err(_) => DeserializedPoolState::Unknown,
                }
            }
            "Heaven" => {
                match HeavenAmmInfo::try_deserialize(data) {
                    Ok(info) => DeserializedPoolState::Heaven {
                        virtual_sol_reserves: info.virtual_sol_reserves,
                        virtual_token_reserves: info.virtual_token_reserves,
                        real_sol_reserves: info.real_sol_reserves,
                        real_token_reserves: info.real_token_reserves,
                        complete: info.complete,
                        mint: info.mint,
                        launch_timestamp: info.launch_timestamp,
                    },
                    Err(_) => DeserializedPoolState::Unknown,
                }
            }
            _ => DeserializedPoolState::Unknown,
        }
    }

    /// Refresh vault token balances for all pools
    pub fn refresh_vault_balances(&mut self, pool_data: &MintPoolData) -> Result<usize> {
        self.refresh_count += 1;
        // Only log diagnostics on first refresh and every 500 refreshes to reduce noise
        let should_log_diag = self.refresh_count == 1 || self.refresh_count % 500 == 0;
        let mut vault_addresses: Vec<Pubkey> = Vec::new();
        let mut vault_to_pool: Vec<(Pubkey, bool)> = Vec::new();

        for pool in &pool_data.pools {
            let pool_addr = *pool.pool_address();

            // MeteoraDAmmV2 a_vault/b_vault are Meteora vault program accounts, NOT SPL token
            // accounts. Reading them as SPL token accounts (offset 64) returns garbage bytes
            // that produce wildly incorrect reserve values and phantom arb opportunities.
            // Skip vault balance fetching entirely — reserves stay 0 and the pool is filtered
            // out by the opportunity detector until proper DAMM V2 balance reading is added.
            if pool.get_dex_name() == "MeteoraDAmmV2" {
                continue;
            }

            // SOL mint address used to determine which vault is the SOL side
            let sol_mint = Pubkey::try_from("So11111111111111111111111111111111111111112").unwrap();

            // Use deserialized state for vault addresses if available, otherwise use pool struct.
            // For CLMM/Whirlpool pools the ordering of vault_0/vault_1 (or vault_a/vault_b)
            // depends on which mint was registered first on-chain. We check the mint addresses
            // from the deserialized state so that (token_vault, sol_vault) is always correct.
            let (token_vault, sol_vault) = match self.pool_states.get(&pool_addr) {
                Some(DeserializedPoolState::RaydiumAmm { coin_vault, pc_vault, coin_mint, .. }) => {
                    // Raydium V4: coin/pc ordering is not standardised — coin may be SOL or token.
                    // Always put token_vault first, sol_vault second.
                    if *coin_mint == sol_mint {
                        (*pc_vault, *coin_vault) // coin=SOL → token_vault=pc, sol_vault=coin
                    } else {
                        (*coin_vault, *pc_vault) // coin=token → token_vault=coin, sol_vault=pc
                    }
                }
                Some(DeserializedPoolState::RaydiumCpAmm { vault_0, vault_1, mint_0, .. }) => {
                    // mint_0 is token_0; if it's SOL then vault_0=sol, vault_1=token
                    if *mint_0 == sol_mint {
                        (*vault_1, *vault_0)
                    } else {
                        (*vault_0, *vault_1)
                    }
                }
                Some(DeserializedPoolState::RaydiumClmm { vault_0, vault_1, mint_0, .. }) => {
                    // Same ordering logic for Raydium CLMM
                    if *mint_0 == sol_mint {
                        (*vault_1, *vault_0) // vault_1 = token, vault_0 = SOL
                    } else {
                        (*vault_0, *vault_1) // vault_0 = token, vault_1 = SOL
                    }
                }
                Some(DeserializedPoolState::MeteoraDlmm { reserve_x_vault, reserve_y_vault, token_x_mint, token_y_mint, .. }) => {
                    // Skip non-SOL pairs (e.g., USDC/USDT) — neither token is WSOL
                    if *token_x_mint != sol_mint && *token_y_mint != sol_mint {
                        continue;
                    }
                    if *token_x_mint == sol_mint {
                        (*reserve_y_vault, *reserve_x_vault)
                    } else {
                        (*reserve_x_vault, *reserve_y_vault)
                    }
                }
                Some(DeserializedPoolState::WhirlpoolState { vault_a, vault_b, mint_a, .. }) => {
                    // Whirlpool: mint_a / mint_b — check which is SOL
                    if *mint_a == sol_mint {
                        (*vault_b, *vault_a) // vault_b = token, vault_a = SOL
                    } else {
                        (*vault_a, *vault_b) // vault_a = token, vault_b = SOL
                    }
                }
                Some(DeserializedPoolState::Phoenix { base_vault, quote_vault, base_mint, .. }) => {
                    if *base_mint == sol_mint {
                        (*quote_vault, *base_vault)
                    } else {
                        (*base_vault, *quote_vault)
                    }
                }
                Some(DeserializedPoolState::Lifinity { token_a_vault, token_b_vault, token_a_mint, .. }) => {
                    if *token_a_mint == sol_mint {
                        (*token_b_vault, *token_a_vault)
                    } else {
                        (*token_a_vault, *token_b_vault)
                    }
                }
                Some(DeserializedPoolState::Heaven { .. }) => {
                    // Heaven uses virtual reserves from deserialized state, fallback to pool struct vaults
                    (*pool.token_vault(), *pool.sol_vault())
                }
                Some(DeserializedPoolState::Pump { .. }) => {
                    // Pump reserves are overridden at the end of this function from deserialized state.
                    // Vault SPL token accounts are not fetched for Pump — reserves come from
                    // the virtual_sol_reserves / virtual_token_reserves fields directly.
                    // Use pool struct vaults as placeholder; the override block below will set real values.
                    (*pool.token_vault(), *pool.sol_vault())
                }
                Some(DeserializedPoolState::MeteoraDAmmV2 { .. }) => {
                    unreachable!("MeteoraDAmmV2 is skipped by the early continue above");
                }
                Some(DeserializedPoolState::Unknown) => {
                    // Deserialization was attempted but failed (layout mismatch).
                    // See [PoolState] WARN above for the exact error.
                    // Fallback vault addresses are placeholder (Pubkey::new_unique) and will return
                    // no balance, so reserves stay 0 and this pool is filtered by the opportunity detector.
                    debug!("[VaultRefresh] {} pool={} skipping vault fetch — deserialization Unknown",
                        pool.get_dex_name(), pool_addr);
                    continue; // skip rather than fetching placeholder addresses
                }
                None => {
                    // pool_states has no entry yet (first loop, or RPC fetch failed).
                    debug!("[VaultRefresh] {} pool={} skipping vault fetch — no state cached yet",
                        pool.get_dex_name(), pool_addr);
                    continue; // skip rather than fetching placeholder addresses
                }
            };

            if should_log_diag {
                info!("[VaultDiag] pool={} dex={} token_vault={} sol_vault={}",
                    pool_addr, pool.get_dex_name(), token_vault, sol_vault);
            }

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

        // Log final reserves for diagnostics (throttled)
        if should_log_diag {
            for (pool_addr, _) in vault_to_pool.iter().step_by(2) {
                if let Some(r) = self.reserves.get(pool_addr) {
                    info!("[ReserveDiag] pool={} token={} sol={} (sol={:.4})",
                        pool_addr, r.token_reserve, r.sol_reserve,
                        r.sol_reserve as f64 / 1e9);
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

            // For Heaven pools, override reserves from deserialized state (uses virtual reserves)
            if let Some(DeserializedPoolState::Heaven {
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

        // ── MeteoraDAmmV2: compute pool reserves via LP-token ratio ──────────────
        // Meteora Vault accounts are SHARED across multiple pools; `totalAmount` at [11..19]
        // reflects the whole vault, not just our pool. The pool's real reserve is:
        //   pool_reserve = vault_totalAmount * (pool_vault_lp_balance / vault_lp_supply)
        //
        // Vault layout (Meteora Vault Program 24Uqj9JCLxUeoC3hGfh5W3s9FM9uCHDS2SG3LYwBpyTi):
        //   [8]      enabled (u8)
        //   [9]      vaultBump (u8)
        //   [10]     tokenVaultBump (u8)
        //   [11..19] totalAmount (u64 LE)
        //   [115..147] lpMint (Pubkey)
        //
        // SPL Mint account: supply at [36..44] (u64 LE).
        // SPL Token account: amount at [64..72] (u64 LE).
        {
            let sol_mint = Pubkey::try_from("So11111111111111111111111111111111111111112").unwrap();

            struct DammV2Meta {
                pool_addr: Pubkey,
                a_vault:    Pubkey,
                b_vault:    Pubkey,
                a_vault_lp: Pubkey, // pool's LP-token SPL account inside a_vault
                b_vault_lp: Pubkey, // pool's LP-token SPL account inside b_vault
                a_is_sol:   bool,
            }

            let mut meta: Vec<DammV2Meta> = Vec::new();
            for pool in &pool_data.pools {
                let pool_addr = *pool.pool_address();
                if let Some(DeserializedPoolState::MeteoraDAmmV2 {
                    a_vault, b_vault, a_vault_lp, b_vault_lp, token_a_mint, token_b_mint, enabled, ..
                }) = self.pool_states.get(&pool_addr)
                {
                    if !enabled { continue; }
                    if *token_a_mint != sol_mint && *token_b_mint != sol_mint { continue; }
                    let a_is_sol = *token_a_mint == sol_mint;
                    meta.push(DammV2Meta {
                        pool_addr,
                        a_vault: *a_vault,
                        b_vault: *b_vault,
                        a_vault_lp: *a_vault_lp,
                        b_vault_lp: *b_vault_lp,
                        a_is_sol,
                    });
                }
            }

            if !meta.is_empty() {
                // ── Step 1: fetch vault accounts → totalAmount + lpMint ──────────
                let vault_keys: Vec<Pubkey> = meta.iter()
                    .flat_map(|m| [m.a_vault, m.b_vault])
                    .collect();

                // vault_key → (totalAmount, lp_mint, token_vault)
                // token_vault at [19..51], totalAmount at [11..19], lpMint at [115..147]
                let mut vault_info: HashMap<Pubkey, (u64, Pubkey, Pubkey)> = HashMap::new();
                for chunk in vault_keys.chunks(100) {
                    match self.rpc.get_multiple_accounts(chunk) {
                        Ok(accounts) => {
                            for (i, maybe_acct) in accounts.iter().enumerate() {
                                if let Some(acct) = maybe_acct {
                                    if acct.data.len() >= 147 {
                                        let total = u64::from_le_bytes(
                                            acct.data[11..19].try_into().unwrap_or([0u8; 8])
                                        );
                                        let mut tv_bytes = [0u8; 32];
                                        tv_bytes.copy_from_slice(&acct.data[19..51]);
                                        let token_vault = Pubkey::new_from_array(tv_bytes);
                                        let mut lp_bytes = [0u8; 32];
                                        lp_bytes.copy_from_slice(&acct.data[115..147]);
                                        let lp_mint = Pubkey::new_from_array(lp_bytes);
                                        vault_info.insert(chunk[i], (total, lp_mint, token_vault));
                                    }
                                }
                            }
                        }
                        Err(e) => warn!("[MeteoraDAmmV2] Vault fetch failed: {}", e),
                    }
                }

                // ── Step 2: fetch LP mint accounts → total supply ─────────────
                let lp_mints: Vec<Pubkey> = vault_info.values()
                    .map(|(_, lp, _)| *lp)
                    .collect::<std::collections::HashSet<_>>()
                    .into_iter().collect();

                // lp_mint → supply
                let mut lp_supply: HashMap<Pubkey, u64> = HashMap::new();
                for chunk in lp_mints.chunks(100) {
                    match self.rpc.get_multiple_accounts(chunk) {
                        Ok(accounts) => {
                            for (i, maybe_acct) in accounts.iter().enumerate() {
                                if let Some(acct) = maybe_acct {
                                    if acct.data.len() >= 44 {
                                        let supply = u64::from_le_bytes(
                                            acct.data[36..44].try_into().unwrap_or([0u8; 8])
                                        );
                                        lp_supply.insert(chunk[i], supply);
                                    }
                                }
                            }
                        }
                        Err(e) => warn!("[MeteoraDAmmV2] LP mint fetch failed: {}", e),
                    }
                }

                // ── Step 3: fetch vault_lp SPL accounts → pool's LP balance ──
                let lp_acct_keys: Vec<Pubkey> = meta.iter()
                    .flat_map(|m| [m.a_vault_lp, m.b_vault_lp])
                    .collect();

                // vault_lp_acct → pool's LP balance
                let mut lp_balances: HashMap<Pubkey, u64> = HashMap::new();
                for chunk in lp_acct_keys.chunks(100) {
                    match self.rpc.get_multiple_accounts(chunk) {
                        Ok(accounts) => {
                            for (i, maybe_acct) in accounts.iter().enumerate() {
                                if let Some(acct) = maybe_acct {
                                    if acct.data.len() >= 72 {
                                        let bal = u64::from_le_bytes(
                                            acct.data[64..72].try_into().unwrap_or([0u8; 8])
                                        );
                                        lp_balances.insert(chunk[i], bal);
                                    }
                                }
                            }
                        }
                        Err(e) => warn!("[MeteoraDAmmV2] LP token account fetch failed: {}", e),
                    }
                }

                // ── Step 4: compute pool reserves via ratio, cache swap accounts ──
                for m in &meta {
                    let compute_reserve = |vault: Pubkey, lp_acct: Pubkey| -> Option<u64> {
                        let (total, lp_mint, _) = vault_info.get(&vault)?;
                        let supply = *lp_supply.get(lp_mint)?;
                        let balance = *lp_balances.get(&lp_acct)?;
                        if supply == 0 { return None; }
                        // Use u128 to avoid overflow (totalAmount can be ~10^13)
                        let reserve = (*total as u128) * (balance as u128) / (supply as u128);
                        Some(reserve as u64)
                    };

                    let a_reserve = match compute_reserve(m.a_vault, m.a_vault_lp) {
                        Some(v) => v,
                        None => continue,
                    };
                    let b_reserve = match compute_reserve(m.b_vault, m.b_vault_lp) {
                        Some(v) => v,
                        None => continue,
                    };

                    if a_reserve == 0 || b_reserve == 0 { continue; }

                    // Sanity guard: reject if either side exceeds 1M SOL equivalent
                    let max_sane = 1_000_000u64 * 1_000_000_000u64;
                    if a_reserve > max_sane || b_reserve > max_sane { continue; }

                    let (token_reserve, sol_reserve) = if m.a_is_sol {
                        (b_reserve, a_reserve)
                    } else {
                        (a_reserve, b_reserve)
                    };

                    let entry = self.reserves.entry(m.pool_addr).or_insert(PoolReserves {
                        token_reserve: 0,
                        sol_reserve: 0,
                        last_updated_slot: 0,
                    });
                    entry.token_reserve = token_reserve;
                    entry.sol_reserve = sol_reserve;

                    // Cache vault-internal accounts needed for swap instruction building
                    if let (Some((_, a_lp_mint, a_tv)), Some((_, b_lp_mint, b_tv))) = (
                        vault_info.get(&m.a_vault),
                        vault_info.get(&m.b_vault),
                    ) {
                        self.damm_v2_swap_accounts.insert(m.pool_addr, DammV2SwapAccounts {
                            a_token_vault: *a_tv,
                            b_token_vault: *b_tv,
                            a_vault_lp_mint: *a_lp_mint,
                            b_vault_lp_mint: *b_lp_mint,
                        });
                    }

                    if should_log_diag {
                        info!("[MeteoraDAmmV2] pool={} token={:.4} sol={:.4} (LP share={:.3}%)",
                            m.pool_addr,
                            token_reserve as f64 / 1e9,
                            sol_reserve as f64 / 1e9,
                            lp_balances.get(&m.a_vault_lp).copied().unwrap_or(0) as f64
                                / lp_supply.get(&vault_info.get(&m.a_vault).map(|(_, lp, _)| *lp).unwrap_or_default()).copied().unwrap_or(1) as f64
                                * 100.0);
                    }
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
