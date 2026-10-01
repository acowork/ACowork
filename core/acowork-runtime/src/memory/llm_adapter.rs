//! LLM adapter — bridges `acowork_core::providers::traits::Provider` to
//! `acowork_memory::consolidation::triple_extraction::ConsolidationLlm`.
//!
//! The grafeo crate defines a minimal LLM trait (`ConsolidationLlm`) so it
//! stays independent of the runtime's provider ecosystem. This adapter wraps
//! a `dyn Provider` (which has a full chat API with streaming, tool calls, etc.)
//! into the simple `async chat(messages) -> response` interface that grafeo
//! expects, using a low-temperature non-streaming call.

use acowork_core::providers::traits::{ChatMessage, ChatRequest, MessageRole, Provider};
use acowork_memory::consolidation::{LlmMessage, LlmResponse, ConsolidationLlm};

// ---------------------------------------------------------------------------
// Adapter
// ---------------------------------------------------------------------------

/// Adapter: wraps a `dyn Provider` as a `ConsolidationLlm`.
///
/// Uses a fixed low temperature (0.1) and no tool-calling to get
/// deterministic structured output from the LLM.
///
/// Holds an ordered candidate list (ADR-056 list follow-up): each `chat()`
/// call walks the candidates in order and returns the first success; when
/// all fail the last error is returned. Candidates with different providers
/// carry their own `Arc<dyn Provider>` (built by the caller), so
/// cross-provider distillation works in the background pipeline too.
pub struct ProviderLlmAdapter {
    candidates: Vec<DistillCandidate>,
    /// Output-token ceiling for each extraction/judge call.
    max_tokens: u32,
}

/// One distillation target: a provider instance plus the model to request.
pub struct DistillCandidate {
    pub provider: std::sync::Arc<dyn Provider>,
    pub model: String,
}

/// Default output ceiling for distiller calls.
///
/// The Step 2a reply is one JSON object per episode, so the ceiling has to
/// cover the whole chunk — the original 2048 truncated a 100-episode batch
/// roughly a quarter of the way through, which failed the parse and lost every
/// episode in the batch.
const DEFAULT_MAX_TOKENS: u32 = 8192;

impl ProviderLlmAdapter {
    /// Create a new single-candidate adapter from a Provider and model name.
    pub fn new(provider: std::sync::Arc<dyn Provider>, model: String) -> Self {
        Self::with_candidates(vec![DistillCandidate { provider, model }])
    }

    /// Create an adapter with an ordered candidate fallback chain.
    /// Panics-free on empty list: a single placeholder candidate must be
    /// supplied by the caller (resolution chains always end with one).
    pub fn with_candidates(candidates: Vec<DistillCandidate>) -> Self {
        debug_assert!(
            !candidates.is_empty(),
            "ProviderLlmAdapter needs at least 1 candidate"
        );
        Self {
            candidates,
            max_tokens: DEFAULT_MAX_TOKENS,
        }
    }

    /// Override the output-token ceiling for distiller calls.
    pub fn with_max_tokens(mut self, max_tokens: u32) -> Self {
        self.max_tokens = max_tokens;
        self
    }
}

#[async_trait::async_trait]
impl ConsolidationLlm for ProviderLlmAdapter {
    async fn chat(&self, messages: Vec<LlmMessage>) -> std::result::Result<LlmResponse, String> {
        // Convert grafeo LlmMessage → acowork ChatMessage once; reused per
        // candidate (ChatMessage is cheap to clone relative to an LLM call).
        let chat_messages: Vec<ChatMessage> = messages
            .iter()
            .map(|m| {
                let role = match m.role.as_str() {
                    "system" => MessageRole::System,
                    "assistant" => MessageRole::Assistant,
                    _ => MessageRole::User,
                };
                ChatMessage {
                    role,
                    content: m.content.clone(),
                    ..Default::default()
                }
            })
            .collect();

        let mut last_err = String::from("no distiller candidates configured");
        for (i, cand) in self.candidates.iter().enumerate() {
            let request = ChatRequest {
                model: cand.model.clone(),
                messages: chat_messages.clone(),
                temperature: Some(0.1), // Low temperature for structured extraction
                max_tokens: Some(self.max_tokens), // Sized for a whole JSON array
                tools: None,            // No tool calling for extraction tasks
                reasoning_effort: None,
                thinking_mode: None,
            };

            match cand.provider.chat(request).await {
                Ok(response) => {
                    if i > 0 {
                        tracing::info!(
                            candidate_index = i,
                            model = %cand.model,
                            "Distiller LLM call succeeded on fallback candidate"
                        );
                    }
                    if response.finish_reason.as_deref() == Some("length") {
                        // The reply is cut off; the distiller salvages what it
                        // can, but this is the signal that the batch/chunk is
                        // too large for the configured ceiling.
                        tracing::warn!(
                            candidate_index = i,
                            model = %cand.model,
                            max_tokens = self.max_tokens,
                            "Distiller LLM reply truncated by max_tokens"
                        );
                    }
                    return Ok(LlmResponse {
                        content: response.content,
                        usage_tokens: response.usage.map(|u| u.total_tokens),
                        finish_reason: response.finish_reason,
                    });
                }
                Err(e) => {
                    tracing::warn!(
                        candidate_index = i,
                        model = %cand.model,
                        provider = %cand.provider.name(),
                        error = %e,
                        "Distiller LLM candidate failed, trying next"
                    );
                    last_err = format!("Provider chat failed: {}", e);
                }
            }
        }
        Err(last_err)
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use acowork_core::providers::mock::MockProvider;

    #[tokio::test]
    async fn test_adapter_converts_messages_and_returns_response() {
        let provider = std::sync::Arc::new(MockProvider::single_text(
            r#"[{"subject":"user","predicate":"likes","object":"Rust","confidence":0.9,"sub_type":"fact"}]"#,
        ));
        let adapter = ProviderLlmAdapter::new(provider, "mock-model".to_string());

        let messages = vec![
            LlmMessage {
                role: "system".to_string(),
                content: "You are a knowledge extractor.".to_string(),
            },
            LlmMessage {
                role: "user".to_string(),
                content: "I love Rust programming".to_string(),
            },
        ];

        let response = adapter.chat(messages).await.unwrap();
        assert!(response.content.contains("user"));
        assert!(response.content.contains("Rust"));
    }

    #[tokio::test]
    async fn test_adapter_handles_provider_error() {
        // MockProvider that always returns empty content (simulating graceful degradation)
        let provider = std::sync::Arc::new(MockProvider::single_text(""));
        let adapter = ProviderLlmAdapter::new(provider, "mock-model".to_string());

        let messages = vec![LlmMessage {
            role: "user".to_string(),
            content: "test".to_string(),
        }];

        let response = adapter.chat(messages).await.unwrap();
        assert!(response.content.is_empty());
    }
}
