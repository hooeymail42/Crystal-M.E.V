pub mod heaven;
pub mod lifinity;
pub mod lst;
pub mod meteora;
pub mod phoenix;
pub mod pump;
pub mod raydium;
pub mod solfi;
pub mod vertigo;
pub mod whirlpool;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[allow(dead_code)]
pub enum Dex {
    Raydium,
    Pump,
    Dlmm,
    Whirlpool,
    Phoenix,
    Lifinity,
    Heaven,
}