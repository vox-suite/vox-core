/**
 * Server-side speech-to-text for voice call audio.
 */
pub mod assemblyai;

pub use assemblyai::AssemblyAiClient;

pub mod voice_activity;
