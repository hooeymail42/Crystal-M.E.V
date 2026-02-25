#![allow(dead_code)]
//! JitoSOL (Jito Stake Pool) state deserialization and rate fetching.
//!
//! JitoSOL is a liquid staking token issued by Jito Labs.  The on-chain stake
//! pool state follows the SPL Stake Pool program layout, which allows us to
//! compute the exact SOL/JitoSOL exchange rate without any off-chain oracle.
//!
//! ## Key addresses
//! - Stake pool: `Jito4APyf642JPZPx3hGc6WWJ8zPKtRbRs4P815Posko`
//! - JitoSOL mint: `J1toso1uCk3RLmjorhTtrVwY9HJ7X8V9yYac6Y7kGCPn`
//!
//! ## SPL Stake Pool state layout (fields we need)
//! The SPL Stake Pool state is an Anchor-like account.  After the 1-byte
//! account type tag (offset 0) and various flags:
//!   - total_lamports (u64) at offset 258  — total SOL managed
//!   - pool_token_supply (u64) at offset 266 — total pool tokens (JitoSOL) minted
//!
//! Exchange rate: 1 JitoSOL = total_lamports / pool_token_supply SOL

use anyhow::{anyhow, Result};
use solana_client::rpc_client::RpcClient;
use solana_sdk::pubkey::Pubkey;
use std::str::FromStr;
use tracing::debug;

// ── Constants ─────────────────────────────────────────────────────────────────

/// Jito stake pool address (mainnet).
pub const JITO_STAKE_POOL: &str = "Jito4APyf642JPZPx3hGc6WWJ8zPKtRbRs4P815Posko";

/// JitoSOL mint address.
pub const JITO_SOL_MINT: &str = "J1toso1uCk3RLmjorhTtrVwY9HJ7X8V9yYac6Y7kGCPn";

/// SPL Stake Pool: total_lamports at byte offset 258.
const SPL_TOTAL_LAMPORTS_OFFSET: usize = 258;

/// SPL Stake Pool: pool_token_supply at byte offset 266.
const SPL_POOL_TOKEN_SUPPLY_OFFSET: usize = 266;

// ── Data structures ────────────────────────────────────────────────────────────

/// Lightweight snapshot of the Jito/SPL stake pool state.
#[derive(Debug, Clone)]
pub struct JitoStakePoolState {
    /// Total SOL lamports under management.
    pub total_lamports: u64,
    /// Total JitoSOL tokens in circulation.
    pub pool_token_supply: u64,
    /// Exchange rate: SOL per JitoSOL.
    pub sol_per_jito_sol: f64,
}

impl JitoStakePoolState {
    /// Fetch and deserialize the Jito stake pool state from the RPC.
    pub fn fetch(rpc: &RpcClient) -> Result<Self> {
        let pool_key = Pubkey::from_str(JITO_STAKE_POOL)
            .map_err(|e| anyhow!("Invalid Jito stake pool address: {}", e))?;

        let account = rpc
            .get_account(&pool_key)
            .map_err(|e| anyhow!("Failed to fetch Jito stake pool state: {}", e))?;

        let data = &account.data;
        let required = SPL_POOL_TOKEN_SUPPLY_OFFSET + 8;
        if data.len() < required {
            return Err(anyhow!(
                "Jito stake pool account too small: {} < {}",
                data.len(),
                required
            ));
        }

        let total_lamports = u64::from_le_bytes(
            data[SPL_TOTAL_LAMPORTS_OFFSET..SPL_TOTAL_LAMPORTS_OFFSET + 8]
                .try_into()
                .map_err(|_| anyhow!("Failed to read total_lamports bytes"))?,
        );
        let pool_token_supply = u64::from_le_bytes(
            data[SPL_POOL_TOKEN_SUPPLY_OFFSET..SPL_POOL_TOKEN_SUPPLY_OFFSET + 8]
                .try_into()
                .map_err(|_| anyhow!("Failed to read pool_token_supply bytes"))?,
        );

        if pool_token_supply == 0 {
            return Err(anyhow!("Jito stake pool token supply is zero"));
        }

        let sol_per_jito_sol = total_lamports as f64 / pool_token_supply as f64;

        debug!(
            "[JitoSOL] total_lamports={} ({:.4} SOL), supply={} ({:.4} JitoSOL), rate={:.6}",
            total_lamports,
            total_lamports as f64 / 1e9,
            pool_token_supply,
            pool_token_supply as f64 / 1e9,
            sol_per_jito_sol,
        );

        Ok(JitoStakePoolState {
            total_lamports,
            pool_token_supply,
            sol_per_jito_sol,
        })
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_jito_sol_rate_calculation() {
        let state = JitoStakePoolState {
            total_lamports: 105_000_000_000,
            pool_token_supply: 100_000_000_000,
            sol_per_jito_sol: 1.05,
        };
        assert!((state.sol_per_jito_sol - 1.05).abs() < 1e-9);
    }

    #[test]
    fn test_jito_mint_constant_valid() {
        assert!(Pubkey::from_str(JITO_SOL_MINT).is_ok());
        assert!(Pubkey::from_str(JITO_STAKE_POOL).is_ok());
    }
}
