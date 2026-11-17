//! Harness de testes ponta a ponta: bots por processo, sonda de áudio e relatório.

pub mod bot;
pub mod bot_main;
pub mod cli;
pub mod controller;
pub mod detect;
pub mod dsp;
pub mod frames;
#[cfg(windows)]
pub mod loopback;
pub mod probe;
pub mod proc;
pub mod protocol;
pub mod report;
pub mod runner;
pub mod scenario;
pub mod scenarios;
