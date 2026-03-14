//! Voice message processing — speech-to-text transcription.

use std::path::PathBuf;

use serde_json::Value;

// Imported for future Tool trait implementation.
#[allow(unused_imports)]
use crate::{ToolContext, ToolDefinition, ToolError, ToolOutput};

/// Supported STT (speech-to-text) providers.
#[derive(Debug, Clone)]
pub enum SttProvider {
    /// OpenAI Whisper cloud API.
    Whisper { api_key: String },
    /// Local Whisper model.
    WhisperLocal { model_path: PathBuf },
    /// System-level STT (OS provided).
    SystemStt,
}

/// Audio format of the incoming voice message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioFormat {
    OggOpus,
    Mp3,
    Wav,
    M4a,
}

impl AudioFormat {
    /// Detect audio format from raw bytes (magic-byte sniffing).
    pub fn detect(data: &[u8]) -> Option<Self> {
        if data.len() < 4 {
            return None;
        }
        // OGG container (OggS)
        if data.starts_with(b"OggS") {
            return Some(Self::OggOpus);
        }
        // RIFF/WAV
        if data.starts_with(b"RIFF") && data.len() >= 12 && &data[8..12] == b"WAVE" {
            return Some(Self::Wav);
        }
        // MP3: ID3 tag or frame sync
        if data.starts_with(b"ID3") || (data[0] == 0xFF && (data[1] & 0xE0) == 0xE0) {
            return Some(Self::Mp3);
        }
        // M4A/MP4: ftyp box
        if data.len() >= 8 && &data[4..8] == b"ftyp" {
            return Some(Self::M4a);
        }
        None
    }

    /// MIME type string for multipart uploads.
    pub fn mime_type(&self) -> &'static str {
        match self {
            Self::OggOpus => "audio/ogg",
            Self::Mp3 => "audio/mpeg",
            Self::Wav => "audio/wav",
            Self::M4a => "audio/mp4",
        }
    }

    /// File extension for multipart uploads.
    pub fn extension(&self) -> &'static str {
        match self {
            Self::OggOpus => "ogg",
            Self::Mp3 => "mp3",
            Self::Wav => "wav",
            Self::M4a => "m4a",
        }
    }
}

/// Voice processing errors.
#[derive(Debug, thiserror::Error)]
pub enum VoiceError {
    #[error("no STT provider configured")]
    NoProvider,
    #[error("audio too long: {duration_secs}s exceeds max {max_secs}s")]
    TooLong { duration_secs: u32, max_secs: u32 },
    #[error("unrecognised audio format")]
    UnknownFormat,
    #[error("transcription failed: {0}")]
    TranscriptionFailed(String),
    #[error("HTTP error: {0}")]
    Http(String),
    #[error("provider not supported: {0}")]
    UnsupportedProvider(String),
}

/// Configuration for voice processing.
#[derive(Debug, Clone)]
pub struct VoiceConfig {
    pub stt_provider: SttProvider,
    pub max_duration_seconds: u32,
}

impl Default for VoiceConfig {
    fn default() -> Self {
        Self {
            stt_provider: SttProvider::SystemStt,
            max_duration_seconds: 300,
        }
    }
}

/// Voice processor — handles STT transcription of audio messages.
#[derive(Debug)]
pub struct VoiceProcessor {
    config: VoiceConfig,
    #[cfg(feature = "http")]
    http_client: reqwest::Client,
}

impl VoiceProcessor {
    /// Create a new voice processor with the given config.
    pub fn new(config: VoiceConfig) -> Self {
        Self {
            config,
            #[cfg(feature = "http")]
            http_client: reqwest::Client::new(),
        }
    }

    /// Auto-detect the best available provider.
    ///
    /// If `OPENAI_API_KEY` is set, uses Whisper cloud API.
    /// Otherwise returns an error.
    pub fn auto_detect() -> Result<Self, VoiceError> {
        if let Ok(api_key) = std::env::var("OPENAI_API_KEY") {
            Ok(Self::new(VoiceConfig {
                stt_provider: SttProvider::Whisper { api_key },
                ..VoiceConfig::default()
            }))
        } else {
            Err(VoiceError::NoProvider)
        }
    }

    /// Check duration against the configured maximum.
    pub fn check_duration(&self, duration_secs: u32) -> Result<(), VoiceError> {
        if duration_secs > self.config.max_duration_seconds {
            return Err(VoiceError::TooLong {
                duration_secs,
                max_secs: self.config.max_duration_seconds,
            });
        }
        Ok(())
    }

    /// Transcribe audio bytes to text.
    pub async fn transcribe(
        &self,
        audio: &[u8],
        format: AudioFormat,
    ) -> Result<String, VoiceError> {
        match &self.config.stt_provider {
            SttProvider::Whisper { api_key } => {
                self.transcribe_whisper_api(audio, format, api_key).await
            }
            SttProvider::WhisperLocal { model_path } => Err(VoiceError::UnsupportedProvider(
                format!("local whisper at {:?} not yet implemented", model_path),
            )),
            SttProvider::SystemStt => {
                Err(VoiceError::UnsupportedProvider("system STT not yet implemented".into()))
            }
        }
    }

    /// Call the OpenAI Whisper API for transcription.
    #[cfg(feature = "http")]
    async fn transcribe_whisper_api(
        &self,
        audio: &[u8],
        format: AudioFormat,
        api_key: &str,
    ) -> Result<String, VoiceError> {
        let file_name = format!("audio.{}", format.extension());
        let file_part = reqwest::multipart::Part::bytes(audio.to_vec())
            .file_name(file_name)
            .mime_str(format.mime_type())
            .map_err(|e| VoiceError::Http(e.to_string()))?;

        let form = reqwest::multipart::Form::new()
            .text("model", "whisper-1")
            .part("file", file_part);

        let response = self
            .http_client
            .post("https://api.openai.com/v1/audio/transcriptions")
            .bearer_auth(api_key)
            .multipart(form)
            .send()
            .await
            .map_err(|e| VoiceError::Http(e.to_string()))?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response
                .text()
                .await
                .unwrap_or_else(|_| "no body".into());
            return Err(VoiceError::TranscriptionFailed(format!(
                "Whisper API returned {}: {}",
                status, body
            )));
        }

        let json: Value = response
            .json()
            .await
            .map_err(|e| VoiceError::TranscriptionFailed(e.to_string()))?;

        json.get("text")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .ok_or_else(|| {
                VoiceError::TranscriptionFailed("missing 'text' field in response".into())
            })
    }

    /// Fallback when HTTP feature is disabled.
    #[cfg(not(feature = "http"))]
    async fn transcribe_whisper_api(
        &self,
        _audio: &[u8],
        _format: AudioFormat,
        _api_key: &str,
    ) -> Result<String, VoiceError> {
        Err(VoiceError::Http(
            "HTTP support not compiled in (enable 'http' feature)".into(),
        ))
    }

    /// Access the underlying config.
    pub fn config(&self) -> &VoiceConfig {
        &self.config
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_audio_format_detection_ogg() {
        let ogg_header = b"OggS\x00\x02\x00\x00\x00\x00\x00\x00\x00\x00";
        assert_eq!(AudioFormat::detect(ogg_header), Some(AudioFormat::OggOpus));
    }

    #[test]
    fn test_audio_format_detection_wav() {
        let mut wav = Vec::new();
        wav.extend_from_slice(b"RIFF");
        wav.extend_from_slice(&[0u8; 4]); // size placeholder
        wav.extend_from_slice(b"WAVE");
        assert_eq!(AudioFormat::detect(&wav), Some(AudioFormat::Wav));
    }

    #[test]
    fn test_audio_format_detection_mp3() {
        let mp3_id3 = b"ID3\x04\x00\x00\x00\x00\x00\x00";
        assert_eq!(AudioFormat::detect(mp3_id3), Some(AudioFormat::Mp3));

        // Frame sync bytes
        let mp3_sync = &[0xFF_u8, 0xFB, 0x90, 0x00];
        assert_eq!(AudioFormat::detect(mp3_sync), Some(AudioFormat::Mp3));
    }

    #[test]
    fn test_audio_format_detection_m4a() {
        let mut m4a = Vec::new();
        m4a.extend_from_slice(&[0u8; 4]); // size
        m4a.extend_from_slice(b"ftyp");
        m4a.extend_from_slice(b"M4A ");
        assert_eq!(AudioFormat::detect(&m4a), Some(AudioFormat::M4a));
    }

    #[test]
    fn test_audio_format_detection_unknown() {
        let garbage = &[0x00, 0x01, 0x02, 0x03];
        assert_eq!(AudioFormat::detect(garbage), None);
    }

    #[test]
    fn test_voice_config_defaults() {
        let cfg = VoiceConfig::default();
        assert_eq!(cfg.max_duration_seconds, 300);
        match cfg.stt_provider {
            SttProvider::SystemStt => {} // expected
            _ => panic!("expected SystemStt as default provider"),
        }
    }

    #[test]
    fn test_voice_error_display() {
        let err = VoiceError::NoProvider;
        assert_eq!(err.to_string(), "no STT provider configured");

        let err = VoiceError::TooLong {
            duration_secs: 400,
            max_secs: 300,
        };
        assert_eq!(
            err.to_string(),
            "audio too long: 400s exceeds max 300s"
        );

        let err = VoiceError::UnknownFormat;
        assert_eq!(err.to_string(), "unrecognised audio format");

        let err = VoiceError::Http("connection refused".into());
        assert_eq!(err.to_string(), "HTTP error: connection refused");
    }

    #[test]
    fn test_provider_auto_detection_no_key() {
        // Remove any existing key to test the error path
        std::env::remove_var("OPENAI_API_KEY");
        let result = VoiceProcessor::auto_detect();
        assert!(result.is_err());
        match result.unwrap_err() {
            VoiceError::NoProvider => {} // expected
            other => panic!("expected NoProvider, got: {}", other),
        }
    }

    #[test]
    fn test_max_duration_enforcement() {
        let processor = VoiceProcessor::new(VoiceConfig {
            stt_provider: SttProvider::SystemStt,
            max_duration_seconds: 300,
        });

        // Under limit — should succeed
        assert!(processor.check_duration(60).is_ok());
        assert!(processor.check_duration(300).is_ok());

        // Over limit — should fail
        let err = processor.check_duration(301).unwrap_err();
        match err {
            VoiceError::TooLong {
                duration_secs,
                max_secs,
            } => {
                assert_eq!(duration_secs, 301);
                assert_eq!(max_secs, 300);
            }
            other => panic!("expected TooLong, got: {}", other),
        }
    }

    #[test]
    fn test_whisper_api_request_construction() {
        // Validate that with a Whisper provider, the processor is correctly configured
        let api_key = "sk-test-key-12345";
        let processor = VoiceProcessor::new(VoiceConfig {
            stt_provider: SttProvider::Whisper {
                api_key: api_key.into(),
            },
            max_duration_seconds: 300,
        });

        // Verify provider is set correctly
        match &processor.config().stt_provider {
            SttProvider::Whisper { api_key: key } => {
                assert_eq!(key, "sk-test-key-12345");
            }
            _ => panic!("expected Whisper provider"),
        }

        // Verify format metadata used in requests
        let fmt = AudioFormat::OggOpus;
        assert_eq!(fmt.mime_type(), "audio/ogg");
        assert_eq!(fmt.extension(), "ogg");

        let fmt = AudioFormat::Mp3;
        assert_eq!(fmt.mime_type(), "audio/mpeg");
        assert_eq!(fmt.extension(), "mp3");

        let fmt = AudioFormat::Wav;
        assert_eq!(fmt.mime_type(), "audio/wav");
        assert_eq!(fmt.extension(), "wav");

        let fmt = AudioFormat::M4a;
        assert_eq!(fmt.mime_type(), "audio/mp4");
        assert_eq!(fmt.extension(), "m4a");
    }
}
