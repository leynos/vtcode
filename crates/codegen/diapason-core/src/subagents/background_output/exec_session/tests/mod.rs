//! Exercises the default [`ExecSessionOutputReader`](super::ExecSessionOutputReader)
//! adapter.
//!
//! Each case pins one contract the controller relies on when it reads
//! through [`BackgroundOutputReader`](super::super::BackgroundOutputReader):
//! live output is returned unchanged, a not-ready session falls back to the
//! archive, a missing exec session or
//! a removed PTY handle falls back to the archive resolved for the task's own
//! session, a recorded path that is another task's archive is refused, a
//! recorded path outside the task's scope is refused, a recorded path absent
//! when it is checked is refused rather than served, a session this process did
//! not launch is refused as an archive fault, a task without a recorded path
//! stays `NotReady`, and an unreadable archive is reported as an archive fault.

mod archive;
mod fixtures;
mod live;
