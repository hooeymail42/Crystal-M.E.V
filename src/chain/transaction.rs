use anyhow::{Result, anyhow};
use rand::Rng;
use solana_client::rpc_client::RpcClient;
use solana_sdk::{
    address_lookup_table_account::AddressLookupTableAccount,
    compute_budget::ComputeBudgetInstruction,
    hash::Hash,
    instruction::{AccountMeta, Instruction},
    message::{VersionedMessage, v0},
    pubkey::Pubkey,
    signature::{Keypair, Signer},
    system_instruction,
    transaction::VersionedTransaction,
};
use spl_associated_token_account::get_associated_token_address;
use std::sync::Arc;
use std::str::FromStr;
use tracing::{info, warn};

use crate::chain::opportunity_detector::{ArbitrageOpportunity, PathStep};
use crate::chain::refresh::{DeserializedPoolState, PoolRefreshManager};
use crate::dex::raydium::constants::{raydium_amm_program_id, raydium_cp_amm_program_id, raydium_clmm_program_id};
use crate::dex::pump::constants::pump_program_id;
use crate::dex::meteora::constants::{meteora_dlmm_program_id, meteora_damm_v2_program_id};
use crate::dex::whirlpool::constants::whirlpool_program_id;
use crate::dex::phoenix::constants::phoenix_program_id;
use crate::dex::lifinity::constants::lifinity_program_id;
use crate::dex::heaven::constants::heaven_program_id;

/// Well-known Jito tip accounts. The bot randomly selects one per bundle
/// to distribute tips across validators.
const JITO_TIP_ACCOUNTS: [&str; 8] = [
    "96gYZGLnJYVFmbjzopPSU6QiEV5fGqZNyN9nmNhvrZU5",
    "HFqU5x63VTqvQss8hp11i4bVqkfRtQ7NmXwkiCKtHBTo",
    "Cw8CFyM9FkoMi7K7Crf6HNQqf4uEMzpKw6QNghXLvLkY",
    "ADaUMid9yfUytqMBgopwjb2DTLSeyqR5Dg9TZLqzBQoU",
    "DfXygSm4jCyNCybVYYK6DwvWqjKee8pbDmJGcLWNDXjh",
    "ADuUkR4vqLUMWXxW9gh6D6L8pMSawimctcNZ5pGwDcEt",
    "DttWaMuVvTiduZRnguLF7jNxTgiMBZ1hyAumKUiL2KRL",
    "3AVi9Tg9Uo68tJfuvoKvqKNWKkC5wPdSSdeBnizKZ6jT",
];

/// Builds and submits versioned transactions with compute budget
pub struct TransactionBuilder {
    rpc: Arc<RpcClient>,
    payer: Arc<Keypair>,
    compute_unit_limit: u32,
    priority_fee_lamports: u64,
    spam_rpc_urls: Vec<String>,
    real_execution: bool,
    jito_enabled: bool,
    jito_tip_lamports: u64,
    jito_block_engine_url: String,
    /// When true, estimate CU from simulation instead of using compute_unit_limit
    dynamic_cu_enabled: bool,
    /// Buffer percentage added on top of simulated CU (e.g., 0.15 = 15%)
    cu_buffer_pct: f64,
    /// When true, fetch priority fee from network instead of using priority_fee_lamports
    dynamic_fee_enabled: bool,
    /// Percentile of recent fees to use (0-100)
    fee_percentile: usize,
    /// Cached dynamic priority fee in microlamports per CU
    cached_priority_fee_microlamports: u64,
    /// Address Lookup Tables for v0 transaction compression
    address_lookup_tables: Vec<AddressLookupTableAccount>,
}

impl TransactionBuilder {
    pub fn new(
        rpc: Arc<RpcClient>,
        payer: Arc<Keypair>,
        compute_unit_limit: u32,
        priority_fee_lamports: u64,
        spam_rpc_urls: Vec<String>,
        real_execution: bool,
    ) -> Self {
        Self {
            rpc,
            payer,
            compute_unit_limit,
            priority_fee_lamports,
            spam_rpc_urls,
            real_execution,
            jito_enabled: false,
            jito_tip_lamports: 0,
            jito_block_engine_url: String::new(),
            dynamic_cu_enabled: false,
            cu_buffer_pct: 0.15,
            dynamic_fee_enabled: false,
            fee_percentile: 75,
            cached_priority_fee_microlamports: 0,
            address_lookup_tables: vec![],
        }
    }

    /// Configure Jito MEV bundle submission settings.
    pub fn with_jito(mut self, enabled: bool, tip_lamports: u64, block_engine_url: String) -> Self {
        self.jito_enabled = enabled;
        self.jito_tip_lamports = tip_lamports;
        self.jito_block_engine_url = block_engine_url;
        self
    }

    /// Enable dynamic CU estimation from simulation results.
    pub fn with_dynamic_cu(mut self, enabled: bool, buffer_pct: f64) -> Self {
        self.dynamic_cu_enabled = enabled;
        self.cu_buffer_pct = buffer_pct;
        self
    }

    /// Enable dynamic priority fee from `getRecentPrioritizationFees`.
    pub fn with_dynamic_fee(mut self, enabled: bool, percentile: usize) -> Self {
        self.dynamic_fee_enabled = enabled;
        self.fee_percentile = percentile.min(100);
        self
    }

    /// Set Address Lookup Tables for v0 transaction compression.
    pub fn with_lookup_tables(mut self, tables: Vec<AddressLookupTableAccount>) -> Self {
        self.address_lookup_tables = tables;
        self
    }

    /// Fetch and deserialize Address Lookup Tables from RPC.
    /// Skips any tables that fail to load (logs a warning).
    pub fn load_lookup_tables(rpc: &RpcClient, alt_keys: &[Pubkey]) -> Vec<AddressLookupTableAccount> {
        let mut tables = Vec::with_capacity(alt_keys.len());
        for key in alt_keys {
            match rpc.get_account(key) {
                Ok(account) => {
                    match solana_program::address_lookup_table::state::AddressLookupTable::deserialize(&account.data) {
                        Ok(alt) => {
                            tables.push(AddressLookupTableAccount {
                                key: *key,
                                addresses: alt.addresses.to_vec(),
                            });
                            info!("Loaded ALT {} with {} addresses", key, alt.addresses.len());
                        }
                        Err(e) => {
                            warn!("Failed to deserialize ALT {}: {}", key, e);
                        }
                    }
                }
                Err(e) => {
                    warn!("Failed to fetch ALT account {}: {}", key, e);
                }
            }
        }
        tables
    }

    /// Returns true if Jito bundle submission is enabled.
    pub fn is_jito_enabled(&self) -> bool {
        self.jito_enabled
    }

    /// Simulate swap instructions with max CU budget to measure actual consumption.
    /// Returns `(simulation_passed, units_consumed)`.
    pub fn estimate_cu_from_simulation(
        &self,
        swap_instructions: &[Instruction],
    ) -> Result<(bool, u64)> {
        let mut sim_ixs = Vec::with_capacity(swap_instructions.len() + 1);
        sim_ixs.push(ComputeBudgetInstruction::set_compute_unit_limit(1_400_000));
        sim_ixs.extend_from_slice(swap_instructions);

        let blockhash = self.rpc.get_latest_blockhash()?;
        let tx = self.build_versioned_tx(&sim_ixs, blockhash)?;

        let result = self.rpc.simulate_transaction(&tx)?;

        let passed = result.value.err.is_none();
        let units = result.value.units_consumed.unwrap_or(0);

        Ok((passed, units))
    }

    /// Compute the CU limit to use: either dynamic (from simulation) or static fallback.
    fn resolve_cu_limit(&self, swap_instructions: &[Instruction]) -> u32 {
        if !self.dynamic_cu_enabled {
            return self.compute_unit_limit;
        }

        match self.estimate_cu_from_simulation(swap_instructions) {
            Ok((true, actual_cu)) if actual_cu > 0 => {
                let buffered = (actual_cu as f64 * (1.0 + self.cu_buffer_pct)) as u32;
                let clamped = buffered.clamp(50_000, 1_400_000);
                info!("Dynamic CU: actual={}, limit={} (+{}% buffer)", actual_cu, clamped, (self.cu_buffer_pct * 100.0) as u32);
                clamped
            }
            Ok((false, _)) => {
                warn!("Dynamic CU sim failed, using static limit {}", self.compute_unit_limit);
                self.compute_unit_limit
            }
            Ok(_) => {
                warn!("Dynamic CU sim returned 0, using static limit {}", self.compute_unit_limit);
                self.compute_unit_limit
            }
            Err(e) => {
                warn!("Dynamic CU estimation error, using static {}: {}", self.compute_unit_limit, e);
                self.compute_unit_limit
            }
        }
    }

    /// Fetch recent prioritization fees for the given writable accounts.
    /// Updates the cached fee and returns microlamports per CU.
    pub fn refresh_priority_fee(&mut self, writable_accounts: &[Pubkey]) -> u64 {
        if !self.dynamic_fee_enabled {
            // Convert static lamports to microlamports per CU
            if self.priority_fee_lamports > 0 && self.compute_unit_limit > 0 {
                return (self.priority_fee_lamports * 1_000_000) / self.compute_unit_limit as u64;
            }
            return 0;
        }

        let accounts: Vec<Pubkey> = writable_accounts.iter().take(128).copied().collect();
        match self.rpc.get_recent_prioritization_fees(&accounts) {
            Ok(response) => {
                let mut fees: Vec<u64> = response
                    .iter()
                    .map(|f| f.prioritization_fee)
                    .filter(|&f| f > 0)
                    .collect();

                if fees.is_empty() {
                    self.cached_priority_fee_microlamports = 100; // minimum floor
                } else {
                    fees.sort_unstable();
                    let idx = (fees.len() * self.fee_percentile / 100).min(fees.len() - 1);
                    self.cached_priority_fee_microlamports = fees[idx].clamp(100, 1_000_000);
                }

                info!(
                    "Dynamic priority fee: {} microlamports/CU (p{}, {} samples)",
                    self.cached_priority_fee_microlamports,
                    self.fee_percentile,
                    fees.len()
                );
                self.cached_priority_fee_microlamports
            }
            Err(e) => {
                warn!("Failed to fetch priority fees: {}", e);
                if self.cached_priority_fee_microlamports > 0 {
                    self.cached_priority_fee_microlamports
                } else {
                    100 // minimum floor
                }
            }
        }
    }

    /// Get the current priority fee in microlamports per CU (cached or static).
    fn resolve_priority_fee_microlamports(&self, cu_limit: u32) -> u64 {
        if self.dynamic_fee_enabled && self.cached_priority_fee_microlamports > 0 {
            return self.cached_priority_fee_microlamports;
        }
        if self.priority_fee_lamports > 0 && cu_limit > 0 {
            return (self.priority_fee_lamports * 1_000_000) / cu_limit as u64;
        }
        0
    }

    /// Build compute budget instructions (CU limit + priority fee) for a set of swap IXs.
    /// Uses dynamic estimation when enabled.
    fn build_compute_budget_ixs(&self, swap_instructions: &[Instruction]) -> Vec<Instruction> {
        let cu_limit = self.resolve_cu_limit(swap_instructions);
        let fee_microlamports = self.resolve_priority_fee_microlamports(cu_limit);

        let mut ixs = Vec::with_capacity(2);
        ixs.push(ComputeBudgetInstruction::set_compute_unit_limit(cu_limit));
        if fee_microlamports > 0 {
            ixs.push(ComputeBudgetInstruction::set_compute_unit_price(fee_microlamports));
        }
        ixs
    }

    /// Derive user's Associated Token Account for a given mint
    fn get_user_ata(&self, mint: &Pubkey) -> Pubkey {
        get_associated_token_address(&self.payer.pubkey(), mint)
    }

    /// Derive Raydium AMM authority PDA
    fn raydium_amm_authority() -> Pubkey {
        // Well-known Raydium AMM V4 authority (derived with nonce=254)
        Pubkey::from_str("5Q544fKrFoe6tsEbD7S8EmxGTJYAKtTVhAW5Q5pge4j1").unwrap()
    }

    /// Build a versioned (v0) transaction from instructions and a blockhash.
    /// Uses Address Lookup Tables when available for account compression.
    fn build_versioned_tx(&self, instructions: &[Instruction], blockhash: Hash) -> Result<VersionedTransaction> {
        let message = v0::Message::try_compile(
            &self.payer.pubkey(),
            instructions,
            &self.address_lookup_tables,
            blockhash,
        )?;
        let tx = VersionedTransaction::try_new(
            VersionedMessage::V0(message),
            &[self.payer.as_ref()],
        )?;
        Ok(tx)
    }

    /// Build a transaction with compute budget instructions prepended.
    /// Uses dynamic CU estimation and priority fees when enabled.
    pub fn build_swap_transaction(
        &self,
        swap_instructions: Vec<Instruction>,
        recent_blockhash: Hash,
    ) -> Result<VersionedTransaction> {
        let budget_ixs = self.build_compute_budget_ixs(&swap_instructions);
        let mut instructions = Vec::with_capacity(swap_instructions.len() + budget_ixs.len());

        instructions.extend(budget_ixs);
        instructions.extend(swap_instructions);

        self.build_versioned_tx(&instructions, recent_blockhash)
    }

    /// Simulate a transaction without sending it
    pub fn simulate(&self, tx: &VersionedTransaction) -> Result<bool> {
        let result = self.rpc.simulate_transaction(tx)?;

        if let Some(err) = result.value.err {
            warn!("Simulation failed: {:?}", err);
            Ok(false)
        } else {
            let units_consumed = result.value.units_consumed.unwrap_or(0);
            info!("Simulation passed, CU used: {}", units_consumed);
            Ok(true)
        }
    }

    /// Send transaction and wait for confirmation
    pub fn send_and_confirm(&self, tx: &VersionedTransaction) -> Result<String> {
        if !self.real_execution {
            let ix_count = match &tx.message {
                VersionedMessage::V0(m) => m.instructions.len(),
                VersionedMessage::Legacy(m) => m.instructions.len(),
            };
            info!("[DEMO] Would send transaction with {} instructions", ix_count);
            return Ok(format!("demo_sig_{}", self.payer.pubkey()));
        }

        let sig = self.rpc.send_and_confirm_transaction(tx)?;
        info!("Transaction confirmed: {}", sig);
        Ok(sig.to_string())
    }

    /// Submit transaction across multiple RPC endpoints in parallel
    pub async fn parallel_submit(&self, tx: &VersionedTransaction) -> Result<String> {
        if !self.real_execution {
            info!("[DEMO] Would parallel submit across {} RPC endpoints", self.spam_rpc_urls.len() + 1);
            return Ok(format!("demo_sig_{}", self.payer.pubkey()));
        }

        let primary_sig = self.rpc.send_and_confirm_transaction(tx)?;
        let sig_str = primary_sig.to_string();

        for url in &self.spam_rpc_urls {
            let rpc = RpcClient::new(url.clone());
            let tx_clone = tx.clone();
            tokio::task::spawn_blocking(move || {
                let _ = rpc.send_transaction(&tx_clone);
            });
        }

        info!("Parallel submit completed, primary sig: {}", sig_str);
        Ok(sig_str)
    }

    /// Get a fresh blockhash
    pub fn get_recent_blockhash(&self) -> Result<Hash> {
        let hash = self.rpc.get_latest_blockhash()?;
        Ok(hash)
    }

    /// Select a random Jito tip account from the well-known set.
    fn random_jito_tip_account() -> Result<Pubkey> {
        let idx = rand::thread_rng().gen_range(0..JITO_TIP_ACCOUNTS.len());
        Pubkey::from_str(JITO_TIP_ACCOUNTS[idx])
            .map_err(|e| anyhow!("Invalid Jito tip account: {}", e))
    }

    /// Build a Jito bundle transaction: compute budget + swap instructions + tip transfer.
    /// The tip is sent to a randomly selected Jito tip account.
    pub fn build_jito_bundle(
        &self,
        swap_instructions: Vec<Instruction>,
        recent_blockhash: Hash,
    ) -> Result<VersionedTransaction> {
        let budget_ixs = self.build_compute_budget_ixs(&swap_instructions);
        let mut instructions = Vec::with_capacity(swap_instructions.len() + budget_ixs.len() + 1);

        // Compute budget instructions (dynamic when enabled)
        instructions.extend(budget_ixs);

        // Swap instructions
        instructions.extend(swap_instructions);

        // Jito tip transfer
        let tip_account = Self::random_jito_tip_account()?;
        instructions.push(
            system_instruction::transfer(&self.payer.pubkey(), &tip_account, self.jito_tip_lamports)
        );

        let tx = self.build_versioned_tx(&instructions, recent_blockhash)?;

        info!(
            "Built Jito bundle TX: tip={} lamports to {}",
            self.jito_tip_lamports,
            tip_account,
        );

        Ok(tx)
    }

    /// Submit a signed transaction as a Jito MEV bundle via the block engine API.
    /// Returns the bundle ID on success.
    pub fn submit_jito_bundle(&self, tx: &VersionedTransaction) -> Result<String> {
        if !self.real_execution {
            info!(
                "[DEMO] Would submit Jito bundle, tip={} lamports",
                self.jito_tip_lamports,
            );
            return Ok(format!("demo_jito_bundle_{}", self.payer.pubkey()));
        }

        let serialized = bincode::serialize(tx)
            .map_err(|e| anyhow!("Failed to serialize transaction: {}", e))?;
        let encoded = bs58::encode(&serialized).into_string();

        let url = format!("{}/api/v1/bundles", self.jito_block_engine_url);
        let body = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "sendBundle",
            "params": [[encoded]]
        });

        let client = reqwest::blocking::Client::builder()
            .timeout(std::time::Duration::from_secs(10))
            .build()
            .map_err(|e| anyhow!("Failed to create HTTP client: {}", e))?;

        let response = client
            .post(&url)
            .json(&body)
            .send()
            .map_err(|e| anyhow!("Jito bundle submission failed: {}", e))?;

        let status = response.status();
        let resp_text = response
            .text()
            .unwrap_or_else(|_| "no response body".to_string());

        if !status.is_success() {
            return Err(anyhow!(
                "Jito bundle rejected (HTTP {}): {}",
                status,
                resp_text
            ));
        }

        let resp_json: serde_json::Value = serde_json::from_str(&resp_text)
            .map_err(|e| anyhow!("Failed to parse Jito response: {} body={}", e, resp_text))?;

        if let Some(error) = resp_json.get("error") {
            return Err(anyhow!("Jito RPC error: {}", error));
        }

        let bundle_id = resp_json
            .get("result")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown")
            .to_string();

        info!("Jito bundle submitted: {}", bundle_id);
        Ok(bundle_id)
    }

    /// Build swap instructions from an ArbitrageOpportunity path.
    /// Uses the refresh manager's deserialized pool states to get actual vault addresses.
    pub fn build_instructions_from_opportunity(
        &self,
        opportunity: &ArbitrageOpportunity,
        refresh_manager: &PoolRefreshManager,
        user_token_accounts: &std::collections::HashMap<String, Pubkey>,
        slippage_bps: u64,
    ) -> Result<Vec<Instruction>> {
        let mut instructions = Vec::new();

        for step in &opportunity.path {
            let pool_address = Pubkey::from_str(&step.pool_address)
                .map_err(|e| anyhow!("Invalid pool address {}: {}", step.pool_address, e))?;

            let amount_in = step.amount_in as u64;
            let min_amount_out = ((step.amount_out as f64) * (10000.0 - slippage_bps as f64) / 10000.0) as u64;

            let ix = match step.dex.as_str() {
                "Raydium" => {
                    self.build_raydium_swap_from_step(
                        &pool_address, refresh_manager, amount_in, min_amount_out, step,
                    )?
                }
                "Pump" => {
                    self.build_pump_swap_from_step(
                        &pool_address, refresh_manager, amount_in, min_amount_out, step,
                    )?
                }
                "DLMM" => {
                    self.build_dlmm_swap_from_step(
                        &pool_address, refresh_manager, amount_in, min_amount_out, step,
                    )?
                }
                "MeteoraDAmmV2" => {
                    self.build_damm_v2_swap_from_step(
                        &pool_address, refresh_manager, amount_in, min_amount_out, step,
                    )?
                }
                "Whirlpool" => {
                    self.build_whirlpool_swap_from_step(
                        &pool_address, refresh_manager, amount_in, min_amount_out, step,
                    )?
                }
                "RaydiumClmm" => {
                    self.build_raydium_clmm_swap_from_step(
                        &pool_address, refresh_manager, amount_in, min_amount_out, step,
                    )?
                }
                "Phoenix" => {
                    self.build_phoenix_swap_from_step(
                        &pool_address, refresh_manager, amount_in, min_amount_out, step,
                    )?
                }
                "Lifinity" => {
                    self.build_lifinity_swap_from_step(
                        &pool_address, refresh_manager, amount_in, min_amount_out, step,
                    )?
                }
                "Heaven" => {
                    self.build_heaven_swap_from_step(
                        &pool_address, refresh_manager, amount_in, min_amount_out, step,
                    )?
                }
                other => {
                    warn!("Unsupported DEX for IX building: {}", other);
                    continue;
                }
            };

            instructions.push(ix);
        }

        if instructions.is_empty() {
            return Err(anyhow!("No swap instructions could be built from opportunity"));
        }

        Ok(instructions)
    }

    fn build_raydium_swap_from_step(
        &self,
        pool_address: &Pubkey,
        refresh_manager: &PoolRefreshManager,
        amount_in: u64,
        min_amount_out: u64,
        step: &PathStep,
    ) -> Result<Instruction> {
        let program_id = raydium_amm_program_id();

        let (coin_vault, pc_vault, coin_mint, pc_mint,
             amm_open_orders, amm_target_orders,
             serum_market, serum_program_id) = match refresh_manager.get_pool_state(pool_address) {
            Some(DeserializedPoolState::RaydiumAmm {
                coin_vault, pc_vault, coin_mint, pc_mint,
                amm_open_orders, amm_target_orders,
                serum_market, serum_program_id, ..
            }) => {
                (*coin_vault, *pc_vault, *coin_mint, *pc_mint,
                 *amm_open_orders, *amm_target_orders,
                 *serum_market, *serum_program_id)
            }
            _ => {
                return Err(anyhow!("No deserialized state for Raydium pool {}", pool_address));
            }
        };

        // Derive user ATAs based on swap direction
        let (user_source, user_dest) = if step.action == "buy" {
            // buying tokens with SOL: source=WSOL ATA, dest=token ATA
            let sol_mint = Pubkey::from_str(crate::chain::constants::SOL_MINT)?;
            (self.get_user_ata(&sol_mint), self.get_user_ata(&coin_mint))
        } else {
            // selling tokens for SOL
            let sol_mint = Pubkey::from_str(crate::chain::constants::SOL_MINT)?;
            (self.get_user_ata(&coin_mint), self.get_user_ata(&sol_mint))
        };

        let amm_authority = Self::raydium_amm_authority();

        // Get Serum market accounts
        let (serum_bids, serum_asks, serum_event_queue,
             serum_coin_vault, serum_pc_vault, serum_vault_signer) =
            match refresh_manager.get_serum_market(&serum_market) {
                Some(market) => {
                    (market.bids, market.asks, market.event_queue,
                     market.coin_vault, market.pc_vault, market.vault_signer)
                }
                None => {
                    return Err(anyhow!("No Serum market state for {}", serum_market));
                }
            };

        let data = Self::encode_raydium_swap_data(amount_in, min_amount_out);

        Ok(Instruction {
            program_id,
            accounts: vec![
                AccountMeta::new_readonly(spl_token::id(), false),
                AccountMeta::new(*pool_address, false),
                AccountMeta::new_readonly(amm_authority, false),
                AccountMeta::new(amm_open_orders, false),
                AccountMeta::new(amm_target_orders, false),
                AccountMeta::new(coin_vault, false),
                AccountMeta::new(pc_vault, false),
                AccountMeta::new_readonly(serum_program_id, false),
                AccountMeta::new(serum_market, false),
                AccountMeta::new(serum_bids, false),
                AccountMeta::new(serum_asks, false),
                AccountMeta::new(serum_event_queue, false),
                AccountMeta::new(serum_coin_vault, false),
                AccountMeta::new(serum_pc_vault, false),
                AccountMeta::new_readonly(serum_vault_signer, false),
                AccountMeta::new(user_source, false),
                AccountMeta::new(user_dest, false),
                AccountMeta::new_readonly(self.payer.pubkey(), true),
            ],
            data,
        })
    }

    fn build_pump_swap_from_step(
        &self,
        pool_address: &Pubkey,
        refresh_manager: &PoolRefreshManager,
        amount_in: u64,
        min_amount_out: u64,
        step: &PathStep,
    ) -> Result<Instruction> {
        // Get mint from deserialized state
        let mint = match refresh_manager.get_pool_state(pool_address) {
            Some(DeserializedPoolState::Pump { mint, .. }) => *mint,
            _ => {
                return Err(anyhow!("No deserialized state for Pump pool {}", pool_address));
            }
        };

        let bonding_curve_token_account = get_associated_token_address(pool_address, &mint);
        let user_token_account = self.get_user_ata(&mint);
        let is_buy = step.action == "buy";

        Ok(Self::build_pump_swap_ix(
            &pump_program_id(),
            pool_address,
            &bonding_curve_token_account,
            pool_address, // bonding curve is its own SOL holder
            &user_token_account,
            &self.payer.pubkey(),
            amount_in,
            min_amount_out,
            is_buy,
        ))
    }

    fn build_dlmm_swap_from_step(
        &self,
        pool_address: &Pubkey,
        refresh_manager: &PoolRefreshManager,
        amount_in: u64,
        min_amount_out: u64,
        step: &PathStep,
    ) -> Result<Instruction> {
        let program_id = meteora_dlmm_program_id();

        let (reserve_x, reserve_y, token_x_mint, token_y_mint) = match refresh_manager.get_pool_state(pool_address) {
            Some(DeserializedPoolState::MeteoraDlmm {
                reserve_x_vault, reserve_y_vault, token_x_mint, token_y_mint, ..
            }) => {
                (*reserve_x_vault, *reserve_y_vault, *token_x_mint, *token_y_mint)
            }
            _ => {
                return Err(anyhow!("No deserialized state for DLMM pool {}", pool_address));
            }
        };

        // Derive user ATAs
        let (user_token_in, user_token_out) = if step.action == "buy" {
            // Buying token_x with token_y (SOL)
            (self.get_user_ata(&token_y_mint), self.get_user_ata(&token_x_mint))
        } else {
            (self.get_user_ata(&token_x_mint), self.get_user_ata(&token_y_mint))
        };

        // Derive event authority PDA
        let (event_authority, _) = Pubkey::find_program_address(
            &[b"__event_authority"],
            &program_id,
        );

        // Derive bin array bitmap extension PDA
        let (bin_array_bitmap_extension, _) = Pubkey::find_program_address(
            &[b"bitmap", pool_address.as_ref()],
            &program_id,
        );

        let mut data = Vec::new();
        // DLMM swap discriminator (anchor: hash of "global:swap")
        data.extend_from_slice(&[248, 198, 158, 145, 225, 117, 135, 200]);
        data.extend_from_slice(&amount_in.to_le_bytes());
        data.extend_from_slice(&min_amount_out.to_le_bytes());

        Ok(Instruction {
            program_id,
            accounts: vec![
                AccountMeta::new(*pool_address, false),
                AccountMeta::new_readonly(bin_array_bitmap_extension, false),
                AccountMeta::new(reserve_x, false),
                AccountMeta::new(reserve_y, false),
                AccountMeta::new(user_token_in, false),
                AccountMeta::new(user_token_out, false),
                AccountMeta::new_readonly(spl_token::id(), false),
                AccountMeta::new_readonly(event_authority, false),
                AccountMeta::new_readonly(program_id, false),
                AccountMeta::new_readonly(self.payer.pubkey(), true),
            ],
            data,
        })
    }

    fn build_damm_v2_swap_from_step(
        &self,
        pool_address: &Pubkey,
        refresh_manager: &PoolRefreshManager,
        amount_in: u64,
        min_amount_out: u64,
        step: &PathStep,
    ) -> Result<Instruction> {
        let program_id = meteora_damm_v2_program_id();

        let (a_vault, b_vault, token_a_mint, token_b_mint) = match refresh_manager.get_pool_state(pool_address) {
            Some(DeserializedPoolState::MeteoraDAmmV2 {
                a_vault, b_vault, token_a_mint, token_b_mint, ..
            }) => {
                (*a_vault, *b_vault, *token_a_mint, *token_b_mint)
            }
            _ => {
                return Err(anyhow!("No deserialized state for DAMM V2 pool {}", pool_address));
            }
        };

        // Derive user ATAs
        let (user_source_token, user_destination_token) = if step.action == "buy" {
            (self.get_user_ata(&token_b_mint), self.get_user_ata(&token_a_mint))
        } else {
            (self.get_user_ata(&token_a_mint), self.get_user_ata(&token_b_mint))
        };

        let mut data = Vec::new();
        data.extend_from_slice(&[248, 198, 158, 145, 225, 117, 135, 200]); // swap discriminator
        data.extend_from_slice(&amount_in.to_le_bytes());
        data.extend_from_slice(&min_amount_out.to_le_bytes());

        Ok(Instruction {
            program_id,
            accounts: vec![
                AccountMeta::new(*pool_address, false),
                AccountMeta::new(a_vault, false),
                AccountMeta::new(b_vault, false),
                AccountMeta::new(user_source_token, false),
                AccountMeta::new(user_destination_token, false),
                AccountMeta::new_readonly(spl_token::id(), false),
                AccountMeta::new_readonly(self.payer.pubkey(), true),
            ],
            data,
        })
    }

    fn build_whirlpool_swap_from_step(
        &self,
        pool_address: &Pubkey,
        refresh_manager: &PoolRefreshManager,
        amount_in: u64,
        min_amount_out: u64,
        step: &PathStep,
    ) -> Result<Instruction> {
        let program_id = whirlpool_program_id();

        let (vault_a, vault_b, mint_a, mint_b, tick_current_index, tick_spacing) =
            match refresh_manager.get_pool_state(pool_address) {
                Some(DeserializedPoolState::WhirlpoolState {
                    vault_a, vault_b, mint_a, mint_b,
                    tick_current_index, tick_spacing, ..
                }) => {
                    (*vault_a, *vault_b, *mint_a, *mint_b, *tick_current_index, *tick_spacing)
                }
                _ => {
                    return Err(anyhow!("No deserialized state for Whirlpool pool {}", pool_address));
                }
            };

        let a_to_b = step.action == "sell"; // selling token A for token B

        // Derive user ATAs
        let user_token_a = self.get_user_ata(&mint_a);
        let user_token_b = self.get_user_ata(&mint_b);

        // Derive oracle PDA
        let (oracle, _) = Pubkey::find_program_address(
            &[b"oracle", pool_address.as_ref()],
            &program_id,
        );

        // Derive tick array PDAs from current tick
        let ticks_per_array: i32 = tick_spacing as i32 * 88; // Whirlpool uses 88 ticks per array
        let tick_arrays = Self::derive_whirlpool_tick_arrays(
            pool_address,
            &program_id,
            tick_current_index,
            ticks_per_array,
            a_to_b,
        );

        let mut data = Vec::new();
        // Whirlpool swap discriminator
        data.extend_from_slice(&[248, 198, 158, 145, 225, 117, 135, 200]);
        data.extend_from_slice(&amount_in.to_le_bytes());
        data.extend_from_slice(&min_amount_out.to_le_bytes());
        // sqrt_price_limit (u128): use min/max based on direction
        let sqrt_price_limit: u128 = if a_to_b {
            4295048016u128 // MIN_SQRT_PRICE
        } else {
            79226673515401279992447579055u128 // MAX_SQRT_PRICE
        };
        data.extend_from_slice(&sqrt_price_limit.to_le_bytes());
        // amount_specified_is_input (bool)
        data.push(1u8);
        // a_to_b (bool)
        data.push(if a_to_b { 1 } else { 0 });

        Ok(Instruction {
            program_id,
            accounts: vec![
                AccountMeta::new_readonly(spl_token::id(), false),
                AccountMeta::new_readonly(self.payer.pubkey(), true),
                AccountMeta::new(*pool_address, false),
                AccountMeta::new(user_token_a, false),
                AccountMeta::new(vault_a, false),
                AccountMeta::new(user_token_b, false),
                AccountMeta::new(vault_b, false),
                AccountMeta::new(tick_arrays[0], false),
                AccountMeta::new(tick_arrays[1], false),
                AccountMeta::new(tick_arrays[2], false),
                AccountMeta::new_readonly(oracle, false),
            ],
            data,
        })
    }

    fn build_raydium_clmm_swap_from_step(
        &self,
        pool_address: &Pubkey,
        refresh_manager: &PoolRefreshManager,
        amount_in: u64,
        min_amount_out: u64,
        step: &PathStep,
    ) -> Result<Instruction> {
        let program_id = raydium_clmm_program_id();

        let (vault_0, vault_1, mint_0, mint_1, tick_current, tick_spacing, amm_config, observation_state) =
            match refresh_manager.get_pool_state(pool_address) {
                Some(DeserializedPoolState::RaydiumClmm {
                    vault_0, vault_1, mint_0, mint_1,
                    tick_current, tick_spacing, amm_config, observation_state, ..
                }) => {
                    (*vault_0, *vault_1, *mint_0, *mint_1,
                     *tick_current, *tick_spacing, *amm_config, *observation_state)
                }
                _ => {
                    return Err(anyhow!("No deserialized state for Raydium CLMM pool {}", pool_address));
                }
            };

        let a_to_b = step.action == "sell";

        let user_token_0 = self.get_user_ata(&mint_0);
        let user_token_1 = self.get_user_ata(&mint_1);

        // Derive tick array PDAs
        let ticks_per_array: i32 = tick_spacing as i32 * 60; // Raydium CLMM uses 60 ticks per array
        let tick_arrays = Self::derive_raydium_clmm_tick_arrays(
            pool_address,
            &program_id,
            tick_current,
            ticks_per_array,
            a_to_b,
        );

        // Derive bitmap extension PDA
        let (bitmap_extension, _) = Pubkey::find_program_address(
            &[
                crate::dex::raydium::POOL_TICK_ARRAY_BITMAP_SEED.as_bytes(),
                pool_address.as_ref(),
            ],
            &program_id,
        );

        // Raydium CLMM swap discriminator
        let mut data = Vec::new();
        data.extend_from_slice(&[43, 4, 237, 11, 26, 201, 106, 116]); // swap_v2 discriminator
        data.extend_from_slice(&amount_in.to_le_bytes());
        data.extend_from_slice(&min_amount_out.to_le_bytes());
        // sqrt_price_limit_x64 (u128)
        let sqrt_price_limit: u128 = if a_to_b {
            4295048016u128
        } else {
            79226673515401279992447579055u128
        };
        data.extend_from_slice(&sqrt_price_limit.to_le_bytes());
        // is_base_input (bool)
        data.push(1u8);

        Ok(Instruction {
            program_id,
            accounts: vec![
                AccountMeta::new_readonly(self.payer.pubkey(), true),
                AccountMeta::new_readonly(amm_config, false),
                AccountMeta::new(*pool_address, false),
                AccountMeta::new(user_token_0, false),
                AccountMeta::new(user_token_1, false),
                AccountMeta::new(vault_0, false),
                AccountMeta::new(vault_1, false),
                AccountMeta::new(observation_state, false),
                AccountMeta::new_readonly(spl_token::id(), false),
                AccountMeta::new_readonly(spl_token_2022::id(), false),
                // Remaining accounts: tick arrays
                AccountMeta::new(tick_arrays[0], false),
                AccountMeta::new(tick_arrays[1], false),
                AccountMeta::new(tick_arrays[2], false),
                AccountMeta::new_readonly(bitmap_extension, false),
            ],
            data,
        })
    }

    fn build_phoenix_swap_from_step(
        &self,
        pool_address: &Pubkey,
        refresh_manager: &PoolRefreshManager,
        amount_in: u64,
        min_amount_out: u64,
        step: &PathStep,
    ) -> Result<Instruction> {
        let program_id = phoenix_program_id();

        let (base_vault, quote_vault, base_mint, quote_mint) = match refresh_manager.get_pool_state(pool_address) {
            Some(DeserializedPoolState::Phoenix {
                base_vault, quote_vault, base_mint, quote_mint, ..
            }) => {
                (*base_vault, *quote_vault, *base_mint, *quote_mint)
            }
            _ => {
                return Err(anyhow!("No deserialized state for Phoenix pool {}", pool_address));
            }
        };

        let (user_source, user_dest) = if step.action == "buy" {
            (self.get_user_ata(&quote_mint), self.get_user_ata(&base_mint))
        } else {
            (self.get_user_ata(&base_mint), self.get_user_ata(&quote_mint))
        };

        // Phoenix swap discriminator
        let mut data = Vec::new();
        data.extend_from_slice(&[0xf8, 0xc6, 0x9e, 0x91, 0xe1, 0x75, 0x87, 0xc8]);
        data.extend_from_slice(&amount_in.to_le_bytes());
        data.extend_from_slice(&min_amount_out.to_le_bytes());

        Ok(Instruction {
            program_id,
            accounts: vec![
                AccountMeta::new(*pool_address, false),
                AccountMeta::new_readonly(self.payer.pubkey(), true),
                AccountMeta::new(user_source, false),
                AccountMeta::new(user_dest, false),
                AccountMeta::new(base_vault, false),
                AccountMeta::new(quote_vault, false),
                AccountMeta::new_readonly(spl_token::id(), false),
            ],
            data,
        })
    }

    fn build_lifinity_swap_from_step(
        &self,
        pool_address: &Pubkey,
        refresh_manager: &PoolRefreshManager,
        amount_in: u64,
        min_amount_out: u64,
        step: &PathStep,
    ) -> Result<Instruction> {
        let program_id = lifinity_program_id();

        let (token_a_vault, token_b_vault, token_a_mint, token_b_mint) = match refresh_manager.get_pool_state(pool_address) {
            Some(DeserializedPoolState::Lifinity {
                token_a_vault, token_b_vault, token_a_mint, token_b_mint, ..
            }) => {
                (*token_a_vault, *token_b_vault, *token_a_mint, *token_b_mint)
            }
            _ => {
                return Err(anyhow!("No deserialized state for Lifinity pool {}", pool_address));
            }
        };

        let (user_source, user_dest) = if step.action == "buy" {
            (self.get_user_ata(&token_b_mint), self.get_user_ata(&token_a_mint))
        } else {
            (self.get_user_ata(&token_a_mint), self.get_user_ata(&token_b_mint))
        };

        // Lifinity swap discriminator
        let mut data = Vec::new();
        data.extend_from_slice(&[0xf8, 0xc6, 0x9e, 0x91, 0xe1, 0x75, 0x87, 0xc8]);
        data.extend_from_slice(&amount_in.to_le_bytes());
        data.extend_from_slice(&min_amount_out.to_le_bytes());

        Ok(Instruction {
            program_id,
            accounts: vec![
                AccountMeta::new(*pool_address, false),
                AccountMeta::new_readonly(self.payer.pubkey(), true),
                AccountMeta::new(user_source, false),
                AccountMeta::new(user_dest, false),
                AccountMeta::new(token_a_vault, false),
                AccountMeta::new(token_b_vault, false),
                AccountMeta::new_readonly(spl_token::id(), false),
            ],
            data,
        })
    }

    fn build_heaven_swap_from_step(
        &self,
        pool_address: &Pubkey,
        refresh_manager: &PoolRefreshManager,
        amount_in: u64,
        min_amount_out: u64,
        step: &PathStep,
    ) -> Result<Instruction> {
        let program_id = heaven_program_id();

        let mint = match refresh_manager.get_pool_state(pool_address) {
            Some(DeserializedPoolState::Heaven { mint, .. }) => *mint,
            _ => {
                return Err(anyhow!("No deserialized state for Heaven pool {}", pool_address));
            }
        };

        let bonding_curve_token_account = get_associated_token_address(pool_address, &mint);
        let user_token_account = self.get_user_ata(&mint);
        let is_buy = step.action == "buy";

        let mut data = Vec::new();
        if is_buy {
            data.extend_from_slice(&[102, 6, 61, 18, 1, 218, 235, 234]); // buy discriminator
        } else {
            data.extend_from_slice(&[51, 230, 133, 164, 1, 127, 131, 173]); // sell discriminator
        }
        data.extend_from_slice(&amount_in.to_le_bytes());
        data.extend_from_slice(&min_amount_out.to_le_bytes());

        Ok(Instruction {
            program_id,
            accounts: vec![
                AccountMeta::new_readonly(solana_sdk::system_program::id(), false),
                AccountMeta::new_readonly(spl_token::id(), false),
                AccountMeta::new(*pool_address, false),
                AccountMeta::new(bonding_curve_token_account, false),
                AccountMeta::new(*pool_address, false),
                AccountMeta::new(user_token_account, false),
                AccountMeta::new(self.payer.pubkey(), true),
            ],
            data,
        })
    }

    /// Derive 3 tick array PDAs for Whirlpool based on current tick and direction
    fn derive_whirlpool_tick_arrays(
        pool_address: &Pubkey,
        program_id: &Pubkey,
        tick_current: i32,
        ticks_per_array: i32,
        a_to_b: bool,
    ) -> [Pubkey; 3] {
        let start_index = (tick_current / ticks_per_array) * ticks_per_array;

        let offsets: [i32; 3] = if a_to_b {
            [0, -ticks_per_array, -2 * ticks_per_array]
        } else {
            [0, ticks_per_array, 2 * ticks_per_array]
        };

        let mut result = [Pubkey::default(); 3];
        for (i, offset) in offsets.iter().enumerate() {
            let idx = start_index + offset;
            let (pda, _) = Pubkey::find_program_address(
                &[b"tick_array", pool_address.as_ref(), &idx.to_le_bytes()],
                program_id,
            );
            result[i] = pda;
        }
        result
    }

    /// Derive 3 tick array PDAs for Raydium CLMM
    fn derive_raydium_clmm_tick_arrays(
        pool_address: &Pubkey,
        program_id: &Pubkey,
        tick_current: i32,
        ticks_per_array: i32,
        a_to_b: bool,
    ) -> [Pubkey; 3] {
        let start_index = if ticks_per_array != 0 {
            (tick_current / ticks_per_array) * ticks_per_array
        } else {
            0
        };

        let offsets: [i32; 3] = if a_to_b {
            [0, -ticks_per_array, -2 * ticks_per_array]
        } else {
            [0, ticks_per_array, 2 * ticks_per_array]
        };

        let mut result = [Pubkey::default(); 3];
        for (i, offset) in offsets.iter().enumerate() {
            let idx = start_index + offset;
            let (pda, _) = Pubkey::find_program_address(
                &[b"tick_array", pool_address.as_ref(), &idx.to_le_bytes()],
                program_id,
            );
            result[i] = pda;
        }
        result
    }

    /// Build Raydium AMM V4 swap instruction (full account set, for direct use)
    pub fn build_raydium_swap_ix(
        program_id: &Pubkey,
        amm_id: &Pubkey,
        amm_authority: &Pubkey,
        amm_open_orders: &Pubkey,
        amm_target_orders: &Pubkey,
        pool_coin_vault: &Pubkey,
        pool_pc_vault: &Pubkey,
        serum_program: &Pubkey,
        serum_market: &Pubkey,
        serum_bids: &Pubkey,
        serum_asks: &Pubkey,
        serum_event_queue: &Pubkey,
        serum_coin_vault: &Pubkey,
        serum_pc_vault: &Pubkey,
        serum_vault_signer: &Pubkey,
        user_source: &Pubkey,
        user_destination: &Pubkey,
        user_owner: &Pubkey,
        amount_in: u64,
        minimum_amount_out: u64,
    ) -> Instruction {
        let data = Self::encode_raydium_swap_data(amount_in, minimum_amount_out);

        Instruction {
            program_id: *program_id,
            accounts: vec![
                AccountMeta::new_readonly(spl_token::id(), false),
                AccountMeta::new(*amm_id, false),
                AccountMeta::new_readonly(*amm_authority, false),
                AccountMeta::new(*amm_open_orders, false),
                AccountMeta::new(*amm_target_orders, false),
                AccountMeta::new(*pool_coin_vault, false),
                AccountMeta::new(*pool_pc_vault, false),
                AccountMeta::new_readonly(*serum_program, false),
                AccountMeta::new(*serum_market, false),
                AccountMeta::new(*serum_bids, false),
                AccountMeta::new(*serum_asks, false),
                AccountMeta::new(*serum_event_queue, false),
                AccountMeta::new(*serum_coin_vault, false),
                AccountMeta::new(*serum_pc_vault, false),
                AccountMeta::new_readonly(*serum_vault_signer, false),
                AccountMeta::new(*user_source, false),
                AccountMeta::new(*user_destination, false),
                AccountMeta::new_readonly(*user_owner, true),
            ],
            data,
        }
    }

    fn encode_raydium_swap_data(amount_in: u64, minimum_amount_out: u64) -> Vec<u8> {
        let mut data = vec![9u8]; // Raydium swap discriminator
        data.extend_from_slice(&amount_in.to_le_bytes());
        data.extend_from_slice(&minimum_amount_out.to_le_bytes());
        data
    }

    /// Build Pump.fun swap instruction
    pub fn build_pump_swap_ix(
        program_id: &Pubkey,
        bonding_curve: &Pubkey,
        bonding_curve_token_account: &Pubkey,
        bonding_curve_sol_account: &Pubkey,
        user_token_account: &Pubkey,
        user: &Pubkey,
        amount: u64,
        min_out: u64,
        is_buy: bool,
    ) -> Instruction {
        let mut data = Vec::new();
        if is_buy {
            data.extend_from_slice(&[102, 6, 61, 18, 1, 218, 235, 234]);
        } else {
            data.extend_from_slice(&[51, 230, 133, 164, 1, 127, 131, 173]);
        }
        data.extend_from_slice(&amount.to_le_bytes());
        data.extend_from_slice(&min_out.to_le_bytes());

        Instruction {
            program_id: *program_id,
            accounts: vec![
                AccountMeta::new_readonly(solana_sdk::system_program::id(), false),
                AccountMeta::new_readonly(spl_token::id(), false),
                AccountMeta::new(*bonding_curve, false),
                AccountMeta::new(*bonding_curve_token_account, false),
                AccountMeta::new(*bonding_curve_sol_account, false),
                AccountMeta::new(*user_token_account, false),
                AccountMeta::new(*user, true),
            ],
            data,
        }
    }

    pub fn payer_pubkey(&self) -> Pubkey {
        self.payer.pubkey()
    }

    /// Calculate optimal flash loan borrow amount for an arbitrage opportunity.
    /// Returns (borrow_lamports, estimated_net_profit_lamports), or None if a
    /// flash loan would not increase profitability.
    pub fn calculate_optimal_flashloan(
        &self,
        opportunity_input_sol: f64,
        opportunity_profit_pct: f64,
        own_capital_lamports: u64,
    ) -> Option<(u64, u64)> {
        let kamino_fee_pct = 0.09; // 0.09% Kamino flash loan fee

        if opportunity_profit_pct <= kamino_fee_pct {
            return None;
        }

        let opp_input_lamports = (opportunity_input_sol * 1e9) as u64;

        if own_capital_lamports >= opp_input_lamports {
            // We have enough capital. Only borrow if leveraging improves profit.
            let profit_own = opportunity_input_sol * (opportunity_profit_pct / 100.0);

            // Try up to 3x leverage
            let borrow_amount = own_capital_lamports.saturating_mul(3).min(opp_input_lamports * 3);
            let borrow_sol = borrow_amount as f64 / 1e9;

            let gross_fl = borrow_sol * (opportunity_profit_pct / 100.0);
            let fl_fee = borrow_sol * (kamino_fee_pct / 100.0);
            let net_fl = gross_fl - fl_fee;

            if net_fl > profit_own * 1.1 {
                return Some((borrow_amount, (net_fl * 1e9) as u64));
            }
            return None; // Own capital sufficient
        }

        // Insufficient own capital — borrow the deficit
        let deficit = opp_input_lamports.saturating_sub(own_capital_lamports);
        let borrow_sol = deficit as f64 / 1e9;
        let gross_profit = opportunity_input_sol * (opportunity_profit_pct / 100.0);
        let fl_fee = borrow_sol * (kamino_fee_pct / 100.0);
        let net = gross_profit - fl_fee;

        if net > 0.0 {
            Some((deficit, (net * 1e9) as u64))
        } else {
            None
        }
    }

    // --- Kamino Flash Loan Integration ---

    /// KLend program ID (Kamino Finance)
    fn klend_program_id() -> Pubkey {
        Pubkey::from_str("KLend2g3cP87fffoy8q1mQqGKjrxjC8boSyAYavgmjD").unwrap()
    }

    /// KLend main lending market
    fn klend_lending_market() -> Pubkey {
        Pubkey::from_str("7u3HeHxYDLhnCoErrtycNokbQYbWGzLs6JSDqGAv5PfF").unwrap()
    }

    /// Derive KLend lending market authority PDA
    fn klend_market_authority() -> Pubkey {
        let market = Self::klend_lending_market();
        Pubkey::find_program_address(
            &[b"lma", market.as_ref()],
            &Self::klend_program_id(),
        ).0
    }

    /// Instructions sysvar (used by KLend to verify atomicity)
    fn instructions_sysvar() -> Pubkey {
        Pubkey::from_str("Sysvar1nstructions1111111111111111111111111").unwrap()
    }

    /// Build KLend flash borrow instruction
    pub fn build_flash_borrow_ix(
        &self,
        amount: u64,
        reserve: &Pubkey,
        reserve_liquidity_vault: &Pubkey,
        fee_receiver: &Pubkey,
    ) -> Instruction {
        let klend = Self::klend_program_id();
        let market = Self::klend_lending_market();
        let market_authority = Self::klend_market_authority();
        let user_wsol_ata = self.get_user_ata(&spl_token::native_mint::id());
        let sysvar = Self::instructions_sysvar();

        let mut data = Vec::with_capacity(16);
        data.extend_from_slice(&[135, 231, 52, 167, 7, 52, 212, 193]); // flash_borrow discriminator
        data.extend_from_slice(&amount.to_le_bytes());

        Instruction {
            program_id: klend,
            accounts: vec![
                AccountMeta::new_readonly(self.payer.pubkey(), true),  // user_transfer_authority
                AccountMeta::new_readonly(market_authority, false),     // lending_market_authority
                AccountMeta::new_readonly(market, false),              // lending_market
                AccountMeta::new(*reserve, false),                     // reserve
                AccountMeta::new(*reserve_liquidity_vault, false),     // reserve_source_liquidity
                AccountMeta::new(user_wsol_ata, false),                // user_destination_liquidity
                AccountMeta::new(*fee_receiver, false),                // reserve_liquidity_fee_receiver
                AccountMeta::new_readonly(klend, false),               // referrer_token_state (placeholder)
                AccountMeta::new_readonly(klend, false),               // referrer_account (placeholder)
                AccountMeta::new_readonly(sysvar, false),              // sysvar_info
                AccountMeta::new_readonly(spl_token::id(), false),     // token_program
            ],
            data,
        }
    }

    /// Build KLend flash repay instruction
    pub fn build_flash_repay_ix(
        &self,
        amount: u64,
        borrow_ix_index: u8,
        reserve: &Pubkey,
        reserve_liquidity_vault: &Pubkey,
        fee_receiver: &Pubkey,
    ) -> Instruction {
        let klend = Self::klend_program_id();
        let market = Self::klend_lending_market();
        let market_authority = Self::klend_market_authority();
        let user_wsol_ata = self.get_user_ata(&spl_token::native_mint::id());
        let sysvar = Self::instructions_sysvar();

        let mut data = Vec::with_capacity(17);
        data.extend_from_slice(&[185, 117, 0, 203, 96, 245, 180, 186]); // flash_repay discriminator
        data.extend_from_slice(&amount.to_le_bytes());
        data.push(borrow_ix_index);

        Instruction {
            program_id: klend,
            accounts: vec![
                AccountMeta::new_readonly(self.payer.pubkey(), true),  // user_transfer_authority
                AccountMeta::new_readonly(market_authority, false),     // lending_market_authority
                AccountMeta::new_readonly(market, false),              // lending_market
                AccountMeta::new(*reserve, false),                     // reserve
                AccountMeta::new(*reserve_liquidity_vault, false),     // reserve_destination_liquidity
                AccountMeta::new(user_wsol_ata, false),                // user_source_liquidity
                AccountMeta::new(*fee_receiver, false),                // reserve_liquidity_fee_receiver
                AccountMeta::new_readonly(klend, false),               // referrer_token_state
                AccountMeta::new_readonly(klend, false),               // referrer_account
                AccountMeta::new_readonly(sysvar, false),              // sysvar_info
                AccountMeta::new_readonly(spl_token::id(), false),     // token_program
            ],
            data,
        }
    }

    /// Build a flash loan arbitrage transaction:
    /// [compute_limit, compute_price, flash_borrow, swap_ix..., flash_repay]
    pub fn build_flashloan_transaction(
        &self,
        swap_instructions: Vec<Instruction>,
        borrow_amount: u64,
        reserve: &Pubkey,
        reserve_vault: &Pubkey,
        fee_receiver: &Pubkey,
        recent_blockhash: Hash,
    ) -> Result<VersionedTransaction> {
        let mut instructions = Vec::with_capacity(swap_instructions.len() + 4);

        // Compute budget: use dynamic CU/fee when enabled, otherwise higher static limit for flash loans
        let budget_ixs = self.build_compute_budget_ixs(&swap_instructions);
        instructions.extend(budget_ixs);

        // idx 2: flash borrow
        let borrow_ix_index = instructions.len() as u8;
        instructions.push(self.build_flash_borrow_ix(
            borrow_amount, reserve, reserve_vault, fee_receiver,
        ));

        // idx 3..N: swap instructions
        instructions.extend(swap_instructions);

        // idx N+1: flash repay (include ~0.09% Kamino flash loan fee)
        let flash_loan_fee = borrow_amount / 1111 + 1; // ~0.09% rounded up
        let repay_amount = borrow_amount + flash_loan_fee;
        instructions.push(self.build_flash_repay_ix(
            repay_amount, borrow_ix_index, reserve, reserve_vault, fee_receiver,
        ));

        self.build_versioned_tx(&instructions, recent_blockhash)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_transaction_builder_creation() {
        let rpc = Arc::new(RpcClient::new("https://api.mainnet-beta.solana.com".to_string()));
        let payer = Arc::new(Keypair::new());
        let builder = TransactionBuilder::new(
            rpc, payer, 400_000, 10_000, vec![], false,
        );
        assert!(!builder.real_execution);
    }

    #[test]
    fn test_raydium_swap_data_encoding() {
        let data = TransactionBuilder::encode_raydium_swap_data(1000, 900);
        assert_eq!(data[0], 9);
        assert_eq!(data.len(), 1 + 8 + 8);
        let amount_in = u64::from_le_bytes(data[1..9].try_into().unwrap());
        let min_out = u64::from_le_bytes(data[9..17].try_into().unwrap());
        assert_eq!(amount_in, 1000);
        assert_eq!(min_out, 900);
    }

    #[test]
    fn test_pump_swap_ix_buy() {
        let ix = TransactionBuilder::build_pump_swap_ix(
            &Pubkey::new_unique(),
            &Pubkey::new_unique(),
            &Pubkey::new_unique(),
            &Pubkey::new_unique(),
            &Pubkey::new_unique(),
            &Pubkey::new_unique(),
            1000,
            900,
            true,
        );
        assert_eq!(ix.data[0..8], [102, 6, 61, 18, 1, 218, 235, 234]); // buy discriminator
    }

    #[test]
    fn test_pump_swap_ix_sell() {
        let ix = TransactionBuilder::build_pump_swap_ix(
            &Pubkey::new_unique(),
            &Pubkey::new_unique(),
            &Pubkey::new_unique(),
            &Pubkey::new_unique(),
            &Pubkey::new_unique(),
            &Pubkey::new_unique(),
            1000,
            900,
            false,
        );
        assert_eq!(ix.data[0..8], [51, 230, 133, 164, 1, 127, 131, 173]); // sell discriminator
    }

    #[test]
    fn test_user_ata_derivation() {
        let rpc = Arc::new(RpcClient::new("https://api.mainnet-beta.solana.com".to_string()));
        let payer = Arc::new(Keypair::new());
        let builder = TransactionBuilder::new(rpc, payer.clone(), 400_000, 10_000, vec![], false);

        let mint = Pubkey::new_unique();
        let ata = builder.get_user_ata(&mint);
        let expected = get_associated_token_address(&payer.pubkey(), &mint);
        assert_eq!(ata, expected);
    }

    #[test]
    fn test_raydium_amm_authority() {
        let authority = TransactionBuilder::raydium_amm_authority();
        // Should be deterministic
        let authority2 = TransactionBuilder::raydium_amm_authority();
        assert_eq!(authority, authority2);
    }

    #[test]
    fn test_whirlpool_tick_array_derivation() {
        let pool = Pubkey::new_unique();
        let program = whirlpool_program_id();
        let arrays = TransactionBuilder::derive_whirlpool_tick_arrays(
            &pool, &program, 100, 5632, true,
        );
        // All three should be different
        assert_ne!(arrays[0], arrays[1]);
        assert_ne!(arrays[1], arrays[2]);
    }

    #[test]
    fn test_jito_tip_account_selection() {
        // Verify all 8 tip accounts are valid pubkeys
        for addr in super::JITO_TIP_ACCOUNTS {
            Pubkey::from_str(addr).expect("Invalid Jito tip account pubkey");
        }
        // random_jito_tip_account should succeed
        let tip = TransactionBuilder::random_jito_tip_account().unwrap();
        assert_ne!(tip, Pubkey::default());
    }

    #[test]
    fn test_with_jito_configuration() {
        let rpc = Arc::new(RpcClient::new("https://api.mainnet-beta.solana.com".to_string()));
        let payer = Arc::new(Keypair::new());
        let builder = TransactionBuilder::new(rpc, payer, 400_000, 10_000, vec![], false)
            .with_jito(true, 50_000, "https://mainnet.block-engine.jito.wtf".to_string());
        assert!(builder.is_jito_enabled());
    }

    #[test]
    fn test_build_jito_bundle() {
        let rpc = Arc::new(RpcClient::new("https://api.mainnet-beta.solana.com".to_string()));
        let payer = Arc::new(Keypair::new());
        let builder = TransactionBuilder::new(rpc.clone(), payer, 400_000, 10_000, vec![], false)
            .with_jito(true, 50_000, "https://mainnet.block-engine.jito.wtf".to_string());

        let blockhash = solana_sdk::hash::Hash::new_unique();
        // A no-op instruction for testing
        let swap_ix = Instruction {
            program_id: Pubkey::new_unique(),
            accounts: vec![],
            data: vec![1, 2, 3],
        };

        let tx = builder.build_jito_bundle(vec![swap_ix], blockhash).unwrap();
        // Should have: compute limit + compute price + swap ix + tip transfer = 4 instructions
        let ix_count = match &tx.message {
            VersionedMessage::V0(m) => m.instructions.len(),
            VersionedMessage::Legacy(m) => m.instructions.len(),
        };
        assert_eq!(ix_count, 4);
    }

    #[test]
    fn test_submit_jito_bundle_demo_mode() {
        let rpc = Arc::new(RpcClient::new("https://api.mainnet-beta.solana.com".to_string()));
        let payer = Arc::new(Keypair::new());
        let builder = TransactionBuilder::new(rpc.clone(), payer, 400_000, 10_000, vec![], false)
            .with_jito(true, 50_000, "https://mainnet.block-engine.jito.wtf".to_string());

        let blockhash = solana_sdk::hash::Hash::new_unique();
        let swap_ix = Instruction {
            program_id: Pubkey::new_unique(),
            accounts: vec![],
            data: vec![1, 2, 3],
        };
        let tx = builder.build_jito_bundle(vec![swap_ix], blockhash).unwrap();

        // In demo mode (real_execution=false), should return demo bundle ID
        let result = builder.submit_jito_bundle(&tx).unwrap();
        assert!(result.starts_with("demo_jito_bundle_"));
    }

    #[test]
    fn test_build_versioned_tx_without_alts() {
        let rpc = Arc::new(RpcClient::new("https://api.mainnet-beta.solana.com".to_string()));
        let payer = Arc::new(Keypair::new());
        let builder = TransactionBuilder::new(rpc, payer, 400_000, 10_000, vec![], false);

        let blockhash = solana_sdk::hash::Hash::new_unique();
        let ix = Instruction {
            program_id: Pubkey::new_unique(),
            accounts: vec![],
            data: vec![1, 2, 3],
        };

        let tx = builder.build_swap_transaction(vec![ix], blockhash).unwrap();
        // Should produce a v0 message even without ALTs
        assert!(matches!(tx.message, VersionedMessage::V0(_)));
    }

    #[test]
    fn test_build_versioned_tx_with_alts() {
        let rpc = Arc::new(RpcClient::new("https://api.mainnet-beta.solana.com".to_string()));
        let payer = Arc::new(Keypair::new());

        // Create a mock ALT with some addresses
        let alt_key = Pubkey::new_unique();
        let alt_addresses: Vec<Pubkey> = (0..10).map(|_| Pubkey::new_unique()).collect();
        let alt = AddressLookupTableAccount {
            key: alt_key,
            addresses: alt_addresses.clone(),
        };

        let builder = TransactionBuilder::new(rpc, payer.clone(), 400_000, 10_000, vec![], false)
            .with_lookup_tables(vec![alt]);

        let blockhash = solana_sdk::hash::Hash::new_unique();
        // Build an instruction that references ALT addresses
        let ix = Instruction {
            program_id: Pubkey::new_unique(),
            accounts: alt_addresses.iter().map(|a| AccountMeta::new(*a, false)).collect(),
            data: vec![1, 2, 3],
        };

        let tx = builder.build_swap_transaction(vec![ix], blockhash).unwrap();
        match &tx.message {
            VersionedMessage::V0(m) => {
                // Should have address table lookups
                assert!(!m.address_table_lookups.is_empty(), "Expected ALT lookups in v0 message");
            }
            _ => panic!("Expected V0 message"),
        }
    }

    #[test]
    fn test_with_lookup_tables() {
        let rpc = Arc::new(RpcClient::new("https://api.mainnet-beta.solana.com".to_string()));
        let payer = Arc::new(Keypair::new());
        let alt = AddressLookupTableAccount {
            key: Pubkey::new_unique(),
            addresses: vec![Pubkey::new_unique()],
        };
        let builder = TransactionBuilder::new(rpc, payer, 400_000, 10_000, vec![], false)
            .with_lookup_tables(vec![alt]);
        assert_eq!(builder.address_lookup_tables.len(), 1);
    }
}
