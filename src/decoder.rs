//! Streaming decoder facade.
//!
//! The public `crate::decoder::*` API stays here while implementation details
//! live in focused submodules.

mod channel_layout;
mod error;
mod metadata;
mod source;
mod streaming;

#[cfg(feature = "http")]
pub use error::NetworkError;
pub use error::{DecodeCancelToken, DecoderError};
pub use metadata::{AudioInfo, TrackMetadata};
pub use source::{
    HttpCredentials, HttpMediaLocation, MediaLocation, MediaLocationError, MediaLocationKind,
    OpenedMediaSource,
};
pub use streaming::{StreamingDecoder, StreamingDecoderBuilder};

#[cfg(test)]
mod tests;
