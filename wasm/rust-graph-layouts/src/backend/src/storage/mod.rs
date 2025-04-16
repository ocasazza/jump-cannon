pub mod memory;
pub mod traits;

// Re-export the storage trait and implementation
pub use self::traits::GraphStorage;
pub use self::memory::InMemoryStorage;
