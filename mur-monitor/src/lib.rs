//! Durable monitor: keep checking asynchronous work until it is settled.
//! Sits below `mur-core` so an agent runtime can register a monitor without
//! pulling the vector store in. Adapters that need `mur-core` live there.
pub mod spec;
