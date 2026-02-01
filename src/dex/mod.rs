pub mod meteora;
pub mod pump;
pub mod raydium;
pub mod solfi;
pub mod vertigo;
pub mod whirlpool;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Dex {
    Raydium,
    Pump,
    Dlmm,
    Whirlpool,
}