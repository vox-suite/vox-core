/**
 * Real-time bidirectional voice WebSocket endpoint for desktop and mobile clients.
 *
 * Clients stream raw 16-bit mono PCM audio at 16kHz for one utterance as one or
 * more binary WebSocket frames, followed by a `Turn` text message marking the
 * utterance complete. The server transcribes it (AssemblyAI), queries the Gemini
 * agent, and streams back the transcript, text deltas, and high-fidelity
 * ElevenLabs MP3 audio chunks.
 */
use axum::{
    Extension,
    extract::{
        State,
        ws::{Message, WebSocket, WebSocketUpgrade},
    },
    response::IntoResponse,
};
use bytes::Bytes;
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;
use vox_core::{
    conversations::{RespondRequest, service::ConversationService},
    domain::identity::Actor,
    identity::{ChannelIdentity, ResolvedUserContext},
    stt::AssemblyAiClient,
    tts::{ElevenLabsClient, SentenceChunker},
};

/// The sample rate (Hz) clients must resample captured mic audio to before
/// sending it as binary PCM16 frames.
pub const VOICE_INPUT_SAMPLE_RATE: u32 = 16000;

#[derive(Clone)]
pub struct VoiceSocketState {
    pub conversations: Option<Arc<ConversationService>>,
    pub tts: Option<Arc<ElevenLabsClient>>,
    pub stt: Option<Arc<AssemblyAiClient>>,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum VoiceClientMessage {
    /// Marks the audio sent as preceding binary frames (since the last Turn
    /// or Interrupt) as one complete utterance ready to transcribe.
    Turn {
        #[serde(default)]
        conversation_id: Option<String>,
    },
    Interrupt,
    Ping,
}

#[derive(Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum VoiceServerMessage<'a> {
    Connected {
        format: &'a str,
        sample_rate: u32,
    },
    /// The server's transcription of the audio for this turn.
    UserTranscript {
        turn_id: &'a str,
        text: &'a str,
    },
    Thinking {
        turn_id: &'a str,
    },
    TextDelta {
        turn_id: &'a str,
        delta: &'a str,
    },
    Done {
        turn_id: &'a str,
    },
    Interrupted,
    Error {
        message: &'a str,
    },
    Pong,
}

pub async fn voice_socket(
    State(state): State<VoiceSocketState>,
    Extension(actor): Extension<Actor>,
    ws: WebSocketUpgrade,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_voice_socket(socket, state, actor))
}

enum OutboundFrame {
    Text(String),
    Binary(Bytes),
    Pong(Bytes),
}

/// Synthesizes and streams one sentence's audio to the client. Returns
/// `false` if the caller should stop the turn immediately (cancelled
/// mid-stream, or the outbound channel closed), `true` to continue.
async fn speak_sentence(
    tts: Option<&ElevenLabsClient>,
    sentence: &str,
    out_tx: &mpsc::Sender<OutboundFrame>,
    cancel_token: &CancellationToken,
) -> bool {
    let Some(tts_client) = tts else {
        return true;
    };
    let Ok(mut audio_stream) = tts_client.synthesize_stream(sentence).await else {
        return true;
    };
    while let Some(chunk_res) = audio_stream.next().await {
        if cancel_token.is_cancelled() {
            return false;
        }
        if let Ok(bytes) = chunk_res
            && out_tx.send(OutboundFrame::Binary(bytes)).await.is_err()
        {
            return false;
        }
    }
    true
}

/// Drains queued sentences in order, one at a time, so the caller can push
/// sentences as soon as they're chunked without waiting for each one's full
/// TTS synthesis+streaming before consuming the next LLM token.
async fn run_speaker(
    mut sentence_rx: mpsc::UnboundedReceiver<String>,
    tts: Option<Arc<ElevenLabsClient>>,
    out_tx: mpsc::Sender<OutboundFrame>,
    cancel_token: CancellationToken,
) {
    while let Some(sentence) = sentence_rx.recv().await {
        if !speak_sentence(tts.as_deref(), &sentence, &out_tx, &cancel_token).await {
            return;
        }
    }
}

async fn handle_voice_socket(socket: WebSocket, state: VoiceSocketState, actor: Actor) {
    let (mut ws_sender, mut ws_receiver) = socket.split();

    let (out_tx, mut out_rx) = mpsc::channel::<OutboundFrame>(64);

    let sender_task = tokio::spawn(async move {
        while let Some(frame) = out_rx.recv().await {
            let msg = match frame {
                OutboundFrame::Text(text) => Message::Text(text.into()),
                OutboundFrame::Binary(bytes) => Message::Binary(bytes),
                OutboundFrame::Pong(bytes) => Message::Pong(bytes),
            };
            if ws_sender.send(msg).await.is_err() {
                break;
            }
        }
    });

    let connected_msg = serde_json::to_string(&VoiceServerMessage::Connected {
        format: "mp3",
        sample_rate: 44100,
    })
    .unwrap_or_default();
    let _ = out_tx.send(OutboundFrame::Text(connected_msg)).await;

    let user_id = actor.user_id;
    let context: ResolvedUserContext = match state.conversations.clone() {
        Some(conversations) => match conversations.resolve_context_for_user(user_id).await {
            Ok(ctx) => ctx,
            Err(e) => {
                let err_msg = serde_json::to_string(&VoiceServerMessage::Error {
                    message: "Failed to resolve user context",
                })
                .unwrap_or_default();
                tracing::error!(error = %e, "Failed to resolve context for voice socket");
                let _ = out_tx.send(OutboundFrame::Text(err_msg)).await;
                let _ = sender_task.await;
                return;
            }
        },
        None => {
            let err_msg = serde_json::to_string(&VoiceServerMessage::Error {
                message: "Conversation service is unavailable",
            })
            .unwrap_or_default();
            let _ = out_tx.send(OutboundFrame::Text(err_msg)).await;
            let _ = sender_task.await;
            return;
        }
    };

    let mut current_turn_cancel: Option<CancellationToken> = None;
    let external_conv_id = format!("desktop:{}", actor.user_id);
    let mut pending_audio: Vec<u8> = Vec::new();

    while let Some(msg) = ws_receiver.next().await {
        let msg = match msg {
            Ok(msg) => msg,
            Err(e) => {
                tracing::debug!(error = %e, "Voice WebSocket disconnected");
                break;
            }
        };

        match msg {
            Message::Text(raw) => {
                let parsed: Result<VoiceClientMessage, _> = serde_json::from_str(&raw);
                match parsed {
                    Ok(VoiceClientMessage::Ping) => {
                        let pong =
                            serde_json::to_string(&VoiceServerMessage::Pong).unwrap_or_default();
                        let _ = out_tx.send(OutboundFrame::Text(pong)).await;
                    }
                    Ok(VoiceClientMessage::Interrupt) => {
                        if let Some(cancel) = current_turn_cancel.take() {
                            cancel.cancel();
                        }
                        pending_audio.clear();
                        let interrupted = serde_json::to_string(&VoiceServerMessage::Interrupted)
                            .unwrap_or_default();
                        let _ = out_tx.send(OutboundFrame::Text(interrupted)).await;
                    }
                    Ok(VoiceClientMessage::Turn { conversation_id }) => {
                        let audio = std::mem::take(&mut pending_audio);
                        if audio.is_empty() {
                            continue;
                        }

                        let Some(stt) = state.stt.clone() else {
                            let err_msg = serde_json::to_string(&VoiceServerMessage::Error {
                                message: "Speech-to-text is unavailable",
                            })
                            .unwrap_or_default();
                            let _ = out_tx.send(OutboundFrame::Text(err_msg)).await;
                            continue;
                        };

                        if let Some(cancel) = current_turn_cancel.take() {
                            cancel.cancel();
                        }

                        let cancel_token = CancellationToken::new();
                        current_turn_cancel = Some(cancel_token.clone());

                        let turn_id = Uuid::new_v4().to_string();
                        let conv_id = conversation_id.unwrap_or_else(|| external_conv_id.clone());

                        let Some(conversations) = state.conversations.clone() else {
                            let err_msg = serde_json::to_string(&VoiceServerMessage::Error {
                                message: "Conversation service is unavailable",
                            })
                            .unwrap_or_default();
                            let _ = out_tx.send(OutboundFrame::Text(err_msg)).await;
                            continue;
                        };

                        let tts = state.tts.clone();
                        let out_tx_clone = out_tx.clone();
                        let context = context.clone();

                        tokio::spawn(async move {
                            let pcm: Vec<i16> = audio
                                .as_chunks::<2>()
                                .0
                                .iter()
                                .map(|b| i16::from_le_bytes(*b))
                                .collect();
                            let text =
                                match stt.transcribe_pcm16(&pcm, VOICE_INPUT_SAMPLE_RATE).await {
                                    Ok(text) => text,
                                    Err(e) => {
                                        tracing::error!(error = %e, "Voice transcription failed");
                                        let err_msg =
                                            serde_json::to_string(&VoiceServerMessage::Error {
                                                message: "Transcription failed",
                                            })
                                            .unwrap_or_default();
                                        let _ =
                                            out_tx_clone.send(OutboundFrame::Text(err_msg)).await;
                                        return;
                                    }
                                };
                            let text = text.trim().to_string();
                            if text.is_empty() {
                                return;
                            }

                            let transcript_msg =
                                serde_json::to_string(&VoiceServerMessage::UserTranscript {
                                    turn_id: &turn_id,
                                    text: &text,
                                })
                                .unwrap_or_default();
                            let _ = out_tx_clone.send(OutboundFrame::Text(transcript_msg)).await;

                            let thinking = serde_json::to_string(&VoiceServerMessage::Thinking {
                                turn_id: &turn_id,
                            })
                            .unwrap_or_default();
                            let _ = out_tx_clone.send(OutboundFrame::Text(thinking)).await;

                            let request = RespondRequest {
                                agent_external_key: "general".to_string(),
                                identity: ChannelIdentity {
                                    channel: "desktop".to_string(),
                                    external_id: user_id.to_string(),
                                },
                                external_conversation_id: conv_id,
                                text,
                                initiation_context: None,
                                turn_id: Some(turn_id.clone()),
                                revision: None,
                                tts_provider: Some("elevenlabs".to_string()),
                                filler: None,
                            };

                            let stream_res = conversations.respond_stream(context, request).await;
                            let mut stream = match stream_res {
                                Ok(s) => s,
                                Err(e) => {
                                    let err_msg =
                                        serde_json::to_string(&VoiceServerMessage::Error {
                                            message: "Agent response failed",
                                        })
                                        .unwrap_or_default();
                                    tracing::error!(error = %e, "Agent response stream error");
                                    let _ = out_tx_clone.send(OutboundFrame::Text(err_msg)).await;
                                    return;
                                }
                            };

                            let mut chunker = SentenceChunker::new();
                            let (sentence_tx, sentence_rx) = mpsc::unbounded_channel::<String>();
                            let speaker_task = tokio::spawn(run_speaker(
                                sentence_rx,
                                tts,
                                out_tx_clone.clone(),
                                cancel_token.clone(),
                            ));

                            loop {
                                tokio::select! {
                                    _ = cancel_token.cancelled() => {
                                        tracing::debug!(turn_id = %turn_id, "Voice turn cancelled by user interruption");
                                        drop(sentence_tx);
                                        let _ = speaker_task.await;
                                        return;
                                    }
                                    item = stream.next() => {
                                        match item {
                                            Some(Ok(delta)) => {
                                                if delta.is_empty() {
                                                    continue;
                                                }
                                                let delta_msg = serde_json::to_string(&VoiceServerMessage::TextDelta {
                                                    turn_id: &turn_id,
                                                    delta: &delta,
                                                })
                                                .unwrap_or_default();
                                                let _ = out_tx_clone.send(OutboundFrame::Text(delta_msg)).await;

                                                for sentence in chunker.push(&delta) {
                                                    let _ = sentence_tx.send(sentence);
                                                }
                                            }
                                            Some(Err(err)) => {
                                                tracing::warn!(error = %err, "Error during voice LLM stream");
                                                break;
                                            }
                                            None => {
                                                break;
                                            }
                                        }
                                    }
                                }
                            }

                            if let Some(final_sentence) = chunker.flush() {
                                let _ = sentence_tx.send(final_sentence);
                            }
                            drop(sentence_tx);
                            let _ = speaker_task.await;

                            if !cancel_token.is_cancelled() {
                                let done = serde_json::to_string(&VoiceServerMessage::Done {
                                    turn_id: &turn_id,
                                })
                                .unwrap_or_default();
                                let _ = out_tx_clone.send(OutboundFrame::Text(done)).await;
                            }
                        });
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "Invalid voice client message payload");
                    }
                }
            }
            Message::Binary(bytes) => {
                pending_audio.extend_from_slice(&bytes);
            }
            Message::Close(_) => break,
            Message::Ping(payload) => {
                let _ = out_tx.send(OutboundFrame::Pong(payload)).await;
            }
            _ => {}
        }
    }

    if let Some(cancel) = current_turn_cancel {
        cancel.cancel();
    }
    let _ = sender_task.await;
}
