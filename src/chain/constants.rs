use solana_sdk::pubkey::Pubkey;
use std::str::FromStr;

pub const SOL_MINT: &str = "So11111111111111111111111111111111111111112";

#[allow(dead_code)]
pub fn sol_mint() -> Pubkey {
    Pubkey::from_str(SOL_MINT).unwrap()
}
