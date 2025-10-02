pub mod sled_store;
#[cfg(test)]
mod tests;
pub use sled_store::Persistence;
