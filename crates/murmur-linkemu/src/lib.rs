//! murmur-linkemu: a user-space UDP link emulator that sits between simulated vehicles and the
//! bridge and degrades each vehicle's link according to its link class. See the README.

pub mod config;
pub mod geo;
pub mod http;
pub mod link;
pub mod mavpos;
pub mod model;
pub mod proxy;
pub mod stats;
