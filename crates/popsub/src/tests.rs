//! Tests for the binary's startup decisions.

use popsub_config::StorageBackend;
use popsub_storage::Durability;

use crate::effective_durability;

/// The memory backend runs the default `buffered` durability as `none`.
#[test]
fn memory_runs_the_default_durability_as_none() {
    assert_eq!(
        effective_durability(StorageBackend::Memory, Durability::Buffered),
        Durability::None
    );
}

/// An explicit `synced` with the memory backend is passed on, to be refused.
#[test]
fn memory_passes_synced_on_to_be_refused() {
    assert_eq!(
        effective_durability(StorageBackend::Memory, Durability::Synced),
        Durability::Synced
    );
}

/// The file backend uses the durability it is given.
#[test]
fn file_uses_the_durability_it_is_given() {
    for durability in [Durability::None, Durability::Buffered, Durability::Synced] {
        assert_eq!(
            effective_durability(StorageBackend::File, durability),
            durability
        );
    }
}
