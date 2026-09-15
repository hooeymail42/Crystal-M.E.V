//! Scans MarginFi accounts for the configured group and loads the bank +
//! oracle data needed to evaluate their health.

use std::collections::HashMap;
use std::sync::Arc;

use anyhow::{Context, Result};
use solana_account_decoder::UiAccountEncoding;
use solana_client::rpc_client::RpcClient;
use solana_client::rpc_config::{RpcAccountInfoConfig, RpcProgramAccountsConfig};
use solana_client::rpc_filter::{Memcmp, RpcFilterType};
use solana_sdk::pubkey::Pubkey;
use tracing::{debug, warn};

use super::marginfi::account::{layout, Bank, MarginfiAccount};
use super::oracle::{parse_price, OraclePrice};

/// Fetches MarginFi accounts and their supporting bank/oracle data over RPC.
pub struct MarginfiScanner {
    rpc: Arc<RpcClient>,
    program: Pubkey,
    group: Pubkey,
    /// Decoded banks, keyed by bank pubkey. Banks change slowly (share values
    /// drift), so callers may keep this warm and refresh periodically.
    bank_cache: HashMap<Pubkey, Bank>,
}

impl MarginfiScanner {
    pub fn new(rpc: Arc<RpcClient>, program: Pubkey, group: Pubkey) -> Self {
        Self {
            rpc,
            program,
            group,
            bank_cache: HashMap::new(),
        }
    }

    /// Fetch and decode every MarginFi account belonging to the configured
    /// group. Undecodable accounts are skipped with a warning rather than
    /// failing the whole scan.
    pub fn scan_accounts(&self) -> Result<Vec<(Pubkey, MarginfiAccount)>> {
        // memcmp filter on the `group` field so the RPC only returns accounts in
        // our group. The base58-encoded bytes are the group pubkey.
        let filters = vec![RpcFilterType::Memcmp(Memcmp::new_base58_encoded(
            layout::ACCT_GROUP,
            &self.group.to_bytes(),
        ))];

        let config = RpcProgramAccountsConfig {
            filters: Some(filters),
            account_config: RpcAccountInfoConfig {
                encoding: Some(UiAccountEncoding::Base64),
                ..Default::default()
            },
            ..Default::default()
        };

        let raw = self
            .rpc
            .get_program_accounts_with_config(&self.program, config)
            .context("get_program_accounts for MarginFi group")?;

        let mut out = Vec::with_capacity(raw.len());
        for (pk, acct) in raw {
            match MarginfiAccount::from_account_data(&acct.data) {
                Ok(ma) => out.push((pk, ma)),
                Err(e) => debug!("skip undecodable marginfi account {}: {}", pk, e),
            }
        }
        debug!("scanned {} marginfi accounts in group {}", out.len(), self.group);
        Ok(out)
    }

    /// Load and cache a bank by pubkey.
    pub fn load_bank(&mut self, bank_pk: &Pubkey) -> Result<Bank> {
        if let Some(b) = self.bank_cache.get(bank_pk) {
            return Ok(b.clone());
        }
        let acct = self
            .rpc
            .get_account(bank_pk)
            .with_context(|| format!("get_account for bank {}", bank_pk))?;
        let bank = Bank::from_account_data(&acct.data)
            .with_context(|| format!("decode bank {}", bank_pk))?;
        self.bank_cache.insert(*bank_pk, bank.clone());
        Ok(bank)
    }

    /// Force-refresh all cached banks (share values move over time).
    pub fn refresh_banks(&mut self) {
        let keys: Vec<Pubkey> = self.bank_cache.keys().copied().collect();
        for k in keys {
            self.bank_cache.remove(&k);
            if let Err(e) = self.load_bank(&k) {
                warn!("failed to refresh bank {}: {}", k, e);
            }
        }
    }

    /// Load the current oracle price for a bank's primary oracle.
    pub fn load_price(&self, oracle_pk: &Pubkey) -> Result<OraclePrice> {
        let acct = self
            .rpc
            .get_account(oracle_pk)
            .with_context(|| format!("get_account for oracle {}", oracle_pk))?;
        parse_price(&acct.data).with_context(|| format!("parse oracle {}", oracle_pk))
    }
}
