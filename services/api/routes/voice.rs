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
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;
use vox_core::{
    conversations::{CompleteConversationRequest, RespondRequest, service::ConversationService},
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

const UNSET: u64 = u64::MAX;
const VOICE_CHANNEL: &str = "voice";
const GREETING_TRIGGER: &str = "The call just connected. Greet the user.";

/// Whole-call counters, logged when the socket closes.
#[derive(Default)]
struct SessionStats {
    turns: AtomicU64,
    audio_bytes_out: AtomicU64,
    audio_frames_out: AtomicU64,
}

/// Per-turn TTS counters; `first_audio_ms` is measured from the turn start.
struct SpeakStats {
    turn_started: Instant,
    first_audio_ms: AtomicU64,
    audio_bytes: AtomicU64,
    sentences: AtomicU64,
    session: Arc<SessionStats>,
}

impl SpeakStats {
    fn new(session: Arc<SessionStats>) -> Arc<Self> {
        Arc::new(Self {
            turn_started: Instant::now(),
            first_audio_ms: AtomicU64::new(UNSET),
            audio_bytes: AtomicU64::new(0),
            sentences: AtomicU64::new(0),
            session,
        })
    }

    fn record_audio(&self, len: usize) {
        let _ = self.first_audio_ms.compare_exchange(
            UNSET,
            self.turn_started.elapsed().as_millis() as u64,
            Ordering::Relaxed,
            Ordering::Relaxed,
        );
        self.audio_bytes.fetch_add(len as u64, Ordering::Relaxed);
        self.session
            .audio_bytes_out
            .fetch_add(len as u64, Ordering::Relaxed);
        self.session
            .audio_frames_out
            .fetch_add(1, Ordering::Relaxed);
    }
}

/// Synthesizes and streams one sentence's audio to the client. Returns
/// `false` if the caller should stop the turn immediately (cancelled
/// mid-stream, or the outbound channel closed), `true` to continue.
async fn speak_sentence(
    tts: Option<&ElevenLabsClient>,
    sentence: &str,
    out_tx: &mpsc::Sender<OutboundFrame>,
    cancel_token: &CancellationToken,
    stats: &SpeakStats,
    turn_id: &str,
) -> bool {
    let Some(tts_client) = tts else {
        tracing::warn!(turn_id, "TTS unavailable; reply will be silent");
        return true;
    };
    let sentence_started = Instant::now();
    let mut audio_stream = match tts_client.synthesize_stream(sentence).await {
        Ok(stream) => stream,
        Err(e) => {
            tracing::error!(turn_id, error = %e, sentence_chars = sentence.len(), "VOICE_TTS_FAILED");
            return true;
        }
    };
    stats.sentences.fetch_add(1, Ordering::Relaxed);
    let connect_ms = sentence_started.elapsed().as_millis() as u64;
    let mut first_chunk_ms: Option<u64> = None;
    let mut sentence_bytes = 0usize;
    let mut max_gap_ms = 0u64;
    let mut last_chunk_at = Instant::now();
    while let Some(chunk_res) = audio_stream.next().await {
        if cancel_token.is_cancelled() {
            return false;
        }
        match chunk_res {
            Ok(bytes) => {
                if first_chunk_ms.is_none() {
                    first_chunk_ms = Some(sentence_started.elapsed().as_millis() as u64);
                } else {
                    max_gap_ms = max_gap_ms.max(last_chunk_at.elapsed().as_millis() as u64);
                }
                last_chunk_at = Instant::now();
                sentence_bytes += bytes.len();
                stats.record_audio(bytes.len());
                if out_tx.send(OutboundFrame::Binary(bytes)).await.is_err() {
                    return false;
                }
            }
            Err(e) => {
                tracing::warn!(turn_id, error = %e, "VOICE_TTS_CHUNK_ERROR");
            }
        }
    }
    tracing::info!(
        turn_id,
        sentence_chars = sentence.len(),
        tts_connect_ms = connect_ms,
        tts_first_chunk_ms = ?first_chunk_ms,
        tts_total_ms = sentence_started.elapsed().as_millis() as u64,
        tts_max_chunk_gap_ms = max_gap_ms,
        audio_bytes = sentence_bytes,
        "VOICE_TTS_SENTENCE"
    );
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
    stats: Arc<SpeakStats>,
    turn_id: String,
) {
    while let Some(sentence) = sentence_rx.recv().await {
        if !speak_sentence(
            tts.as_deref(),
            &sentence,
            &out_tx,
            &cancel_token,
            &stats,
            &turn_id,
        )
        .await
        {
            return;
        }
    }
}

/// Everything `stream_reply` needs besides the agent stream itself.
struct Reply {
    kind: &'static str,
    turn_id: String,
    out_tx: mpsc::Sender<OutboundFrame>,
    tts: Option<Arc<ElevenLabsClient>>,
    cancel_token: CancellationToken,
    stats: Arc<SpeakStats>,
    llm_started: Instant,
}

/// Streams an agent reply to the client as text deltas plus spoken audio,
/// then sends `Done`. Shared by the connect greeting and normal turns.
async fn stream_reply<S, E>(mut stream: S, reply: Reply)
where
    S: futures_util::Stream<Item = Result<String, E>> + Unpin,
    E: std::fmt::Display,
{
    let Reply {
        kind,
        turn_id,
        out_tx,
        tts,
        cancel_token,
        stats,
        llm_started,
    } = reply;
    let mut chunker = SentenceChunker::new();
    let (sentence_tx, sentence_rx) = mpsc::unbounded_channel::<String>();
    let speaker_task = tokio::spawn(run_speaker(
        sentence_rx,
        tts,
        out_tx.clone(),
        cancel_token.clone(),
        Arc::clone(&stats),
        turn_id.clone(),
    ));

    let mut first_delta_ms: Option<u64> = None;
    let mut reply_chars = 0usize;
    let mut cancelled = false;
    loop {
        tokio::select! {
            _ = cancel_token.cancelled() => {
                cancelled = true;
                break;
            }
            item = stream.next() => {
                match item {
                    Some(Ok(delta)) => {
                        if delta.is_empty() {
                            continue;
                        }
                        if first_delta_ms.is_none() {
                            first_delta_ms = Some(llm_started.elapsed().as_millis() as u64);
                        }
                        reply_chars += delta.len();
                        let delta_msg = serde_json::to_string(&VoiceServerMessage::TextDelta {
                            turn_id: &turn_id,
                            delta: &delta,
                        })
                        .unwrap_or_default();
                        let _ = out_tx.send(OutboundFrame::Text(delta_msg)).await;
                        for sentence in chunker.push(&delta) {
                            let _ = sentence_tx.send(sentence);
                        }
                    }
                    Some(Err(err)) => {
                        tracing::warn!(turn_id = %turn_id, error = %err, "Error during voice LLM stream");
                        break;
                    }
                    None => break,
                }
            }
        }
    }
    let llm_total_ms = llm_started.elapsed().as_millis() as u64;

    if !cancelled && let Some(final_sentence) = chunker.flush() {
        let _ = sentence_tx.send(final_sentence);
    }
    drop(sentence_tx);
    let _ = speaker_task.await;

    let first_audio = stats.first_audio_ms.load(Ordering::Relaxed);
    let cancelled = cancelled || cancel_token.is_cancelled();
    tracing::info!(
        kind,
        turn_id = %turn_id,
        cancelled,
        llm_first_delta_ms = ?first_delta_ms,
        llm_total_ms,
        reply_chars,
        sentences = stats.sentences.load(Ordering::Relaxed),
        audio_bytes = stats.audio_bytes.load(Ordering::Relaxed),
        first_audio_after_turn_start_ms = if first_audio == UNSET { None } else { Some(first_audio) },
        turn_total_ms = stats.turn_started.elapsed().as_millis() as u64,
        "VOICE_TURN_REPLY"
    );

    if !cancelled {
        let done = serde_json::to_string(&VoiceServerMessage::Done { turn_id: &turn_id })
            .unwrap_or_default();
        let _ = out_tx.send(OutboundFrame::Text(done)).await;
    }
}

async fn handle_voice_socket(socket: WebSocket, state: VoiceSocketState, actor: Actor) {
    let session_id = Uuid::new_v4().to_string();
    let session_started = Instant::now();
    let session_stats = Arc::new(SessionStats::default());
    let user_id = actor.user_id;
    tracing::info!(session_id = %session_id, user_id = %user_id, "VOICE_SESSION_OPEN");

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

    let context_started = Instant::now();
    let context: ResolvedUserContext = match state.conversations.clone() {
        Some(conversations) => match conversations.resolve_context_for_user(user_id).await {
            Ok(ctx) => ctx,
            Err(e) => {
                let err_msg = serde_json::to_string(&VoiceServerMessage::Error {
                    message: "Failed to resolve user context",
                })
                .unwrap_or_default();
                tracing::error!(session_id = %session_id, error = %e, "Failed to resolve context for voice socket");
                let _ = out_tx.send(OutboundFrame::Text(err_msg)).await;
                drop(out_tx);
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
            drop(out_tx);
            let _ = sender_task.await;
            return;
        }
    };
    tracing::info!(
        session_id = %session_id,
        context_ms = context_started.elapsed().as_millis() as u64,
        stt_available = state.stt.is_some(),
        tts_available = state.tts.is_some(),
        "VOICE_SESSION_READY"
    );

    let identity = ChannelIdentity {
        channel: VOICE_CHANNEL.to_string(),
        external_id: user_id.to_string(),
    };
    // One conversation per call: it is completed when the socket closes so the
    // summary worker folds it into the user's memory, shared by desktop and mobile.
    let external_conv_id = format!("voice:{user_id}:{session_id}");
    let mut current_turn_cancel: Option<CancellationToken> = None;
    let mut last_turn: Option<tokio::task::JoinHandle<()>> = None;
    let mut pending_audio: Vec<u8> = Vec::new();
    let mut bytes_in = 0u64;

    if let Some(conversations) = state.conversations.clone() {
        let cancel_token = CancellationToken::new();
        current_turn_cancel = Some(cancel_token.clone());
        let turn_id = Uuid::new_v4().to_string();
        let stats = SpeakStats::new(Arc::clone(&session_stats));
        let tts = state.tts.clone();
        let out_tx_clone = out_tx.clone();
        let context = context.clone();
        let request = RespondRequest {
            agent_external_key: "general".to_string(),
            identity: identity.clone(),
            external_conversation_id: external_conv_id.clone(),
            text: GREETING_TRIGGER.to_string(),
            initiation_context: None,
            turn_id: Some(turn_id.clone()),
            revision: None,
            tts_provider: Some("elevenlabs".to_string()),
            filler: None,
        };
        let session_id = session_id.clone();
        last_turn = Some(tokio::spawn(async move {
            let llm_started = Instant::now();
            let stream = match conversations.respond_stream(context, request).await {
                Ok(stream) => stream,
                Err(e) => {
                    tracing::error!(session_id = %session_id, error = %e, "VOICE_GREETING_FAILED");
                    return;
                }
            };
            tracing::info!(
                session_id = %session_id,
                greeting_setup_ms = llm_started.elapsed().as_millis() as u64,
                "VOICE_GREETING_START"
            );
            let thinking =
                serde_json::to_string(&VoiceServerMessage::Thinking { turn_id: &turn_id })
                    .unwrap_or_default();
            let _ = out_tx_clone.send(OutboundFrame::Text(thinking)).await;
            let reply = Reply {
                kind: "greeting",
                turn_id,
                out_tx: out_tx_clone,
                tts,
                cancel_token,
                stats,
                llm_started,
            };
            stream_reply(stream, reply).await;
        }));
    }

    let mut had_turn = false;

    while let Some(msg) = ws_receiver.next().await {
        let msg = match msg {
            Ok(msg) => msg,
            Err(e) => {
                tracing::info!(session_id = %session_id, error = %e, "VOICE_SOCKET_ERROR");
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
                        tracing::info!(session_id = %session_id, "VOICE_INTERRUPT");
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

                        let Some(conversations) = state.conversations.clone() else {
                            let err_msg = serde_json::to_string(&VoiceServerMessage::Error {
                                message: "Conversation service is unavailable",
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
                        had_turn = true;
                        session_stats.turns.fetch_add(1, Ordering::Relaxed);

                        let turn_id = Uuid::new_v4().to_string();
                        let conv_id = conversation_id.unwrap_or_else(|| external_conv_id.clone());
                        let stats = SpeakStats::new(Arc::clone(&session_stats));

                        let tts = state.tts.clone();
                        let out_tx_clone = out_tx.clone();
                        let context = context.clone();
                        let identity = identity.clone();
                        let session_id = session_id.clone();

                        last_turn = Some(tokio::spawn(async move {
                            let pcm: Vec<i16> = audio
                                .as_chunks::<2>()
                                .0
                                .iter()
                                .map(|b| i16::from_le_bytes(*b))
                                .collect();
                            let audio_ms = pcm.len() as u64 * 1000 / VOICE_INPUT_SAMPLE_RATE as u64;
                            let peak = pcm.iter().map(|s| s.unsigned_abs()).max().unwrap_or(0);
                            let rms = (pcm.iter().map(|&s| (s as f64) * (s as f64)).sum::<f64>()
                                / pcm.len().max(1) as f64)
                                .sqrt();
                            let stt_started = Instant::now();
                            let text =
                                match stt.transcribe_pcm16(&pcm, VOICE_INPUT_SAMPLE_RATE).await {
                                    Ok(text) => text,
                                    Err(e) => {
                                        tracing::error!(
                                            session_id = %session_id, turn_id = %turn_id,
                                            error = %e, audio_ms, "VOICE_STT_FAILED"
                                        );
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
                            tracing::info!(
                                session_id = %session_id,
                                turn_id = %turn_id,
                                audio_ms,
                                pcm_peak = peak,
                                pcm_rms = rms as u32,
                                stt_ms = stt_started.elapsed().as_millis() as u64,
                                transcript_chars = text.len(),
                                "VOICE_STT"
                            );
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
                                identity,
                                external_conversation_id: conv_id,
                                text,
                                initiation_context: None,
                                turn_id: Some(turn_id.clone()),
                                revision: None,
                                tts_provider: Some("elevenlabs".to_string()),
                                filler: None,
                            };

                            let llm_started = Instant::now();
                            let stream = match conversations.respond_stream(context, request).await
                            {
                                Ok(s) => s,
                                Err(e) => {
                                    let err_msg =
                                        serde_json::to_string(&VoiceServerMessage::Error {
                                            message: "Agent response failed",
                                        })
                                        .unwrap_or_default();
                                    tracing::error!(session_id = %session_id, turn_id = %turn_id, error = %e, "Agent response stream error");
                                    let _ = out_tx_clone.send(OutboundFrame::Text(err_msg)).await;
                                    return;
                                }
                            };
                            tracing::info!(
                                session_id = %session_id, turn_id = %turn_id,
                                respond_setup_ms = llm_started.elapsed().as_millis() as u64,
                                "VOICE_AGENT_STREAM_OPEN"
                            );
                            let reply = Reply {
                                kind: "turn",
                                turn_id,
                                out_tx: out_tx_clone,
                                tts,
                                cancel_token,
                                stats,
                                llm_started,
                            };
                            stream_reply(stream, reply).await;
                        }));
                    }
                    Err(e) => {
                        tracing::warn!(session_id = %session_id, error = %e, "Invalid voice client message payload");
                    }
                }
            }
            Message::Binary(bytes) => {
                bytes_in += bytes.len() as u64;
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
    if let Some(handle) = last_turn {
        let _ = tokio::time::timeout(Duration::from_secs(5), handle).await;
    }

    let mut summary_queued = false;
    if had_turn && let Some(conversations) = state.conversations.clone() {
        let request = CompleteConversationRequest {
            agent_external_key: "general".to_string(),
            identity,
            external_conversation_id: external_conv_id,
        };
        match conversations.complete(context, request).await {
            Ok(()) => summary_queued = true,
            Err(e) => {
                tracing::error!(session_id = %session_id, error = %e, "VOICE_COMPLETE_FAILED")
            }
        }
    }

    tracing::info!(
        session_id = %session_id,
        user_id = %user_id,
        duration_ms = session_started.elapsed().as_millis() as u64,
        turns = session_stats.turns.load(Ordering::Relaxed),
        mic_bytes_in = bytes_in,
        audio_bytes_out = session_stats.audio_bytes_out.load(Ordering::Relaxed),
        audio_frames_out = session_stats.audio_frames_out.load(Ordering::Relaxed),
        summary_queued,
        "VOICE_SESSION_CLOSE"
    );
    drop(out_tx);
    let _ = sender_task.await;
}
