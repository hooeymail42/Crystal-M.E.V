use solana_sdk::pubkey::Pubkey;
use std::str::FromStr;

pub const POOL_TICK_ARRAY_BITMAP_SEED: &str = "pool_tick_array_bitmap";

pub fn raydium_amm_program_id() -> Pubkey {
    Pubkey::from_str("675kPX9MHTjS2zt1qfr1NYHuzeLXfQM9H24wFSUt1Mp8").unwrap()
}

pub fn raydium_cp_amm_program_id() -> Pubkey {
    Pubkey::from_str("CPMMoo8L3F4NbTegBCKVNunggL7H1ZpdTHKxQB5qKP1C").unwrap()
}

pub fn raydium_clmm_program_id() -> Pubkey {
    Pubkey::from_str("CAMMCzo5YL8w4VFF8KVHrK22GGUsp5VTaW7grrKgrWqK").unwrap()
}
