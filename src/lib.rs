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

/// Crate user agent sent on every upstream request.
///
/// Providers (notably `OpenCode Go`) ask clients to identify with their own
/// agent string rather than a generic HTTP-library default.
pub const USER_AGENT: &str = concat!("infinity-provider-models-dev/", env!("CARGO_PKG_VERSION"));

/// One advertised model: its infinity-facing id plus where to route it.
#[derive(Clone, Debug)]
struct Route {
    entry: ModelEntry,
    backend_idx: usize,
    upstream_model: String,
    default_max_tokens: Option<u64>,
    /// Registration order. `list_models` advertises in this order so the
    /// global default (the first model of the first backend) follows
    /// preset order instead of an arbitrary map order.
    order: usize,
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
        let mut order = 0_usize;
        for (idx, backend) in backends.iter().enumerate() {
            for m in &backend.models {
                let model_id = format!("{}/{}", backend.preset_id, m.upstream);
                // First backend wins on collision; ids are namespaced so this
                // only triggers for duplicate custom config.
                if routes.contains_key(&model_id) {
                    tracing::warn!("duplicate model id {model_id}, keeping first");
                    continue;
                }
                let entry = ModelEntry {
                    model_id: model_id.clone(),
                    display_name: format!("{} ({})", m.display, backend.preset_id),
                    context_window: m.context_window,
                    max_output_tokens: m.max_output_tokens,
                    supports_image_input: m.supports_images,
                };
                order = order.saturating_add(1);
                routes.insert(
                    model_id,
                    Route {
                        entry,
                        backend_idx: idx,
                        upstream_model: m.upstream.clone(),
                        default_max_tokens: m.max_output_tokens,
                        order,
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

    /// Reorder `entries` so the `INFINITY_DEFAULT_MODEL` id (if set) comes
    /// first. Existing ids move; unlisted ids on a known backend are
    /// synthesized as passthrough entries; unknown prefixes warn and keep
    /// the order.
    fn apply_default_override(&self, entries: &mut Vec<ModelEntry>, wanted: Option<String>) {
        let Some(wanted) = wanted else {
            return;
        };
        if let Some(pos) = entries.iter().position(|e| e.model_id == wanted) {
            if pos != 0 {
                let entry = entries.remove(pos);
                entries.insert(0, entry);
            }
            tracing::info!("default model pinned by {DEFAULT_MODEL_ENV}: {wanted}");
            return;
        }
        let Some((prefix, rest)) = wanted.split_once('/') else {
            tracing::warn!("{DEFAULT_MODEL_ENV}={wanted} is not a <backend>/<model> id; ignoring");
            return;
        };
        if rest.is_empty() || !self.backends.iter().any(|b| b.preset_id == prefix) {
            tracing::warn!("{DEFAULT_MODEL_ENV}={wanted} matches no known backend; ignoring");
            return;
        }
        tracing::warn!(
            "{DEFAULT_MODEL_ENV}={wanted} is not in the advertised list; \
             advertising it first as a passthrough (invoke may fail if the id is wrong)"
        );
        entries.insert(
            0,
            ModelEntry {
                model_id: wanted.clone(),
                display_name: format!("{rest} ({prefix})"),
                context_window: 200_000,
                max_output_tokens: Some(32_000),
                supports_image_input: false,
            },
        );
    }
}

/// Env var pinning the global default: when set to a full `model_id`
/// (`<backend>/<upstream-id>`), that model is advertised first from
/// `list_models` so infinity picks it as the default.
pub const DEFAULT_MODEL_ENV: &str = "INFINITY_DEFAULT_MODEL";

fn default_model_override() -> Option<String> {
    std::env::var(DEFAULT_MODEL_ENV)
        .ok()
        .map(|v| v.trim().to_owned())
        .filter(|v| !v.is_empty())
}

#[async_trait]
impl ModelProvider for MultiProvider {
    async fn list_models(&self) -> Result<Vec<ModelEntry>, BoxError> {
        // Advertise in registration order so the global default (the first
        // model of the first backend) follows preset order.
        let mut routes: Vec<&Route> = self.routes.values().collect();
        routes.sort_by_key(|r| r.order);
        let mut entries: Vec<ModelEntry> = routes.into_iter().map(|r| r.entry.clone()).collect();
        let mut known: std::collections::HashSet<String> =
            entries.iter().map(|e| e.model_id.clone()).collect();
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
                if known.contains(&model_id) {
                    continue;
                }
                // Conservative: unknown models are assumed text-only until
                // curated (an unexpected image payload fails the whole
                // turn; a placeholder does not).
                entries.push(ModelEntry {
                    model_id: model_id.clone(),
                    display_name: format!("{id} ({preset_id})"),
                    context_window: 200_000,
                    max_output_tokens: Some(32_000),
                    supports_image_input: false,
                });
                known.insert(model_id);
            }
        }
        // Env override runs after discovery so a discovered id can also be
        // pinned to the top.
        let wanted = default_model_override();
        self.apply_default_override(&mut entries, wanted);
        Ok(entries)
    }

    async fn invoke_model(
        &self,
        model_id: &str,
        request: CompletionRequest,
    ) -> Result<ModelStream, CompletionError> {
        // `<backend>/<id>` passthrough: allow unknown upstream ids on a
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
                                supports_image_input: false,
                            },
                            backend_idx: idx,
                            upstream_model: rest.to_owned(),
                            default_max_tokens: Some(32_000),
                            // Unordered: passthrough routes sort after all
                            // catalogued routes.
                            order: usize::MAX,
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
                let target = openai::Backend {
                    client: &self.client,
                    base_url: &backend.base_url,
                    api_key: &backend.api_key,
                    extra_headers: &backend.extra_headers,
                    supports_images: route.entry.supports_image_input,
                };
                openai::invoke(
                    &target,
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
                    route.entry.supports_image_input,
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

    fn two_backend_provider() -> MultiProvider {
        MultiProvider::from_backends(vec![
            backend("openai", vec![("gpt-5.4", "GPT")]),
            backend("groq", vec![("llama-3.3-70b-versatile", "Llama")]),
        ])
    }

    fn baseline_entries(p: &MultiProvider) -> Vec<ModelEntry> {
        let mut routes: Vec<&Route> = p.routes.values().collect();
        routes.sort_by_key(|r| r.order);
        routes.into_iter().map(|r| r.entry.clone()).collect()
    }

    fn ids(entries: &[ModelEntry]) -> Vec<&str> {
        entries.iter().map(|m| m.model_id.as_str()).collect()
    }

    #[test]
    fn default_override_moves_existing_model_first() {
        let p = two_backend_provider();
        let mut entries = baseline_entries(&p);
        p.apply_default_override(
            &mut entries,
            Some("groq/llama-3.3-70b-versatile".to_owned()),
        );
        assert_eq!(
            ids(&entries),
            vec!["groq/llama-3.3-70b-versatile", "openai/gpt-5.4"]
        );
    }

    #[test]
    fn default_override_synthesizes_passthrough_for_known_backend() {
        let p = two_backend_provider();
        let mut entries = baseline_entries(&p);
        p.apply_default_override(&mut entries, Some("groq/some-future-model".to_owned()));
        assert_eq!(
            ids(&entries),
            vec![
                "groq/some-future-model",
                "openai/gpt-5.4",
                "groq/llama-3.3-70b-versatile"
            ]
        );
    }

    #[test]
    fn default_override_unknown_prefix_leaves_order() {
        let p = two_backend_provider();
        let mut entries = baseline_entries(&p);
        p.apply_default_override(&mut entries, Some("nope/nothing".to_owned()));
        assert_eq!(
            ids(&entries),
            vec!["openai/gpt-5.4", "groq/llama-3.3-70b-versatile"]
        );
    }

    #[test]
    fn default_override_none_is_noop() {
        let p = two_backend_provider();
        let mut entries = baseline_entries(&p);
        p.apply_default_override(&mut entries, None);
        assert_eq!(
            ids(&entries),
            vec!["openai/gpt-5.4", "groq/llama-3.3-70b-versatile"]
        );
    }

    #[test]
    fn default_model_env_parsing_trims_and_rejects_empty() {
        let _guard = default_env_lock();
        let saved = std::env::var(DEFAULT_MODEL_ENV).ok();
        unsafe {
            std::env::set_var(DEFAULT_MODEL_ENV, "  groq/m  ");
        }
        assert_eq!(default_model_override(), Some("groq/m".to_owned()));
        unsafe {
            std::env::set_var(DEFAULT_MODEL_ENV, "   ");
        }
        assert_eq!(default_model_override(), None);
        restore_default_env(saved);
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn lists_namespaced_models_in_registration_order() {
        // Registration order (not alphabetical) decides the listing: the
        // daemon treats the first model as the global default. Isolated
        // from INFINITY_DEFAULT_MODEL which other tests mutate.
        let _guard = default_env_lock();
        let saved = std::env::var(DEFAULT_MODEL_ENV).ok();
        unsafe {
            std::env::remove_var(DEFAULT_MODEL_ENV);
        }
        let p = two_backend_provider();
        let models = p.list_models().await.unwrap();
        // `list_models` reads the env after an await point, so restore only
        // after it returns while still holding the lock.
        let list_ids: Vec<String> = models.iter().map(|m| m.model_id.clone()).collect();
        restore_default_env(saved);
        assert_eq!(
            list_ids,
            vec!["openai/gpt-5.4", "groq/llama-3.3-70b-versatile"]
        );
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn list_models_pins_default_from_env_end_to_end() {
        let _guard = default_env_lock();
        let saved = std::env::var(DEFAULT_MODEL_ENV).ok();
        unsafe {
            std::env::set_var(DEFAULT_MODEL_ENV, "groq/llama-3.3-70b-versatile");
        }
        let p = two_backend_provider();
        let models = p.list_models().await.unwrap();
        let list_ids: Vec<String> = models.iter().map(|m| m.model_id.clone()).collect();
        restore_default_env(saved);
        assert_eq!(
            list_ids,
            vec!["groq/llama-3.3-70b-versatile", "openai/gpt-5.4"]
        );
    }

    fn default_env_lock() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
        LOCK.get_or_init(|| std::sync::Mutex::new(()))
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn restore_default_env(saved: Option<String>) {
        if let Some(v) = saved {
            unsafe {
                std::env::set_var(DEFAULT_MODEL_ENV, v);
            }
        } else {
            unsafe {
                std::env::remove_var(DEFAULT_MODEL_ENV);
            }
        }
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
