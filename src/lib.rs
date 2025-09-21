//! # PopSub
//!
//! `popsub` is a minimalist, in-memory publish/subscribe server built with Rust.
//! It uses WebSockets for client communication and is designed to be a lightweight,
//! real-time messaging backbone for applications.
//!
//! ## Core Modules
//!
//! The library is structured into several modules, each with a distinct responsibility:
//!
//! - `broker`: The central component that manages topics, subscribers, and message routing.
//! - `client`: Represents a connected WebSocket client.
//! - `config`: Handles loading and managing server configuration.
//! - `persistence`: Provides a mechanism for storing and retrieving messages (currently using an in-memory store).
//! - `transport`: Manages the WebSocket server and communication with clients.
//! - `utils`: Contains shared utilities, such as error handling.

pub mod broker;
pub mod client;
pub mod config;
pub mod persistence;
pub mod transport;
pub mod utils;
