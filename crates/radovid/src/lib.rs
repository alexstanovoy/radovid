//! radovid: accounts, matchmaking, ratings and the launching of game servers,
//! for any game that honours the launcher contract (DEVELOPMENT.md).

#[cfg(test)]
extern crate self as radovid;

pub mod artifacts;
pub mod config;
mod control;
pub mod db;
mod hub;
pub mod launcher;
mod matching;
pub mod password;
pub mod private;
pub mod rating;
mod server;
pub mod service;
mod supervisor;
pub mod template;
pub mod throttle;
pub mod tls;
mod web;
