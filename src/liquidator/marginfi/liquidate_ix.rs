//! Builder for the MarginFi `lending_account_liquidate` instruction.
//!
//! # ⚠️ Account list verification
//!
//! The account ordering below follows the MarginFi v2 IDL for
//! `lending_account_liquidate`. The set of trailing "remaining accounts" (the
//! bank + oracle pairs MarginFi walks to recompute both accounts' health) is
//! version-sensitive and must be confirmed against a successful mainnet
//! `simulateTransaction` before real execution. The struct-based API here makes
//! it easy to adjust the list in one place.

use solana_sdk::{
    instruction::{AccountMeta, Instruction},
    pubkey::Pubkey,
};

use super::constants::{liquidity_vault_authority, marginfi_program_id, LIQUIDATE_DISCRIMINATOR};

/// All fixed accounts required by `lending_account_liquidate`.
#[derive(Debug, Clone)]
pub struct LiquidateAccounts {
    pub group: Pubkey,
    /// Bank of the collateral being seized.
    pub asset_bank: Pubkey,
    /// Bank of the debt being repaid.
    pub liab_bank: Pubkey,
    /// The liquidator's own MarginFi account (receives seized collateral).
    pub liquidator_marginfi_account: Pubkey,
    /// Authority (signer) of the liquidator's MarginFi account.
    pub signer: Pubkey,
    /// The unhealthy account being liquidated.
    pub liquidatee_marginfi_account: Pubkey,
    /// Liability bank's liquidity vault (source of repay / debt reduction).
    pub liab_liquidity_vault: Pubkey,
    /// Liability bank's insurance vault (receives insurance fee).
    pub liab_insurance_vault: Pubkey,
    /// SPL token program for the liability mint.
    pub token_program: Pubkey,
}

/// A trailing (bank, oracle) pair MarginFi needs to re-price an account's
/// positions during the health check.
#[derive(Debug, Clone)]
pub struct HealthAccount {
    pub bank: Pubkey,
    pub oracle: Pubkey,
}

/// Build the `lending_account_liquidate` instruction.
///
/// `asset_amount` is the amount of collateral (in the asset bank's native
/// units) the liquidator wants to seize. `health_accounts` are the bank/oracle
/// pairs for every active balance across the liquidator and liquidatee accounts,
/// plus the two banks being acted on — supply the liab oracle and asset oracle
/// first, matching the IDL's expectation, followed by the rest.
pub fn build_liquidate_ix(
    accounts: &LiquidateAccounts,
    asset_amount: u64,
    health_accounts: &[HealthAccount],
) -> Instruction {
    let program_id = marginfi_program_id();
    let liab_vault_authority = liquidity_vault_authority(&accounts.liab_bank);

    let mut data = Vec::with_capacity(16);
    data.extend_from_slice(&LIQUIDATE_DISCRIMINATOR);
    data.extend_from_slice(&asset_amount.to_le_bytes());

    let mut metas = vec![
        AccountMeta::new_readonly(accounts.group, false), // 0: marginfi_group
        AccountMeta::new(accounts.asset_bank, false),     // 1: asset_bank (w)
        AccountMeta::new(accounts.liab_bank, false),      // 2: liab_bank (w)
        AccountMeta::new(accounts.liquidator_marginfi_account, false), // 3 (w)
        AccountMeta::new_readonly(accounts.signer, true), // 4: signer
        AccountMeta::new(accounts.liquidatee_marginfi_account, false), // 5 (w)
        AccountMeta::new(liab_vault_authority, false),    // 6: liab vault authority (w)
        AccountMeta::new(accounts.liab_liquidity_vault, false), // 7 (w)
        AccountMeta::new(accounts.liab_insurance_vault, false), // 8 (w)
        AccountMeta::new_readonly(accounts.token_program, false), // 9
    ];

    // Remaining accounts: (bank, oracle) pairs for the health re-check. Banks
    // are readonly-writable per MarginFi's expectation; oracles are readonly.
    for ha in health_accounts {
        metas.push(AccountMeta::new(ha.bank, false));
        metas.push(AccountMeta::new_readonly(ha.oracle, false));
    }

    Instruction {
        program_id,
        accounts: metas,
        data,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use solana_sdk::signature::{Keypair, Signer};

    fn dummy() -> LiquidateAccounts {
        LiquidateAccounts {
            group: Keypair::new().pubkey(),
            asset_bank: Keypair::new().pubkey(),
            liab_bank: Keypair::new().pubkey(),
            liquidator_marginfi_account: Keypair::new().pubkey(),
            signer: Keypair::new().pubkey(),
            liquidatee_marginfi_account: Keypair::new().pubkey(),
            liab_liquidity_vault: Keypair::new().pubkey(),
            liab_insurance_vault: Keypair::new().pubkey(),
            token_program: spl_token::id(),
        }
    }

    #[test]
    fn data_layout_is_discriminator_plus_u64() {
        let ix = build_liquidate_ix(&dummy(), 12345u64, &[]);
        assert_eq!(&ix.data[..8], &LIQUIDATE_DISCRIMINATOR);
        assert_eq!(
            u64::from_le_bytes(ix.data[8..16].try_into().unwrap()),
            12345u64
        );
    }

    #[test]
    fn fixed_accounts_present_and_signer_marked() {
        let a = dummy();
        let ix = build_liquidate_ix(&a, 1, &[]);
        assert_eq!(ix.accounts.len(), 10);
        // signer is index 4 and must be a signer.
        assert!(ix.accounts[4].is_signer);
        assert_eq!(ix.accounts[4].pubkey, a.signer);
        assert_eq!(ix.program_id, marginfi_program_id());
    }

    #[test]
    fn remaining_accounts_appended_as_pairs() {
        let a = dummy();
        let ha = vec![
            HealthAccount {
                bank: Keypair::new().pubkey(),
                oracle: Keypair::new().pubkey(),
            },
            HealthAccount {
                bank: Keypair::new().pubkey(),
                oracle: Keypair::new().pubkey(),
            },
        ];
        let ix = build_liquidate_ix(&a, 1, &ha);
        assert_eq!(ix.accounts.len(), 10 + 4);
    }
}
