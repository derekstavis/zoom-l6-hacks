//! Experimental QEMU firmware probes and host-side protocol models.
//! These document candidate mechanisms and observations, not proven firmware
//! ownership or synchronization guarantees.

#![allow(dead_code)]

mod boundary_pause;
mod event_queue;
mod handoff_protocol;
mod heap_range;
mod refresh_handoff;

#[cfg(test)]
mod tests;
