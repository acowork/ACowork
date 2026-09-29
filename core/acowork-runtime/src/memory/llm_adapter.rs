//! LLM adapter — bridges `acowork_core::providers::traits::Provider` to
//! `acowork_memory::consolidation::triple_extraction::TripleExtractorLlm`.
//!
//! The grafeo crate defines a minimal LLM trait (`TripleExtractorLlm`) so it
//! stays independent of the runtime's provider ecosystem. This adapter wraps
//! a `dyn Provider` (which has a full chat API with streaming, tool calls, etc.)
//! into the simple `async chat(messages) -> response` interface that grafeo
//! expects, using a low-temperature non-streaming call.

use acowork_core::providers::traits::{ChatMessage, ChatRequest, MessageRole, Provider};
use acowork_memory::consolidation::{LlmMessage, LlmResponse, TripleExtractorLlm};

// ---------------------------------------------------------------------------
// Adapter
// ---------------------------------------------------------------------------

/// Adapter: wraps a `dyn Provider` as a `TripleExtractorLlm`.
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
}

/// One distillation target: a provider instance plus the model to request.
pub struct DistillCandidate {
    pub provider: std::sync::Arc<dyn Provider>,
    pub model: String,
}

impl ProviderLlmAdapter {
    /// Create a new single-candidate adapter from a Provider and model name.
    pub fn new(provider: std::sync::Arc<dyn Provider>, model: String) -> Self {
        Self::with_candidates(vec![DistillCandidate { provider, model }])
    }

    /// Create an adapter with an ordered candidate fallback chain.
    /// Panics-free on empty list: a single placeholder candidate must be
    /// supplied by the caller (resolution chains always end with one).
    pub fn with_candidates(candidates: Vec<DistillCandidate>) -> Self {
        debug_assert!(!candidates.is_empty(), "ProviderLlmAdapter needs ≥1 candidate");
        Self { candidates }
    }
}

#[async_trait::async_trait]
impl TripleExtractorLlm for ProviderLlmAdapter {
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
                max_tokens: Some(2048), // Enough for triple arrays / classification JSON
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
                    return Ok(LlmResponse {
                        content: response.content,
                        usage_tokens: response.usage.map(|u| u.total_tokens),
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
