use solana_sdk::pubkey::Pubkey;
use std::str::FromStr;

pub fn phoenix_program_id() -> Pubkey {
    Pubkey::from_str("PhoeNiXZ8ByJGLkxNfZRnkUfjvmuYqLR89jjFHGqdXY").unwrap()
}

pub const PHOENIX_TAKER_FEE_BPS: u64 = 10; // 0.1% taker fee
