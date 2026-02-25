#![allow(dead_code)]
use solana_sdk::pubkey::Pubkey;
use std::str::FromStr;

pub fn heaven_program_id() -> Pubkey {
    Pubkey::from_str("HEAVqLi4RAGQW6YMPg4PNo3xBLqKNWeCohcaA9gaULRt").unwrap()
}

pub const HEAVEN_BASE_FEE_BPS: u64 = 100; // 1% base fee
pub const SNIPER_TAX_DURATION_SECS: i64 = 6;
