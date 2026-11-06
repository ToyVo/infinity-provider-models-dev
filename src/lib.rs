//! Multi-backend model provider for Infinity, configured through the same
//! API-key env vars that [pi](https://pi.dev/docs/latest/providers) and
//! [opencode](https://opencode.ai/v2/docs/providers/) understand.
//!
//! Each configured upstream backend contributes its models under
//! `<backend>/<model>` ids (e.g. `groq/llama-3.3-70b-versatile`,
//! `anthropic/claude-sonnet-4-6`). The provider speaks `OpenAI` Chat
//! Completions streaming for `OpenAI`-compatible backends and `Anthropic`
//! Messages streaming for `Anthropic`, and translates both into the infinity
//! [`ModelProvider`] protocol — the same shape as
//! `infinity-provider-bedrock`, but for every non-Amazon provider.
//!
//! ```bash
//! export GROQ_API_KEY=...
//! export OPENROUTER_API_KEY=...
//! infinity-provider-models-dev   # prints a unix socket path, serves the provider
//! ```
//!
//! `~/.infinity/providers.json`:
//! ```json
//! { "models_dev": { "command": ["infinity-provider-models-dev"] } }
//! ```

pub mod anthropic;
pub mod catalog_generated;
pub mod convert;
pub mod oauth;
pub mod openai;
pub mod preset;

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use infinity_provider_protocol::{
    CompletionError, CompletionRequest, ErrorClass, ModelEntry, ModelProvider, ModelStream,
};
use preset::{Flavor, ResolvedBackend};

type BoxError = Box<dyn std::error::Error + Send + Sync>;

/// One advertised model: its infinity-facing id plus where to route it.
#[derive(Clone, Debug)]
struct Route {
    entry: ModelEntry,
    backend_idx: usize,
    upstream_model: String,
    default_max_tokens: Option<u64>,
}

/// [`ModelProvider`] that fans out to every env-configured backend.
pub struct MultiProvider {
    client: reqwest::Client,
    backends: Vec<ResolvedBackend>,
    routes: HashMap<String, Route>,
}

impl MultiProvider {
    /// Build from the current process environment. Logs configured backends
    /// to stderr (stdout is reserved for the socket path).
    #[must_use]
    pub fn from_env() -> Self {
        Self::from_backends(preset::resolve_backends())
    }

    #[must_use]
    pub fn from_backends(backends: Vec<ResolvedBackend>) -> Self {
        let client = reqwest::Client::new();
        let mut routes = HashMap::new();
        for (idx, backend) in backends.iter().enumerate() {
            for m in &backend.models {
                let model_id = format!("{}/{}", backend.preset_id, m.upstream);
                // First backend wins on collision; ids are namespaced so this
                // only triggers for duplicate custom config.
                if routes.contains_key(&model_id) {
                    tracing::warn!("duplicate model id {model_id}, keeping first");
                    continue;
                }
                routes.insert(
                    model_id.clone(),
                    Route {
                        entry: ModelEntry {
                            model_id: model_id.clone(),
                            display_name: format!("{} ({})", m.display, backend.preset_id),
                            context_window: m.context_window,
                            max_output_tokens: m.max_output_tokens,
                            supports_image_input: m.supports_images,
                        },
                        backend_idx: idx,
                        upstream_model: m.upstream.clone(),
                        default_max_tokens: m.max_output_tokens,
                    },
                );
            }
        }
        for b in &backends {
            tracing::info!(
                "backend {}: {} model(s) at {}",
                b.preset_id,
                b.models.len(),
                b.base_url,
            );
        }
        if backends.is_empty() {
            tracing::warn!(
                "no backends configured: set one of the pi/opencode API-key env vars \
                 (e.g. OPENAI_API_KEY, ANTHROPIC_API_KEY, GROQ_API_KEY, OPENROUTER_API_KEY)"
            );
        }
        Self {
            client,
            backends,
            routes,
        }
    }

    /// Backends in resolution order (for debugging / tests).
    #[must_use]
    pub fn backends(&self) -> &[ResolvedBackend] {
        &self.backends
    }

    /// Share the provider behind `Arc` for `serve_provider`.
    #[must_use]
    pub fn into_arc(self) -> Arc<Self> {
        Arc::new(self)
    }
}

#[async_trait]
impl ModelProvider for MultiProvider {
    async fn list_models(&self) -> Result<Vec<ModelEntry>, BoxError> {
        let mut entries: Vec<ModelEntry> = self.routes.values().map(|r| r.entry.clone()).collect();
        // Live discovery runs concurrently across backends; each probe is
        // timeout-guarded and falls back to the curated list on failure.
        // Discovered ids need no route entries: `invoke_model` passes any
        // `<backend>/<id>` through to its backend.
        let probed = futures_util::future::join_all(
            self.backends.iter().filter(|b| b.discover).map(|b| async {
                let ids =
                    crate::openai::discover_models(&self.client, &b.base_url, &b.api_key).await;
                (b.preset_id, ids)
            }),
        )
        .await;
        for (preset_id, ids) in probed {
            for id in ids {
                let model_id = format!("{preset_id}/{id}");
                if entries.iter().any(|e| e.model_id == model_id) {
                    continue;
                }
                entries.push(ModelEntry {
                    model_id,
                    display_name: format!("{id} ({preset_id})"),
                    context_window: 200_000,
                    max_output_tokens: Some(32_000),
                    supports_image_input: true,
                });
            }
        }
        entries.sort_by(|a, b| a.model_id.cmp(&b.model_id));
        Ok(entries)
    }

    async fn invoke_model(
        &self,
        model_id: &str,
        request: CompletionRequest,
    ) -> Result<ModelStream, CompletionError> {
        // `bedrock/<id>` passthrough: allow unknown bedrock-style ids on a
        // backend whose preset id matches the prefix, so callers can address
        // upstream models not in the advertised list.
        let route = self.routes.get(model_id).cloned().or_else(|| {
            model_id.split_once('/').and_then(|(prefix, rest)| {
                self.backends.iter().enumerate().find_map(|(idx, b)| {
                    if b.preset_id == prefix && !rest.is_empty() {
                        Some(Route {
                            entry: ModelEntry {
                                model_id: model_id.to_owned(),
                                display_name: model_id.to_owned(),
                                context_window: 200_000,
                                max_output_tokens: Some(32_000),
                                supports_image_input: true,
                            },
                            backend_idx: idx,
                            upstream_model: rest.to_owned(),
                            default_max_tokens: Some(32_000),
                        })
                    } else {
                        None
                    }
                })
            })
        });
        let Some(route) = route else {
            return Err(CompletionError::provider(
                ErrorClass::Fatal,
                format!("unknown model: {model_id}"),
            ));
        };
        let Some(backend) = self.backends.get(route.backend_idx) else {
            return Err(CompletionError::provider(
                ErrorClass::Fatal,
                format!("unknown backend for model: {model_id}"),
            ));
        };
        match backend.flavor {
            Flavor::OpenAi => {
                openai::invoke(
                    &self.client,
                    &backend.base_url,
                    &backend.api_key,
                    &backend.extra_headers,
                    &route.upstream_model,
                    route.default_max_tokens,
                    request,
                )
                .await
            }
            Flavor::Anthropic => {
                let target = anthropic::Backend {
                    client: &self.client,
                    base_url: &backend.base_url,
                    api_key: &backend.api_key,
                    extra_headers: &backend.extra_headers,
                    oauth: backend.oauth.as_ref(),
                };
                anthropic::invoke(
                    &target,
                    &route.upstream_model,
                    route.default_max_tokens,
                    request,
                )
                .await
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn backend(id: &'static str, models: Vec<(&str, &str)>) -> ResolvedBackend {
        ResolvedBackend {
            preset_id: id,
            flavor: Flavor::OpenAi,
            base_url: "https://example.test/v1".to_owned(),
            api_key: "test".to_owned(),
            models: models
                .into_iter()
                .map(|(m, d)| crate::preset::BackendModel {
                    upstream: m.to_owned(),
                    display: d.to_owned(),
                    context_window: 128_000,
                    max_output_tokens: Some(8_000),
                    supports_images: true,
                })
                .collect(),
            oauth: None,
            discover: false,
            extra_headers: Vec::new(),
        }
    }

    #[tokio::test]
    async fn lists_namespaced_models_sorted() {
        let p = MultiProvider::from_backends(vec![
            backend("groq", vec![("llama-3.3-70b-versatile", "Llama")]),
            backend("openai", vec![("gpt-5.4", "GPT")]),
        ]);
        let models = p.list_models().await.unwrap();
        let ids: Vec<_> = models.iter().map(|m| m.model_id.as_str()).collect();
        assert_eq!(ids, vec!["groq/llama-3.3-70b-versatile", "openai/gpt-5.4"]);
    }

    #[tokio::test]
    async fn unknown_model_is_fatal() {
        let p = MultiProvider::from_backends(vec![]);
        let res = p
            .invoke_model(
                "nope/nothing",
                CompletionRequest {
                    chat_history: vec![infinity_provider_protocol::Message::user("hi")],
                    ..Default::default()
                },
            )
            .await;
        let Err(err) = res else {
            panic!("unknown model must fail");
        };
        assert_eq!(err.class(), ErrorClass::Fatal);
    }

    #[tokio::test]
    async fn unlisted_upstream_model_passes_through_on_known_backend() {
        // No network: the passthrough resolves routing, then fails at
        // connect — but with a non-fatal transport class, proving it routed
        // to the backend instead of rejecting the id.
        let p = MultiProvider::from_backends(vec![backend(
            "groq",
            vec![("llama-3.3-70b-versatile", "Llama")],
        )]);
        let res = p
            .invoke_model(
                "groq/some-future-model",
                CompletionRequest {
                    chat_history: vec![infinity_provider_protocol::Message::user("hi")],
                    ..Default::default()
                },
            )
            .await;
        let Err(err) = res else {
            panic!("unreachable example endpoint must fail");
        };
        assert_ne!(err.class(), ErrorClass::Fatal);
    }
}
