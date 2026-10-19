pub mod capture;
pub mod convert;
pub mod device;
pub mod mixer;

/// Every stream inside the app is mono f32 at this rate.
pub const SAMPLE_RATE: u32 = 48_000;
/// One Opus frame: 20 ms at 48 kHz.
pub const FRAME: usize = 960;
