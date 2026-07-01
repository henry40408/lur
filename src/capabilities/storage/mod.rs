//! Storage backend seam. `Backend` isolates all backend-specific code (SQL
//! dialect, binding, row mapping, concurrency) so `db.rs`/`kv.rs` stay
//! backend-neutral. `SQLite` is the only backend today; the `Postgres` variant
//! lands in Phase 2.

pub(crate) mod sqlite;
