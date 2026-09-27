//! ASR (Automatic Speech Recognition) module
//!
//! This module implements the Doubao ASR protocol for real-time speech recognition.

mod client;
mod constants;
mod device;
mod protocol;

pub use client::{is_credential_failure, AsrClient, AsrSetupError, SetupFailure};
pub use constants::*;
pub use device::{DeviceCredentials, register_device, get_asr_token};
pub use protocol::{AsrResponse, ResponseType};

// Include the generated protobuf code
pub mod proto {
    include!(concat!(env!("OUT_DIR"), "/asr.rs"));
}
