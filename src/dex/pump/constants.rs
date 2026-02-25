#![allow(dead_code)]
use solana_sdk::pubkey::Pubkey;
use std::str::FromStr;

pub fn pump_program_id() -> Pubkey {
    Pubkey::from_str("6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P").unwrap()
}

pub const PUMP_FEE_BPS: u64 = 100; // 1% fee
