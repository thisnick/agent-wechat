//! Experimental offline call coordination.
//!
//! This module has no account worker, HTTP route, notification parser, transport
//! or capture-device adapter. Its local state transitions are not server enum
//! values. Enabling the feature does not enable making or receiving calls.

pub mod control;
