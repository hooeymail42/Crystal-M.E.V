#![allow(dead_code)]
use solana_sdk::pubkey::Pubkey;
use std::str::FromStr;

pub fn lifinity_program_id() -> Pubkey {
    Pubkey::from_str("EewxydAPCCVuNEyrVN68PuSYdQ7wKn27V9Gjeoi8dy3S").unwrap()
}

pub const LIFINITY_FEE_BPS: u64 = 30; // 0.3% fee
