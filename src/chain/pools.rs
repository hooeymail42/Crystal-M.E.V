#![allow(dead_code)]
use crate::{
    chain::constants::SOL_MINT,
    dex::raydium::{POOL_TICK_ARRAY_BITMAP_SEED, raydium_clmm_program_id},
};
use solana_sdk::pubkey::Pubkey;
use std::str::FromStr;

pub trait PoolData {
    fn pool_address(&self) -> &Pubkey;
    fn token_vault(&self) -> &Pubkey;
    fn sol_vault(&self) -> &Pubkey;
    fn token_mint(&self) -> &Pubkey;
    fn base_mint(&self) -> &Pubkey;
    fn get_dex_name(&self) -> &str;
}

#[derive(Debug, Clone)]
pub enum Pool {
    Raydium(RaydiumPool),
    RaydiumCp(RaydiumCpPool),
    Pump(PumpPool),
    Dlmm(DlmmPool),
    Whirlpool(WhirlpoolPool),
    RaydiumClmm(RaydiumClmmPool),
    MeteoraDAmm(MeteoraDAmmPool),
    Solfi(SolfiPool),
    MeteoraDAmmV2(MeteoraDAmmV2Pool),
    Vertigo(VertigoPool),
    Phoenix(PhoenixPool),
    Lifinity(LifinityPool),
    Heaven(HeavenPool),
}

impl PoolData for Pool {
    fn pool_address(&self) -> &Pubkey {
        match self {
            Pool::Raydium(pool) => &pool.pool,
            Pool::RaydiumCp(pool) => &pool.pool,
            Pool::Pump(pool) => &pool.pool,
            Pool::Dlmm(pool) => &pool.pair,
            Pool::Whirlpool(pool) => &pool.pool,
            Pool::RaydiumClmm(pool) => &pool.pool,
            Pool::MeteoraDAmm(pool) => &pool.pool,
            Pool::Solfi(pool) => &pool.pool,
            Pool::MeteoraDAmmV2(pool) => &pool.pool,
            Pool::Vertigo(pool) => &pool.pool,
            Pool::Phoenix(pool) => &pool.pool,
            Pool::Lifinity(pool) => &pool.pool,
            Pool::Heaven(pool) => &pool.pool,
        }
    }

    fn token_vault(&self) -> &Pubkey {
        match self {
            Pool::Raydium(pool) => &pool.token_vault,
            Pool::RaydiumCp(pool) => &pool.token_vault,
            Pool::Pump(pool) => &pool.token_vault,
            Pool::Dlmm(pool) => &pool.token_vault,
            Pool::Whirlpool(pool) => &pool.x_vault, // Assuming x_vault is token_vault
            Pool::RaydiumClmm(pool) => &pool.x_vault,
            Pool::MeteoraDAmm(pool) => &pool.token_x_vault,
            Pool::Solfi(pool) => &pool.token_x_vault,
            Pool::MeteoraDAmmV2(pool) => &pool.token_x_vault,
            Pool::Vertigo(pool) => &pool.token_x_vault,
            Pool::Phoenix(pool) => &pool.base_vault,
            Pool::Lifinity(pool) => &pool.token_a_vault,
            Pool::Heaven(pool) => &pool.token_x_vault,
        }
    }

    fn sol_vault(&self) -> &Pubkey {
        match self {
            Pool::Raydium(pool) => &pool.sol_vault,
            Pool::RaydiumCp(pool) => &pool.sol_vault,
            Pool::Pump(pool) => &pool.sol_vault,
            Pool::Dlmm(pool) => &pool.sol_vault,
            Pool::Whirlpool(pool) => &pool.y_vault, // Assuming y_vault is sol_vault
            Pool::RaydiumClmm(pool) => &pool.y_vault,
            Pool::MeteoraDAmm(pool) => &pool.token_sol_vault,
            Pool::Solfi(pool) => &pool.token_sol_vault,
            Pool::MeteoraDAmmV2(pool) => &pool.token_sol_vault,
            Pool::Vertigo(pool) => &pool.token_sol_vault,
            Pool::Phoenix(pool) => &pool.quote_vault,
            Pool::Lifinity(pool) => &pool.token_b_vault,
            Pool::Heaven(pool) => &pool.token_sol_vault,
        }
    }

    fn token_mint(&self) -> &Pubkey {
        match self {
            Pool::Raydium(pool) => &pool.token_mint,
            Pool::RaydiumCp(pool) => &pool.token_mint,
            Pool::Pump(pool) => &pool.token_mint,
            Pool::Dlmm(pool) => &pool.token_mint,
            Pool::Whirlpool(pool) => &pool.token_mint,
            Pool::RaydiumClmm(pool) => &pool.token_mint,
            Pool::MeteoraDAmm(pool) => &pool.token_mint,
            Pool::Solfi(pool) => &pool.token_mint,
            Pool::MeteoraDAmmV2(pool) => &pool.token_mint,
            Pool::Vertigo(pool) => &pool.token_mint,
            Pool::Phoenix(pool) => &pool.token_mint,
            Pool::Lifinity(pool) => &pool.token_mint,
            Pool::Heaven(pool) => &pool.token_mint,
        }
    }

    fn base_mint(&self) -> &Pubkey {
        match self {
            Pool::Raydium(pool) => &pool.base_mint,
            Pool::RaydiumCp(pool) => &pool.base_mint,
            Pool::Pump(pool) => &pool.base_mint,
            Pool::Dlmm(pool) => &pool.base_mint,
            Pool::Whirlpool(pool) => &pool.base_mint,
            Pool::RaydiumClmm(pool) => &pool.base_mint,
            Pool::MeteoraDAmm(pool) => &pool.base_mint,
            Pool::Solfi(pool) => &pool.base_mint,
            Pool::MeteoraDAmmV2(pool) => &pool.base_mint,
            Pool::Vertigo(pool) => &pool.base_mint,
            Pool::Phoenix(pool) => &pool.base_mint,
            Pool::Lifinity(pool) => &pool.base_mint,
            Pool::Heaven(pool) => &pool.base_mint,
        }
    }

    fn get_dex_name(&self) -> &str {
        match self {
            Pool::Raydium(_) => "Raydium",
            Pool::RaydiumCp(_) => "RaydiumCp",
            Pool::Pump(_) => "Pump",
            Pool::Dlmm(_) => "DLMM",
            Pool::Whirlpool(_) => "Whirlpool",
            Pool::RaydiumClmm(_) => "RaydiumClmm",
            Pool::MeteoraDAmm(_) => "MeteoraDAmm",
            Pool::Solfi(_) => "Solfi",
            Pool::MeteoraDAmmV2(_) => "MeteoraDAmmV2",
            Pool::Vertigo(_) => "Vertigo",
            Pool::Phoenix(_) => "Phoenix",
            Pool::Lifinity(_) => "Lifinity",
            Pool::Heaven(_) => "Heaven",
        }
    }
}

#[derive(Debug, Clone)]
pub struct RaydiumPool {
    pub pool: Pubkey,
    pub token_vault: Pubkey,
    pub sol_vault: Pubkey,
    pub token_mint: Pubkey,
    pub base_mint: Pubkey,
}

impl PoolData for RaydiumPool {
    fn pool_address(&self) -> &Pubkey { &self.pool }
    fn token_vault(&self) -> &Pubkey { &self.token_vault }
    fn sol_vault(&self) -> &Pubkey { &self.sol_vault }
    fn token_mint(&self) -> &Pubkey { &self.token_mint }
    fn base_mint(&self) -> &Pubkey { &self.base_mint }
    fn get_dex_name(&self) -> &str { "Raydium" }
}

#[derive(Debug, Clone)]
pub struct RaydiumCpPool {
    pub pool: Pubkey,
    pub token_vault: Pubkey,
    pub sol_vault: Pubkey,
    pub amm_config: Pubkey,
    pub observation: Pubkey,
    pub token_mint: Pubkey,
    pub base_mint: Pubkey,
}

impl PoolData for RaydiumCpPool {
    fn pool_address(&self) -> &Pubkey { &self.pool }
    fn token_vault(&self) -> &Pubkey { &self.token_vault }
    fn sol_vault(&self) -> &Pubkey { &self.sol_vault }
    fn token_mint(&self) -> &Pubkey { &self.token_mint }
    fn base_mint(&self) -> &Pubkey { &self.base_mint }
    fn get_dex_name(&self) -> &str { "RaydiumCp" }
}

#[derive(Debug, Clone)]
pub struct PumpPool {
    pub pool: Pubkey,
    pub token_vault: Pubkey,
    pub sol_vault: Pubkey,
    pub fee_token_wallet: Pubkey,
    pub coin_creator_vault_ata: Pubkey,
    pub coin_creator_vault_authority: Pubkey,
    pub token_mint: Pubkey,
    pub base_mint: Pubkey,
}

impl PoolData for PumpPool {
    fn pool_address(&self) -> &Pubkey { &self.pool }
    fn token_vault(&self) -> &Pubkey { &self.token_vault }
    fn sol_vault(&self) -> &Pubkey { &self.sol_vault }
    fn token_mint(&self) -> &Pubkey { &self.token_mint }
    fn base_mint(&self) -> &Pubkey { &self.base_mint }
    fn get_dex_name(&self) -> &str { "Pump" }
}

#[derive(Debug, Clone)]
pub struct DlmmPool {
    pub pair: Pubkey,
    pub token_vault: Pubkey,
    pub sol_vault: Pubkey,
    pub oracle: Pubkey,
    pub bin_arrays: Vec<Pubkey>,
    pub memo_program: Option<Pubkey>, // For Token 2022 support
    pub token_mint: Pubkey,
    pub base_mint: Pubkey,
}

impl PoolData for DlmmPool {
    fn pool_address(&self) -> &Pubkey { &self.pair }
    fn token_vault(&self) -> &Pubkey { &self.token_vault }
    fn sol_vault(&self) -> &Pubkey { &self.sol_vault }
    fn token_mint(&self) -> &Pubkey { &self.token_mint }
    fn base_mint(&self) -> &Pubkey { &self.base_mint }
    fn get_dex_name(&self) -> &str { "DLMM" }
}

#[derive(Debug, Clone)]
pub struct WhirlpoolPool {
    pub pool: Pubkey,
    pub oracle: Pubkey,
    pub x_vault: Pubkey,
    pub y_vault: Pubkey,
    pub tick_arrays: Vec<Pubkey>,
    pub memo_program: Option<Pubkey>, // For Token 2022 support
    pub token_mint: Pubkey,
    pub base_mint: Pubkey,
}

impl PoolData for WhirlpoolPool {
    fn pool_address(&self) -> &Pubkey { &self.pool }
    fn token_vault(&self) -> &Pubkey { &self.x_vault }
    fn sol_vault(&self) -> &Pubkey { &self.y_vault }
    fn token_mint(&self) -> &Pubkey { &self.token_mint }
    fn base_mint(&self) -> &Pubkey { &self.base_mint }
    fn get_dex_name(&self) -> &str { "Whirlpool" }
}

#[derive(Debug, Clone)]
pub struct RaydiumClmmPool {
    pub pool: Pubkey,
    pub amm_config: Pubkey,
    pub observation_state: Pubkey,
    pub bitmap_extension: Pubkey,
    pub x_vault: Pubkey,
    pub y_vault: Pubkey,
    pub tick_arrays: Vec<Pubkey>,
    pub memo_program: Option<Pubkey>, // For Token 2022 support
    pub token_mint: Pubkey,
    pub base_mint: Pubkey,
}

impl PoolData for RaydiumClmmPool {
    fn pool_address(&self) -> &Pubkey { &self.pool }
    fn token_vault(&self) -> &Pubkey { &self.x_vault } // Assuming x_vault is token_vault
    fn sol_vault(&self) -> &Pubkey { &self.y_vault } // Assuming y_vault is sol_vault
    fn token_mint(&self) -> &Pubkey { &self.token_mint }
    fn base_mint(&self) -> &Pubkey { &self.base_mint }
    fn get_dex_name(&self) -> &str { "RaydiumClmm" }
}

#[derive(Debug, Clone)]
pub struct MeteoraDAmmPool {
    pub pool: Pubkey,
    pub token_x_vault: Pubkey,
    pub token_sol_vault: Pubkey,
    pub token_x_token_vault: Pubkey,
    pub token_sol_token_vault: Pubkey,
    pub token_x_lp_mint: Pubkey,
    pub token_sol_lp_mint: Pubkey,
    pub token_x_pool_lp: Pubkey,
    pub token_sol_pool_lp: Pubkey,
    pub admin_token_fee_x: Pubkey,
    pub admin_token_fee_sol: Pubkey,
    pub token_mint: Pubkey,
    pub base_mint: Pubkey,
}

impl PoolData for MeteoraDAmmPool {
    fn pool_address(&self) -> &Pubkey { &self.pool }
    fn token_vault(&self) -> &Pubkey { &self.token_x_vault }
    fn sol_vault(&self) -> &Pubkey { &self.token_sol_vault }
    fn token_mint(&self) -> &Pubkey { &self.token_mint }
    fn base_mint(&self) -> &Pubkey { &self.base_mint }
    fn get_dex_name(&self) -> &str { "MeteoraDAmm" }
}

#[derive(Debug, Clone)]
pub struct SolfiPool {
    pub pool: Pubkey,
    pub token_x_vault: Pubkey,
    pub token_sol_vault: Pubkey,
    pub token_mint: Pubkey,
    pub base_mint: Pubkey,
}

impl PoolData for SolfiPool {
    fn pool_address(&self) -> &Pubkey { &self.pool }
    fn token_vault(&self) -> &Pubkey { &self.token_x_vault }
    fn sol_vault(&self) -> &Pubkey { &self.token_sol_vault }
    fn token_mint(&self) -> &Pubkey { &self.token_mint }
    fn base_mint(&self) -> &Pubkey { &self.base_mint }
    fn get_dex_name(&self) -> &str { "Solfi" }
}

#[derive(Debug, Clone)]
pub struct MeteoraDAmmV2Pool {
    pub pool: Pubkey,
    pub token_x_vault: Pubkey,
    pub token_sol_vault: Pubkey,
    pub token_mint: Pubkey,
    pub base_mint: Pubkey,
}

impl PoolData for MeteoraDAmmV2Pool {
    fn pool_address(&self) -> &Pubkey { &self.pool }
    fn token_vault(&self) -> &Pubkey { &self.token_x_vault }
    fn sol_vault(&self) -> &Pubkey { &self.token_sol_vault }
    fn token_mint(&self) -> &Pubkey { &self.token_mint }
    fn base_mint(&self) -> &Pubkey { &self.base_mint }
    fn get_dex_name(&self) -> &str { "MeteoraDAmmV2" }
}

#[derive(Debug, Clone)]
pub struct VertigoPool {
    pub pool: Pubkey,
    pub pool_owner: Pubkey,
    pub token_x_vault: Pubkey,
    pub token_sol_vault: Pubkey,
    pub token_mint: Pubkey,
    pub base_mint: Pubkey,
}

impl PoolData for VertigoPool {
    fn pool_address(&self) -> &Pubkey { &self.pool }
    fn token_vault(&self) -> &Pubkey { &self.token_x_vault }
    fn sol_vault(&self) -> &Pubkey { &self.token_sol_vault }
    fn token_mint(&self) -> &Pubkey { &self.token_mint }
    fn base_mint(&self) -> &Pubkey { &self.base_mint }
    fn get_dex_name(&self) -> &str { "Vertigo" }
}

#[derive(Debug, Clone)]
pub struct PhoenixPool {
    pub pool: Pubkey,
    pub base_vault: Pubkey,
    pub quote_vault: Pubkey,
    pub token_mint: Pubkey,
    pub base_mint: Pubkey,
}

impl PoolData for PhoenixPool {
    fn pool_address(&self) -> &Pubkey { &self.pool }
    fn token_vault(&self) -> &Pubkey { &self.base_vault }
    fn sol_vault(&self) -> &Pubkey { &self.quote_vault }
    fn token_mint(&self) -> &Pubkey { &self.token_mint }
    fn base_mint(&self) -> &Pubkey { &self.base_mint }
    fn get_dex_name(&self) -> &str { "Phoenix" }
}

#[derive(Debug, Clone)]
pub struct LifinityPool {
    pub pool: Pubkey,
    pub token_a_vault: Pubkey,
    pub token_b_vault: Pubkey,
    pub token_mint: Pubkey,
    pub base_mint: Pubkey,
}

impl PoolData for LifinityPool {
    fn pool_address(&self) -> &Pubkey { &self.pool }
    fn token_vault(&self) -> &Pubkey { &self.token_a_vault }
    fn sol_vault(&self) -> &Pubkey { &self.token_b_vault }
    fn token_mint(&self) -> &Pubkey { &self.token_mint }
    fn base_mint(&self) -> &Pubkey { &self.base_mint }
    fn get_dex_name(&self) -> &str { "Lifinity" }
}

#[derive(Debug, Clone)]
pub struct HeavenPool {
    pub pool: Pubkey,
    pub token_x_vault: Pubkey,
    pub token_sol_vault: Pubkey,
    pub token_mint: Pubkey,
    pub base_mint: Pubkey,
}

impl PoolData for HeavenPool {
    fn pool_address(&self) -> &Pubkey { &self.pool }
    fn token_vault(&self) -> &Pubkey { &self.token_x_vault }
    fn sol_vault(&self) -> &Pubkey { &self.token_sol_vault }
    fn token_mint(&self) -> &Pubkey { &self.token_mint }
    fn base_mint(&self) -> &Pubkey { &self.base_mint }
    fn get_dex_name(&self) -> &str { "Heaven" }
}

#[derive(Debug, Clone, Default)]
pub struct MintPoolData {
    pub mint: Pubkey,
    pub token_program: Pubkey, // Support for both Token and Token 2022
    pub wallet_account: Pubkey,
    pub wallet_wsol_account: Pubkey,
    pub pools: Vec<Pool>,
}

impl MintPoolData {
    pub fn new(mint: &str, wallet_account: &str, token_program: Pubkey) -> anyhow::Result<Self> {
        let sol_mint = Pubkey::from_str(SOL_MINT)?;
        let wallet_pk = Pubkey::from_str(wallet_account)?;
        let wallet_wsol_pk =
            spl_associated_token_account::get_associated_token_address(&wallet_pk, &sol_mint);
        Ok(Self {
            mint: Pubkey::from_str(mint)?,
            token_program,
            wallet_account: wallet_pk,
            wallet_wsol_account: wallet_wsol_pk,
            pools: Vec::new(),
        })
    }

    pub fn add_raydium_pool(
        &mut self,
        pool: &str,
        token_vault: &str,
        sol_vault: &str,
        token_mint: &str,
        base_mint: &str,
    ) -> anyhow::Result<()> {
        self.pools.push(Pool::Raydium(RaydiumPool {
            pool: Pubkey::from_str(pool)?,
            token_vault: Pubkey::from_str(token_vault)?,
            sol_vault: Pubkey::from_str(sol_vault)?,
            token_mint: Pubkey::from_str(token_mint)?,
            base_mint: Pubkey::from_str(base_mint)?,
        }));
        Ok(())
    }

    pub fn add_raydium_cp_pool(
        &mut self,
        pool: &str,
        token_vault: &str,
        sol_vault: &str,
        _amm_config: &str,
        _observation: &str,
        token_mint: &str,
        base_mint: &str,
    ) -> anyhow::Result<()> {
        self.pools.push(Pool::RaydiumCp(RaydiumCpPool {
            pool: Pubkey::from_str(pool)?,
            token_vault: Pubkey::from_str(token_vault)?,
            sol_vault: Pubkey::from_str(sol_vault)?,
            amm_config: Pubkey::from_str(_amm_config)?,
            observation: Pubkey::from_str(_observation)?,
            token_mint: Pubkey::from_str(token_mint)?,
            base_mint: Pubkey::from_str(base_mint)?,
        }));
        Ok(())
    }

    pub fn add_pump_pool(
        &mut self,
        pool: &str,
        token_vault: &str,
        sol_vault: &str,
        _fee_token_wallet: &str,
        _coin_creator_vault_ata: &str,
        _coin_creator_authority: &str,
        token_mint: &str,
        base_mint: &str,
    ) -> anyhow::Result<()> {
        self.pools.push(Pool::Pump(PumpPool {
            pool: Pubkey::from_str(pool)?,
            token_vault: Pubkey::from_str(token_vault)?,
            sol_vault: Pubkey::from_str(sol_vault)?,
            fee_token_wallet: Pubkey::new_unique(),
            coin_creator_vault_ata: Pubkey::new_unique(),
            coin_creator_vault_authority: Pubkey::new_unique(),
            token_mint: Pubkey::from_str(token_mint)?,
            base_mint: Pubkey::from_str(base_mint)?,
        }));
        Ok(())
    }

    pub fn add_dlmm_pool(
        &mut self,
        pair: &str,
        token_vault: &str,
        sol_vault: &str,
        oracle: &str,
        bin_arrays: Vec<&str>,
        memo_program: Option<&str>,
        token_mint: &str,
        base_mint: &str,
    ) -> anyhow::Result<()> {
        let bin_array_pubkeys = bin_arrays
            .iter()
            .map(|&s| Pubkey::from_str(s))
            .collect::<Result<Vec<_>, _>>()?;

        let memo_program_pubkey = if let Some(memo) = memo_program {
            Some(Pubkey::from_str(memo)?)
        } else {
            None
        };

        self.pools.push(Pool::Dlmm(DlmmPool {
            pair: Pubkey::from_str(pair)?,
            token_vault: Pubkey::from_str(token_vault)?,
            sol_vault: Pubkey::from_str(sol_vault)?,
            oracle: Pubkey::from_str(oracle)?,
            bin_arrays: bin_array_pubkeys,
            memo_program: memo_program_pubkey,
            token_mint: Pubkey::from_str(token_mint)?,
            base_mint: Pubkey::from_str(base_mint)?,
        }));
        Ok(())
    }

    pub fn add_whirlpool_pool(
        &mut self,
        pool: &str,
        oracle: &str,
        x_vault: &str,
        y_vault: &str,
        tick_arrays: Vec<&str>,
        memo_program: Option<&str>,
        token_mint: &str,
        base_mint: &str,
    ) -> anyhow::Result<()> {
        let tick_array_pubkeys = tick_arrays
            .iter()
            .map(|&s| Pubkey::from_str(s))
            .collect::<Result<Vec<_>, _>>()?;

        let memo_program_pubkey = if let Some(memo) = memo_program {
            Some(Pubkey::from_str(memo)?)
        } else {
            None
        };

        self.pools.push(Pool::Whirlpool(WhirlpoolPool {
            pool: Pubkey::from_str(pool)?,
            oracle: Pubkey::from_str(oracle)?,
            x_vault: Pubkey::from_str(x_vault)?,
            y_vault: Pubkey::from_str(y_vault)?,
            tick_arrays: tick_array_pubkeys,
            memo_program: memo_program_pubkey,
            token_mint: Pubkey::from_str(token_mint)?,
            base_mint: Pubkey::from_str(base_mint)?,
        }));
        Ok(())
    }

    pub fn add_raydium_clmm_pool(
        &mut self,
        pool: &str,
        amm_config: &str,
        observation_state: &str,
        x_vault: &str,
        y_vault: &str,
        tick_arrays: Vec<&str>,
        memo_program: Option<&str>,
        token_mint: &str,
        base_mint: &str,
    ) -> anyhow::Result<()> {
        let pool_pubkey = Pubkey::from_str(pool)?;
        let bitmap_extension = Pubkey::find_program_address(
            &[
                POOL_TICK_ARRAY_BITMAP_SEED.as_bytes(),
                &pool_pubkey.as_ref(),
            ],
            &raydium_clmm_program_id(),
        )
        .0;
        let tick_array_pubkeys = tick_arrays
            .iter()
            .map(|&s| Pubkey::from_str(s))
            .collect::<Result<Vec<_>, _>>()?;

        let memo_program_pubkey = if let Some(memo) = memo_program {
            Some(Pubkey::from_str(memo)?)
        } else {
            None
        };

        self.pools.push(Pool::RaydiumClmm(RaydiumClmmPool {
            pool: pool_pubkey,
            amm_config: Pubkey::from_str(amm_config)?,
            observation_state: Pubkey::from_str(observation_state)?,
            x_vault: Pubkey::from_str(x_vault)?,
            y_vault: Pubkey::from_str(y_vault)?,
            bitmap_extension,
            tick_arrays: tick_array_pubkeys,
            memo_program: memo_program_pubkey,
            token_mint: Pubkey::from_str(token_mint)?,
            base_mint: Pubkey::from_str(base_mint)?,
        }));
        Ok(())
    }

    pub fn add_meteora_damm_pool(
        &mut self,
        pool: &str,
        token_x_vault: &str,
        token_sol_vault: &str,
        token_x_token_vault: &str,
        token_sol_token_vault: &str,
        token_x_lp_mint: &str,
        token_sol_lp_mint: &str,
        token_x_pool_lp: &str,
        token_sol_pool_lp: &str,
        admin_token_fee_x: &str,
        admin_token_fee_sol: &str,
        token_mint: &str,
        base_mint: &str,
    ) -> anyhow::Result<()> {
        self.pools.push(Pool::MeteoraDAmm(MeteoraDAmmPool {
            pool: Pubkey::from_str(pool)?,
            token_x_vault: Pubkey::from_str(token_x_vault)?,
            token_sol_vault: Pubkey::from_str(token_sol_vault)?,
            token_x_token_vault: Pubkey::from_str(token_x_token_vault)?,
            token_sol_token_vault: Pubkey::from_str(token_sol_token_vault)?,
            token_x_lp_mint: Pubkey::from_str(token_x_lp_mint)?,
            token_sol_lp_mint: Pubkey::from_str(token_sol_lp_mint)?,
            token_x_pool_lp: Pubkey::from_str(token_x_pool_lp)?,
            token_sol_pool_lp: Pubkey::from_str(token_sol_pool_lp)?,
            admin_token_fee_x: Pubkey::from_str(admin_token_fee_x)?,
            admin_token_fee_sol: Pubkey::from_str(admin_token_fee_sol)?,
            token_mint: Pubkey::from_str(token_mint)?,
            base_mint: Pubkey::from_str(base_mint)?,
        }));
        Ok(())
    }

    pub fn add_solfi_pool(
        &mut self,
        pool: &str,
        token_x_vault: &str,
        token_sol_vault: &str,
        token_mint: &str,
        base_mint: &str,
    ) -> anyhow::Result<()> {
        self.pools.push(Pool::Solfi(SolfiPool {
            pool: Pubkey::from_str(pool)?,
            token_x_vault: Pubkey::from_str(token_x_vault)?,
            token_sol_vault: Pubkey::from_str(token_sol_vault)?,
            token_mint: Pubkey::from_str(token_mint)?,
            base_mint: Pubkey::from_str(base_mint)?,
        }));
        Ok(())
    }

    pub fn add_meteora_damm_v2_pool(
        &mut self,
        pool: &str,
        token_x_vault: &str,
        token_sol_vault: &str,
        token_mint: &str,
        base_mint: &str,
    ) -> anyhow::Result<()> {
        self.pools.push(Pool::MeteoraDAmmV2(MeteoraDAmmV2Pool {
            pool: Pubkey::from_str(pool)?,
            token_x_vault: Pubkey::from_str(token_x_vault)?,
            token_sol_vault: Pubkey::from_str(token_sol_vault)?,
            token_mint: Pubkey::from_str(token_mint)?,
            base_mint: Pubkey::from_str(base_mint)?,
        }));
        Ok(())
    }

    pub fn add_vertigo_pool(
        &mut self,
        pool: &str,
        pool_owner: &str,
        token_x_vault: &str,
        token_sol_vault: &str,
        token_mint: &str,
        base_mint: &str,
    ) -> anyhow::Result<()> {
        self.pools.push(Pool::Vertigo(VertigoPool {
            pool: Pubkey::from_str(pool)?,
            pool_owner: Pubkey::from_str(pool_owner)?,
            token_x_vault: Pubkey::from_str(token_x_vault)?,
            token_sol_vault: Pubkey::from_str(token_sol_vault)?,
            token_mint: Pubkey::from_str(token_mint)?,
            base_mint: Pubkey::from_str(base_mint)?,
        }));
        Ok(())
    }

    pub fn add_phoenix_pool(
        &mut self,
        pool: &str,
        base_vault: &str,
        quote_vault: &str,
        token_mint: &str,
        base_mint: &str,
    ) -> anyhow::Result<()> {
        self.pools.push(Pool::Phoenix(PhoenixPool {
            pool: Pubkey::from_str(pool)?,
            base_vault: Pubkey::from_str(base_vault)?,
            quote_vault: Pubkey::from_str(quote_vault)?,
            token_mint: Pubkey::from_str(token_mint)?,
            base_mint: Pubkey::from_str(base_mint)?,
        }));
        Ok(())
    }

    pub fn add_lifinity_pool(
        &mut self,
        pool: &str,
        token_a_vault: &str,
        token_b_vault: &str,
        token_mint: &str,
        base_mint: &str,
    ) -> anyhow::Result<()> {
        self.pools.push(Pool::Lifinity(LifinityPool {
            pool: Pubkey::from_str(pool)?,
            token_a_vault: Pubkey::from_str(token_a_vault)?,
            token_b_vault: Pubkey::from_str(token_b_vault)?,
            token_mint: Pubkey::from_str(token_mint)?,
            base_mint: Pubkey::from_str(base_mint)?,
        }));
        Ok(())
    }

    pub fn add_heaven_pool(
        &mut self,
        pool: &str,
        token_x_vault: &str,
        token_sol_vault: &str,
        token_mint: &str,
        base_mint: &str,
    ) -> anyhow::Result<()> {
        self.pools.push(Pool::Heaven(HeavenPool {
            pool: Pubkey::from_str(pool)?,
            token_x_vault: Pubkey::from_str(token_x_vault)?,
            token_sol_vault: Pubkey::from_str(token_sol_vault)?,
            token_mint: Pubkey::from_str(token_mint)?,
            base_mint: Pubkey::from_str(base_mint)?,
        }));
        Ok(())
    }
}