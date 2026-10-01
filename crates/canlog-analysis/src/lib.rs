//! CAN analysis shared by the logger (Pi) and the web app (WebAssembly):
//! frame timing on the wire, log import, bus load, per-message timing,
//! response-time analysis and signal series for plots.

pub mod bits;
pub mod dataset;
pub mod engine;
pub mod logfile;
pub mod rta;
pub mod stats;
