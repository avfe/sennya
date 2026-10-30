//! sennya: a GPU terminal with persistent sessions, vertical tabs and a tiling grid.

pub mod claude;
pub mod cli;
pub mod client;
pub mod config;
pub mod daemon;
#[cfg(feature = "gui")]
pub mod gui;
pub mod model;
pub mod paths;
pub mod protocol;
pub mod template;
