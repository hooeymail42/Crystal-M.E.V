use solana_sdk::pubkey::Pubkey;
use std::str::FromStr;

pub fn solfi_program_id() -> Pubkey {
    Pubkey::from_str("SoLFiHG9TfgtdUXUjWAxi3LtvYuFyDLVhBWxdMZxyCe").unwrap()
}

pub const SOLFI_FEE_BPS: u64 = 30; // 0.3% default fee
