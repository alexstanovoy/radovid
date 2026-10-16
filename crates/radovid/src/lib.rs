//! radovid: accounts, matchmaking, ratings and the orchestration of game
//! servers for bota.

pub mod backend;
pub mod config;
mod control;
pub mod db;
mod game;
mod hub;
pub mod password;
pub mod rating;
mod server;
pub mod service;
mod supervisor;
pub mod throttle;
pub mod tls;
