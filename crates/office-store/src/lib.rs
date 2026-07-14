//! `office-store` — durable, atomic, versioned persistence for the Workflow extension,
//! plus the cross-instance dispatch lease (ARCHITECTURE.md 4).

pub mod store;
pub use store::{apply_migrations, root, LoadResult, Migration, RegistryRow, Store, MIGRATIONS, SCHEMA, SCHEMA_MAJOR};

pub mod lease;
pub use lease::{Lease, HEARTBEAT_MS, STALE_MS};

#[cfg(test)]
mod store_test;

#[cfg(test)]
mod lease_test;
