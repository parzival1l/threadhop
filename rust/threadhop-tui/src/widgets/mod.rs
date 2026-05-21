//! Reusable ratatui widgets for the ThreadHop TUI.
//!
//! Each widget is a stateless renderer: it borrows the slice of app state it
//! needs and writes into a `ratatui::buffer::Buffer`. Loading data into the
//! app (e.g. `jsonl::parse_byte_range`) is a worker concern (Wave C/E), not a
//! widget concern.

pub mod contextual_footer;
pub mod transcript;
// session_list lands in a parallel Wave B agent; declared here once that file
// exists.
