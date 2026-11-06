//! Backend preset table: one entry per upstream provider/gateway.
//!
//! Each preset maps the API-key env vars documented by
//! [pi](https://pi.dev/docs/latest/providers) and the provider catalog used
//! by [opencode](https://opencode.ai/v2/docs/providers/) (via models.dev)
//! to an `OpenAI`-compatible (or `Anthropic`-native) base URL plus a small
//! default model list. Presets activate when one of their key env vars is
//! set; everything else has a default so `export FOO_API_KEY=...` is enough.
//!
//! Per-backend overrides (all optional):
//! - `<PREFIX>_BASE_URL` — override the endpoint (also honors the well-known
//!   vars listed in `base_envs`, e.g. `OPENAI_BASE_URL`).
//! - `<PREFIX>_MODELS` — comma-separated model ids to advertise instead of
//!   the built-ins.
//! - `<PREFIX>_MODEL` — single model id (same effect, one entry).
//!
//! A fully custom `OpenAI`-compatible endpoint works without a preset:
//! - `INFINITY_OPENAI_BASE_URL` + `INFINITY_OPENAI_API_KEY` (+ optional
//!   `INFINITY_OPENAI_MODELS`).

use std::env;

/// Wire flavor of a backend.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Flavor {
    /// `OpenAI` Chat Completions (`/chat/completions`, SSE `data:` frames).
    OpenAi,
    /// `Anthropic` Messages (`/v1/messages`, SSE `event:` frames).
    Anthropic,
}

/// A built-in model advertised for a backend.
#[derive(Clone, Debug)]
pub struct PresetModel {
    pub id: &'static str,
    pub display_name: &'static str,
    pub context_window: usize,
    pub max_output_tokens: Option<u64>,
    /// Whether the model accepts image input.
    pub supports_images: bool,
}

/// One model offered by a resolved backend.
#[derive(Clone, Debug)]
pub struct BackendModel {
    /// Upstream model id sent to the endpoint.
    pub upstream: String,
    /// Human-readable name shown in pickers.
    pub display: String,
    /// Context window size in tokens (used for compaction thresholds).
    pub context_window: usize,
    /// Maximum output tokens; `None` falls back to the provider default.
    pub max_output_tokens: Option<u64>,
    /// Whether the model accepts image input.
    pub supports_images: bool,
}

/// One upstream provider/gateway.
#[derive(Clone, Debug)]
pub struct Preset {
    /// Short stable id, used as a prefix in infinity model ids
    /// (`<id>/<model>`), e.g. `groq/llama-3.3-70b-versatile`.
    pub id: &'static str,
    /// Env prefix for `<PREFIX>_BASE_URL` / `<PREFIX>_MODELS` overrides.
    pub prefix: &'static str,
    /// API-key env vars (first one set wins). Empty = no key needed (local).
    pub key_envs: &'static [&'static str],
    /// Extra env vars honored for the base URL (besides `<PREFIX>_BASE_URL`).
    pub base_envs: &'static [&'static str],
    /// Default base URL when no override is set.
    pub default_base_url: Option<&'static str>,
    pub flavor: Flavor,
    pub models: &'static [PresetModel],
    /// Whether the endpoint needs no auth (local servers).
    pub no_auth: bool,
    /// Whether this preset uses Claude subscription `OAuth` (see `oauth`).
    pub oauth_subscription: bool,
}

pub const PRESETS: &[Preset] = &[
    Preset {
        id: "openai",
        prefix: "OPENAI",
        key_envs: &["OPENAI_API_KEY"],
        base_envs: &["OPENAI_BASE_URL"],
        default_base_url: Some("https://api.openai.com/v1"),
        flavor: Flavor::OpenAi,
        no_auth: false,
        oauth_subscription: false,
        models: crate::catalog_generated::OPENAI,
    },
    Preset {
        id: "anthropic",
        prefix: "ANTHROPIC",
        key_envs: &[
            "ANTHROPIC_API_KEY",
            "ANTHROPIC_AUTH_TOKEN",
            "ANTHROPIC_OAUTH_TOKEN",
        ],
        base_envs: &["ANTHROPIC_BASE_URL"],
        default_base_url: Some("https://api.anthropic.com/v1"),
        flavor: Flavor::Anthropic,
        no_auth: false,
        oauth_subscription: false,
        models: crate::catalog_generated::ANTHROPIC,
    },
    Preset {
        id: "claude-oauth",
        prefix: "CLAUDE_OAUTH",
        key_envs: &[],
        base_envs: &["CLAUDE_OAUTH_BASE_URL"],
        default_base_url: Some("https://api.anthropic.com"),
        flavor: Flavor::Anthropic,
        no_auth: false,
        oauth_subscription: true,
        models: crate::catalog_generated::CLAUDE_OAUTH,
    },
    Preset {
        id: "deepseek",
        prefix: "DEEPSEEK",
        key_envs: &["DEEPSEEK_API_KEY"],
        base_envs: &["DEEPSEEK_BASE_URL"],
        default_base_url: Some("https://api.deepseek.com/v1"),
        flavor: Flavor::OpenAi,
        no_auth: false,
        oauth_subscription: false,
        models: crate::catalog_generated::DEEPSEEK,
    },
    Preset {
        id: "groq",
        prefix: "GROQ",
        key_envs: &["GROQ_API_KEY"],
        base_envs: &["GROQ_BASE_URL"],
        default_base_url: Some("https://api.groq.com/openai/v1"),
        flavor: Flavor::OpenAi,
        no_auth: false,
        oauth_subscription: false,
        models: crate::catalog_generated::GROQ,
    },
    Preset {
        id: "cerebras",
        prefix: "CEREBRAS",
        key_envs: &["CEREBRAS_API_KEY"],
        base_envs: &["CEREBRAS_BASE_URL"],
        default_base_url: Some("https://api.cerebras.ai/v1"),
        flavor: Flavor::OpenAi,
        no_auth: false,
        oauth_subscription: false,
        models: crate::catalog_generated::CEREBRAS,
    },
    Preset {
        id: "xai",
        prefix: "XAI",
        key_envs: &["XAI_API_KEY"],
        base_envs: &["XAI_BASE_URL"],
        default_base_url: Some("https://api.x.ai/v1"),
        flavor: Flavor::OpenAi,
        no_auth: false,
        oauth_subscription: false,
        models: crate::catalog_generated::XAI,
    },
    Preset {
        id: "mistral",
        prefix: "MISTRAL",
        key_envs: &["MISTRAL_API_KEY"],
        base_envs: &["MISTRAL_BASE_URL"],
        default_base_url: Some("https://api.mistral.ai/v1"),
        flavor: Flavor::OpenAi,
        no_auth: false,
        oauth_subscription: false,
        models: crate::catalog_generated::MISTRAL,
    },
    Preset {
        id: "gemini",
        prefix: "GEMINI",
        key_envs: &[
            "GEMINI_API_KEY",
            "GOOGLE_GENERATIVE_AI_API_KEY",
            "GOOGLE_API_KEY",
        ],
        base_envs: &["GEMINI_BASE_URL", "GOOGLE_GENERATIVE_AI_BASE_URL"],
        default_base_url: Some("https://generativelanguage.googleapis.com/v1beta/openai/"),
        flavor: Flavor::OpenAi,
        no_auth: false,
        oauth_subscription: false,
        models: crate::catalog_generated::GEMINI,
    },
    Preset {
        id: "together",
        prefix: "TOGETHER",
        key_envs: &["TOGETHER_API_KEY"],
        base_envs: &["TOGETHER_BASE_URL"],
        default_base_url: Some("https://api.together.xyz/v1"),
        flavor: Flavor::OpenAi,
        no_auth: false,
        oauth_subscription: false,
        models: crate::catalog_generated::TOGETHER,
    },
    Preset {
        id: "fireworks",
        prefix: "FIREWORKS",
        key_envs: &["FIREWORKS_API_KEY"],
        base_envs: &["FIREWORKS_BASE_URL"],
        default_base_url: Some("https://api.fireworks.ai/inference/v1"),
        flavor: Flavor::OpenAi,
        no_auth: false,
        oauth_subscription: false,
        models: crate::catalog_generated::FIREWORKS,
    },
    Preset {
        id: "openrouter",
        prefix: "OPENROUTER",
        key_envs: &["OPENROUTER_API_KEY"],
        base_envs: &["OPENROUTER_BASE_URL"],
        default_base_url: Some("https://openrouter.ai/api/v1"),
        flavor: Flavor::OpenAi,
        no_auth: false,
        oauth_subscription: false,
        models: crate::catalog_generated::OPENROUTER,
    },
    Preset {
        id: "nvidia",
        prefix: "NVIDIA",
        key_envs: &["NVIDIA_API_KEY"],
        base_envs: &["NVIDIA_BASE_URL"],
        default_base_url: Some("https://integrate.api.nvidia.com/v1"),
        flavor: Flavor::OpenAi,
        no_auth: false,
        oauth_subscription: false,
        models: crate::catalog_generated::NVIDIA,
    },
    Preset {
        id: "moonshot",
        prefix: "MOONSHOT",
        key_envs: &["MOONSHOT_API_KEY", "KIMI_API_KEY"],
        base_envs: &["MOONSHOT_BASE_URL", "KIMI_BASE_URL"],
        default_base_url: Some("https://api.moonshot.ai/v1"),
        flavor: Flavor::OpenAi,
        no_auth: false,
        oauth_subscription: false,
        models: crate::catalog_generated::MOONSHOT,
    },
    Preset {
        id: "zai",
        prefix: "ZAI",
        key_envs: &["ZAI_API_KEY"],
        base_envs: &["ZAI_BASE_URL"],
        default_base_url: Some("https://api.z.ai/api/coding/paas/v4"),
        flavor: Flavor::OpenAi,
        no_auth: false,
        oauth_subscription: false,
        models: crate::catalog_generated::ZAI,
    },
    Preset {
        id: "minimax",
        prefix: "MINIMAX",
        key_envs: &["MINIMAX_API_KEY", "MINIMAX_CN_API_KEY"],
        base_envs: &["MINIMAX_BASE_URL"],
        default_base_url: Some("https://api.minimax.io/v1"),
        flavor: Flavor::OpenAi,
        no_auth: false,
        oauth_subscription: false,
        models: crate::catalog_generated::MINIMAX,
    },
    Preset {
        id: "qwen",
        prefix: "QWEN",
        key_envs: &[
            "QWEN_TOKEN_PLAN_API_KEY",
            "QWEN_TOKEN_PLAN_CN_API_KEY",
            "DASHSCOPE_API_KEY",
        ],
        base_envs: &["QWEN_BASE_URL", "DASHSCOPE_BASE_URL"],
        default_base_url: Some("https://dashscope-intl.aliyuncs.com/compatible-mode/v1"),
        flavor: Flavor::OpenAi,
        no_auth: false,
        oauth_subscription: false,
        models: crate::catalog_generated::QWEN,
    },
    Preset {
        id: "baseten",
        prefix: "BASETEN",
        key_envs: &["BASETEN_API_KEY"],
        base_envs: &["BASETEN_BASE_URL"],
        default_base_url: None, // model endpoint required
        flavor: Flavor::OpenAi,
        no_auth: false,
        oauth_subscription: false,
        models: &[],
    },
    Preset {
        id: "huggingface",
        prefix: "HF",
        key_envs: &["HF_TOKEN", "HUGGINGFACE_API_KEY"],
        base_envs: &["HF_BASE_URL"],
        default_base_url: Some("https://router.huggingface.co/v1"),
        flavor: Flavor::OpenAi,
        no_auth: false,
        oauth_subscription: false,
        models: crate::catalog_generated::HUGGINGFACE,
    },
    Preset {
        id: "vercel-gateway",
        prefix: "AI_GATEWAY",
        key_envs: &["AI_GATEWAY_API_KEY"],
        base_envs: &["AI_GATEWAY_BASE_URL"],
        default_base_url: Some("https://ai-gateway.vercel.sh/v1"),
        flavor: Flavor::OpenAi,
        no_auth: false,
        oauth_subscription: false,
        models: crate::catalog_generated::VERCEL_GATEWAY,
    },
    Preset {
        id: "opencode-zen",
        prefix: "OPENCODE",
        key_envs: &["OPENCODE_API_KEY"],
        base_envs: &["OPENCODE_BASE_URL"],
        default_base_url: Some("https://opencode.ai/zen/v1"),
        flavor: Flavor::OpenAi,
        no_auth: false,
        oauth_subscription: false,
        models: crate::catalog_generated::OPENCODE_ZEN,
    },
    Preset {
        id: "azure",
        prefix: "AZURE_OPENAI",
        key_envs: &["AZURE_OPENAI_API_KEY", "AZURE_API_KEY"],
        base_envs: &["AZURE_OPENAI_BASE_URL", "AZURE_OPENAI_ENDPOINT"],
        default_base_url: None, // derived from RESOURCE_NAME or BASE_URL
        flavor: Flavor::OpenAi,
        no_auth: false,
        oauth_subscription: false,
        models: crate::catalog_generated::AZURE,
    },
    Preset {
        id: "ollama",
        prefix: "OLLAMA",
        key_envs: &[],
        base_envs: &["OLLAMA_HOST", "OLLAMA_BASE_URL"],
        default_base_url: Some("http://127.0.0.1:11434/v1"),
        flavor: Flavor::OpenAi,
        models: &[PresetModel {
            id: "qwen3-coder:32b",
            display_name: "Qwen3 Coder 32B (Ollama)",
            context_window: 128_000,
            max_output_tokens: Some(32_000),
            supports_images: true,
        }],
        no_auth: true,
        oauth_subscription: false,
    },
    Preset {
        id: "lmstudio",
        prefix: "LMSTUDIO",
        key_envs: &[],
        base_envs: &["LMSTUDIO_BASE_URL"],
        default_base_url: Some("http://127.0.0.1:1234/v1"),
        flavor: Flavor::OpenAi,
        models: &[],
        no_auth: true,
        oauth_subscription: false,
    },
    Preset {
        id: "vllm",
        prefix: "VLLM",
        key_envs: &[],
        base_envs: &["VLLM_BASE_URL"],
        default_base_url: Some("http://127.0.0.1:8000/v1"),
        flavor: Flavor::OpenAi,
        models: &[],
        no_auth: true,
        oauth_subscription: false,
    },
];

/// A backend resolved from the environment and ready to serve.
#[derive(Clone, Debug)]
pub struct ResolvedBackend {
    pub preset_id: &'static str,
    pub flavor: Flavor,
    pub base_url: String,
    pub api_key: String,
    /// Models offered by this backend.
    pub models: Vec<BackendModel>,
    /// Subscription session for the `claude-oauth` preset; `None` otherwise.
    pub oauth: Option<crate::oauth::OAuthSessionHandle>,
    /// Enrich the curated list with a live `GET {base}/models` probe.
    pub discover: bool,
    /// Extra default headers (e.g. `OpenRouter` app attribution).
    pub extra_headers: Vec<(String, String)>,
}

fn non_empty(var: &str) -> Option<String> {
    env::var(var)
        .ok()
        .map(|v| v.trim().to_owned())
        .filter(|v| !v.is_empty())
}

fn first_set(vars: &[&str]) -> Option<String> {
    vars.iter().find_map(|v| non_empty(v))
}

fn model_overrides(prefix: &str) -> Option<Vec<String>> {
    if let Some(list) = non_empty(&format!("{prefix}_MODELS")) {
        let ids: Vec<String> = list
            .split(',')
            .map(|s| s.trim().to_owned())
            .filter(|s| !s.is_empty())
            .collect();
        if !ids.is_empty() {
            return Some(ids);
        }
    }
    non_empty(&format!("{prefix}_MODEL")).map(|m| vec![m])
}

fn display_for(id: &str) -> String {
    id.to_owned()
}

impl Preset {
    fn resolve(&self) -> Option<ResolvedBackend> {
        if self.oauth_subscription {
            return self.resolve_oauth();
        }
        if self.no_auth {
            // Local servers: only enable when explicitly opted in so a fresh
            // install without Ollama running doesn't advertise dead models.
            let enabled = env::var("INFINITY_LOCAL").ok().as_deref() == Some("1")
                || self.base_envs.iter().any(|v| non_empty(v).is_some())
                || non_empty(&format!("{}_MODELS", self.prefix)).is_some()
                || non_empty(&format!("{}_MODEL", self.prefix)).is_some();
            if !enabled {
                return None;
            }
            let base_url =
                first_set(self.base_envs).or_else(|| self.default_base_url.map(str::to_owned))?;
            let models = resolve_models(self, &base_url);
            if models.is_empty() {
                return None;
            }
            // Local servers always probe `GET {base}/models`: the user
            // controls what is pulled, so everything served is relevant.
            return Some(ResolvedBackend {
                preset_id: self.id,
                flavor: self.flavor,
                base_url: trim_trailing_slash(&base_url).to_owned(),
                api_key: String::new(),
                oauth: None,
                discover: true,
                models,
                extra_headers: Vec::new(),
            });
        }

        let api_key = first_set(self.key_envs)?;
        // `<PREFIX>_DISCOVER=1` merges the endpoint's live model list into
        // the curated catalog (off by default: cloud catalogs are noisy).
        let discover = std::env::var(format!("{}_DISCOVER", self.prefix))
            .ok()
            .as_deref()
            == Some("1");
        let mut base_url = first_set(self.base_envs)
            .or_else(|| non_empty(&format!("{}_BASE_URL", self.prefix)))
            .or_else(|| self.default_base_url.map(str::to_owned));

        // Azure: derive `https://{resource}.openai.azure.com/openai/v1` when
        // only a resource name was given.
        if self.id == "azure" && base_url.is_none() {
            if let Some(resource) =
                non_empty("AZURE_OPENAI_RESOURCE_NAME").or_else(|| non_empty("AZURE_RESOURCE_NAME"))
            {
                base_url = Some(format!(
                    "https://{}.openai.azure.com/openai/v1",
                    resource.trim()
                ));
            }
        }
        // Baseten / Azure with no endpoint: key alone is not enough.
        let base_url = base_url?;
        let models = resolve_models(self, &base_url);
        if models.is_empty() && !discover {
            return None;
        }

        let mut extra_headers = Vec::new();
        if self.id == "openrouter" {
            extra_headers.push((
                "HTTP-Referer".to_owned(),
                non_empty("OPENROUTER_APP_URL")
                    .unwrap_or_else(|| "https://github.com/hydro-project/infinity".to_owned()),
            ));
            extra_headers.push((
                "X-Title".to_owned(),
                non_empty("OPENROUTER_APP_NAME").unwrap_or_else(|| "infinity".to_owned()),
            ));
        }

        Some(ResolvedBackend {
            preset_id: self.id,
            flavor: self.flavor,
            base_url: trim_trailing_slash(&base_url).to_owned(),
            api_key,
            oauth: None,
            discover,
            models,
            extra_headers,
        })
    }
}

impl Preset {
    /// Resolve the Claude subscription preset: explicit opt-in plus
    /// credentials from the environment, our login file, or `Claude Code`.
    fn resolve_oauth(&self) -> Option<ResolvedBackend> {
        use crate::oauth::{self, OAuthSession, OAuthSessionHandle};
        use std::sync::{Arc, RwLock};

        let static_token = non_empty(oauth::STATIC_TOKEN_ENV);
        let opted_in =
            std::env::var(oauth::OPT_IN_ENV).ok().as_deref() == Some("1") || static_token.is_some();
        if !opted_in {
            return None;
        }
        let session = if let Some(token) = static_token {
            OAuthSession {
                access_token: token,
                refresh_token: String::new(),
                expires_at_ms: i64::MAX,
                persist_path: None,
            }
        } else if let Some(loaded) = oauth::load_credentials() {
            if loaded.access_token.trim().is_empty() {
                return None;
            }
            OAuthSession {
                access_token: loaded.access_token,
                refresh_token: loaded.refresh_token,
                expires_at_ms: loaded.expires_at_ms,
                persist_path: loaded.persist_path,
            }
        } else {
            tracing::warn!(
                "claude-oauth: opted in but no credentials found; run \
                 `infinity-provider-models-dev login`, `claude auth login`, \
                 or set ANTHROPIC_OAUTH_TOKEN"
            );
            return None;
        };
        if !session.refresh_token.trim().is_empty()
            && oauth::needs_refresh(session.expires_at_ms, oauth::now_ms())
        {
            tracing::info!("claude-oauth: stored bearer expired; will refresh on first use");
        }
        let base_url =
            first_set(self.base_envs).or_else(|| self.default_base_url.map(str::to_owned))?;
        let base_url = trim_trailing_slash(&base_url).to_owned();
        let models = resolve_models(self, &base_url);
        if models.is_empty() {
            return None;
        }
        let handle: OAuthSessionHandle = Arc::new(RwLock::new(session));
        Some(ResolvedBackend {
            preset_id: self.id,
            flavor: self.flavor,
            base_url,
            api_key: String::new(),
            oauth: Some(handle),
            discover: false,
            models,
            extra_headers: Vec::new(),
        })
    }
}

fn preset_to_backend(m: &PresetModel) -> BackendModel {
    BackendModel {
        upstream: m.id.to_owned(),
        display: m.display_name.to_owned(),
        context_window: m.context_window,
        max_output_tokens: m.max_output_tokens,
        supports_images: m.supports_images,
    }
}

fn resolve_models(preset: &Preset, _base_url: &str) -> Vec<BackendModel> {
    if let Some(ids) = model_overrides(preset.prefix) {
        return ids
            .into_iter()
            .map(|id| {
                let builtin = preset.models.iter().find(|m| m.id == id);
                builtin.map_or_else(
                    || BackendModel {
                        upstream: id.clone(),
                        display: display_for(&id),
                        context_window: 200_000,
                        max_output_tokens: Some(32_000),
                        supports_images: true,
                    },
                    preset_to_backend,
                )
            })
            .collect();
    }
    preset.models.iter().map(preset_to_backend).collect()
}

/// Scan the environment for every configured backend, in preset order.
pub fn resolve_backends() -> Vec<ResolvedBackend> {
    let mut backends: Vec<ResolvedBackend> = PRESETS.iter().filter_map(Preset::resolve).collect();

    // Fully custom OpenAI-compatible endpoint (any proxy/gateway).
    // Without `INFINITY_OPENAI_MODELS` the model list comes from a live
    // `GET {base}/models` probe instead.
    if let Some(base_url) = non_empty("INFINITY_OPENAI_BASE_URL") {
        let api_key = non_empty("INFINITY_OPENAI_API_KEY").unwrap_or_default();
        let ids = non_empty("INFINITY_OPENAI_MODELS")
            .map(|list| {
                list.split(',')
                    .map(|s| s.trim().to_owned())
                    .filter(|s| !s.is_empty())
                    .collect::<Vec<_>>()
            })
            .filter(|v| !v.is_empty())
            .unwrap_or_default();
        backends.push(ResolvedBackend {
            preset_id: "custom",
            flavor: Flavor::OpenAi,
            base_url: trim_trailing_slash(&base_url).to_owned(),
            api_key,
            oauth: None,
            discover: true,
            models: ids
                .into_iter()
                .map(|id| BackendModel {
                    upstream: id.clone(),
                    display: display_for(&id),
                    context_window: 200_000,
                    max_output_tokens: Some(32_000),
                    supports_images: true,
                })
                .collect(),
            extra_headers: Vec::new(),
        });
    }

    backends
}

fn trim_trailing_slash(s: &str) -> &str {
    s.trim_end_matches('/')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_overrides_empty_without_env() {
        // A prefix that is never set must yield no overrides.
        assert!(model_overrides("INFINITY_PROVIDER_MODELS_DEV_UNSET_TEST_PREFIX").is_none());
    }

    /// Opt-in gating and static-token activation run in one test because they
    /// mutate the same process-global env vars.
    #[test]
    fn oauth_preset_opt_in_flow() {
        let saved_token = std::env::var("ANTHROPIC_OAUTH_TOKEN").ok();
        let saved_opt_in = std::env::var("INFINITY_CLAUDE_OAUTH").ok();
        let restore = |saved_token: &Option<String>, saved_opt_in: &Option<String>| {
            if let Some(token) = saved_token {
                unsafe {
                    std::env::set_var("ANTHROPIC_OAUTH_TOKEN", token);
                }
            } else {
                unsafe {
                    std::env::remove_var("ANTHROPIC_OAUTH_TOKEN");
                }
            }
            if let Some(opt_in) = saved_opt_in {
                unsafe {
                    std::env::set_var("INFINITY_CLAUDE_OAUTH", opt_in);
                }
            } else {
                unsafe {
                    std::env::remove_var("INFINITY_CLAUDE_OAUTH");
                }
            }
        };
        let preset = PRESETS
            .iter()
            .find(|p| p.id == "claude-oauth")
            .expect("claude-oauth preset exists");

        unsafe {
            std::env::remove_var("ANTHROPIC_OAUTH_TOKEN");
            std::env::remove_var("INFINITY_CLAUDE_OAUTH");
        }
        assert!(preset.resolve().is_none());

        unsafe {
            std::env::set_var("ANTHROPIC_OAUTH_TOKEN", "sk-ant-oauth01-test");
        }
        let backend = preset.resolve().expect("static token activates preset");
        assert_eq!(backend.preset_id, "claude-oauth");
        assert!(backend.oauth.is_some());
        assert_eq!(backend.models.len(), 3);

        restore(&saved_token, &saved_opt_in);
    }

    #[test]
    fn model_overrides_round_trip() {
        let prefix = "INFINITY_PROVIDER_MODELS_DEV_TEST";
        let var = format!("{prefix}_MODELS");
        unsafe {
            std::env::set_var(&var, "a-model,b-model");
        }
        assert_eq!(
            model_overrides(prefix),
            Some(vec!["a-model".to_owned(), "b-model".to_owned()])
        );
        unsafe {
            std::env::remove_var(&var);
        }
        assert!(model_overrides(prefix).is_none());
    }
}
