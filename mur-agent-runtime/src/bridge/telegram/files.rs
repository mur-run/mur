//! Telegram document / photo handler (M-c2.4, future work).
//!
//! Currently a stub — the inbound loop carries `document_file_id` /
//! `photo_file_id` / `caption` / `file_size` fields on
//! [`crate::bridge::telegram::mock::MockUpdate`] for tests, but no
//! actual download path is wired.
