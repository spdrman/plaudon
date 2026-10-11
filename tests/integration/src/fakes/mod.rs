//! In-memory implementations of every capability contract.

pub mod device;
pub mod source;
pub mod stt;
pub mod text;

pub use device::FakeDevice;
pub use source::FakeSource;
pub use stt::FakeSpeechToText;
pub use text::FakeTextModel;
