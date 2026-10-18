//! radovid: accounts, matchmaking, ratings and the launching of game servers,
//! for any game that honours the launcher contract (README).

pub mod config;
mod control;
pub mod db;
mod hub;
pub mod launcher;
pub mod password;
pub mod rating;
mod server;
pub mod service;
mod supervisor;
pub mod template;
pub mod throttle;
pub mod tls;
