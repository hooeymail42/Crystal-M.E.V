use anyhow::{Result, anyhow};
use rand::Rng;
use solana_client::rpc_client::RpcClient;
use solana_sdk::{
    compute_budget::ComputeBudgetInstruction,
    hash::Hash,
    instruction::{AccountMeta, Instruction},
    message::Message,
    pubkey::Pubkey,
    signature::{Keypair, Signer},
    system_instruction,
    transaction::Transaction,
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
        }
    }

    /// Configure Jito MEV bundle submission settings.
    pub fn with_jito(mut self, enabled: bool, tip_lamports: u64, block_engine_url: String) -> Self {
        self.jito_enabled = enabled;
        self.jito_tip_lamports = tip_lamports;
        self.jito_block_engine_url = block_engine_url;
        self
    }

    /// Returns true if Jito bundle submission is enabled.
    pub fn is_jito_enabled(&self) -> bool {
        self.jito_enabled
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

    /// Build a transaction with compute budget instructions prepended
    pub fn build_swap_transaction(
        &self,
        swap_instructions: Vec<Instruction>,
        recent_blockhash: Hash,
    ) -> Result<Transaction> {
        let mut instructions = Vec::with_capacity(swap_instructions.len() + 2);

        instructions.push(
            ComputeBudgetInstruction::set_compute_unit_limit(self.compute_unit_limit)
        );

        if self.priority_fee_lamports > 0 {
            let microlamports_per_cu = (self.priority_fee_lamports * 1_000_000) / self.compute_unit_limit as u64;
            instructions.push(
                ComputeBudgetInstruction::set_compute_unit_price(microlamports_per_cu)
            );
        }

        instructions.extend(swap_instructions);

        let message = Message::new(&instructions, Some(&self.payer.pubkey()));
        let mut tx = Transaction::new_unsigned(message);
        tx.sign(&[self.payer.as_ref()], recent_blockhash);

        Ok(tx)
    }

    /// Simulate a transaction without sending it
    pub fn simulate(&self, tx: &Transaction) -> Result<bool> {
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
    pub fn send_and_confirm(&self, tx: &Transaction) -> Result<String> {
        if !self.real_execution {
            info!("[DEMO] Would send transaction with {} instructions", tx.message.instructions.len());
            return Ok(format!("demo_sig_{}", self.payer.pubkey()));
        }

        let sig = self.rpc.send_and_confirm_transaction(tx)?;
        info!("Transaction confirmed: {}", sig);
        Ok(sig.to_string())
    }

    /// Submit transaction across multiple RPC endpoints in parallel
    pub async fn parallel_submit(&self, tx: &Transaction) -> Result<String> {
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
    ) -> Result<Transaction> {
        let mut instructions = Vec::with_capacity(swap_instructions.len() + 3);

        // Compute budget instructions (same as regular path)
        instructions.push(
            ComputeBudgetInstruction::set_compute_unit_limit(self.compute_unit_limit)
        );
        if self.priority_fee_lamports > 0 {
            let microlamports_per_cu =
                (self.priority_fee_lamports * 1_000_000) / self.compute_unit_limit as u64;
            instructions.push(
                ComputeBudgetInstruction::set_compute_unit_price(microlamports_per_cu)
            );
        }

        // Swap instructions
        instructions.extend(swap_instructions);

        // Jito tip transfer
        let tip_account = Self::random_jito_tip_account()?;
        instructions.push(
            system_instruction::transfer(&self.payer.pubkey(), &tip_account, self.jito_tip_lamports)
        );

        let message = Message::new(&instructions, Some(&self.payer.pubkey()));
        let mut tx = Transaction::new_unsigned(message);
        tx.sign(&[self.payer.as_ref()], recent_blockhash);

        info!(
            "Built Jito bundle TX: {} instructions, tip={} lamports to {}",
            tx.message.instructions.len(),
            self.jito_tip_lamports,
            tip_account,
        );

        Ok(tx)
    }

    /// Submit a signed transaction as a Jito MEV bundle via the block engine API.
    /// Returns the bundle ID on success.
    pub fn submit_jito_bundle(&self, tx: &Transaction) -> Result<String> {
        if !self.real_execution {
            info!(
                "[DEMO] Would submit Jito bundle with {} instructions, tip={} lamports",
                tx.message.instructions.len(),
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
    ) -> Result<Transaction> {
        let mut instructions = Vec::with_capacity(swap_instructions.len() + 4);

        // idx 0: compute budget (higher for flash loan TXs)
        instructions.push(
            ComputeBudgetInstruction::set_compute_unit_limit(600_000)
        );

        // idx 1: priority fee
        if self.priority_fee_lamports > 0 {
            let microlamports_per_cu = (self.priority_fee_lamports * 1_000_000) / 600_000;
            instructions.push(
                ComputeBudgetInstruction::set_compute_unit_price(microlamports_per_cu)
            );
        }

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

        let message = Message::new(&instructions, Some(&self.payer.pubkey()));
        let mut tx = Transaction::new_unsigned(message);
        tx.sign(&[self.payer.as_ref()], recent_blockhash);

        Ok(tx)
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
        assert_eq!(tx.message.instructions.len(), 4);
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
}
