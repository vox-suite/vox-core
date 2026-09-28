/**
 * Text-to-speech synthesis and token chunking utilities.
 */
pub mod chunker;
pub mod elevenlabs;

pub use chunker::SentenceChunker;
pub use elevenlabs::ElevenLabsClient;
