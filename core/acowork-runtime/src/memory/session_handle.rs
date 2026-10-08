//! Memory session handle - agent-scoped shared state for memory tools.
//!
//! Memory tools (memory_recall, memory_store) are created once per agent,
//! but the memory provider may be initialized lazily (after tool creation).
//! This handle provides a shared, lock-protected view of the agent-level
//! resources without changing the Tool trait.
//!
//! ADR-051 C3: Primary type is now `Arc<dyn MemoryProvider>`.
//! ADR-051 C4: grafeo_store compat field removed; all callers use trait methods.
//!
//! SESSION-SCOPED STATE DELIBERATELY LIVES HERE NOT (2026-10): the handle is
//! an `Arc` shared by every concurrent SessionTask of the agent
//! (`AgentCore::clone_shallow` clones the `Arc`), so session id / current
//! user message stored on it would be last-writer-wins across sessions.
//! That data is delivered per call via
//! [`acowork_core::tools::traits::ToolContext`] instead.

use std::sync::{Arc, RwLock};

use acowork_memory::{MemoryManagerConfig, MemoryProvider};

use crate::embedding::EmbeddingProvider;

/// Agent-scoped memory resources shared between the agent loop (writer)
/// and memory tools (readers). Safe to share across concurrent sessions:
/// every field is agent-level, never per-session.
pub struct MemorySessionHandle {
    /// Memory provider (lazily initialized, shared across all sessions).
    /// ADR-051 C3: Changed from `Arc<GrafeoStore>` to `Arc<dyn MemoryProvider>`.
    provider: RwLock<Option<Arc<dyn MemoryProvider>>>,
    /// Embedding provider (set once at construction, immutable thereafter).
    embedding_provider: Option<Arc<dyn EmbeddingProvider>>,
    /// Agent-level `MemoryManagerConfig`, set once at memory initialization.
    ///
    /// Config consistency: the `memory_recall` tool reads this so it uses the
    /// SAME quality config (min_cosine / exclude_dormant / …) as auto-inject,
    /// instead of hardcoded defaults. Falls back to default when unset.
    memory_config: RwLock<Option<MemoryManagerConfig>>,
}

impl MemorySessionHandle {
    /// Create a new handle with no provider (lazy initialization).
    pub fn new(embedding_provider: Option<Arc<dyn EmbeddingProvider>>) -> Self {
        Self {
            provider: RwLock::new(None),
            embedding_provider,
            memory_config: RwLock::new(None),
        }
    }

    /// Set the memory provider once it becomes available.
    ///
    /// Called by `AgentCore` when memory initialization completes.
    pub fn set_provider(&self, provider: Arc<dyn MemoryProvider>) {
        let mut guard = self
            .provider
            .write()
            .expect("MemorySessionHandle provider lock poisoned");
        assert!(
            guard.is_none(),
            "MemorySessionHandle provider already initialized"
        );
        *guard = Some(provider);
    }

    /// Read a clone of the provider, if initialized.
    pub fn provider(&self) -> Option<Arc<dyn MemoryProvider>> {
        self.provider.read().ok().and_then(|guard| guard.clone())
    }

    /// Read a clone of the embedding provider, if set.
    pub fn embedding(&self) -> Option<Arc<dyn EmbeddingProvider>> {
        self.embedding_provider.clone()
    }

    /// Set the agent's memory manager config.
    ///
    /// Called at memory init AND whenever a live runtime-config update
    /// changes retrieval-affecting settings (e.g. the memory-forgetting
    /// toggle, ADR-057 §5.3) so the `memory_recall` tool observes the same
    /// config as auto-inject without a restart.
    pub fn set_memory_config(&self, config: MemoryManagerConfig) {
        if let Ok(mut guard) = self.memory_config.write() {
            *guard = Some(config);
        }
    }

    /// Read a clone of the agent's memory manager config, if set.
    pub fn memory_config(&self) -> Option<MemoryManagerConfig> {
        self.memory_config
            .read()
            .ok()
            .and_then(|guard| guard.clone())
    }
}
