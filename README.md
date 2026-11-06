# infinity-provider-models-dev

[![crates.io](https://img.shields.io/crates/v/infinity-provider-models-dev.svg)](https://crates.io/crates/infinity-provider-models-dev)
[![docs.rs](https://img.shields.io/docsrs/infinity-provider-models-dev)](https://docs.rs/infinity-provider-models-dev)
[![license](https://img.shields.io/crates/l/infinity-provider-models-dev)](https://github.com/ToyVo/infinity-provider-models-dev#license)

Multi-backend model provider for [Infinity](https://github.com/hydro-project/infinity) coding agents, configured through the same API-key env vars that [pi](https://pi.dev/docs/latest/providers) and [opencode](https://opencode.ai/v2/docs/providers/) understand — the non-Amazon counterpart to `infinity-provider-bedrock`.

Each configured upstream backend contributes its models under `<backend>/<model>` ids (e.g. `groq/openai/gpt-oss-120b`, `anthropic/claude-sonnet-4-6`). OpenAI-compatible backends speak Chat Completions streaming; Anthropic speaks Messages streaming. Both are translated into the `infinity-provider-protocol` types, with Bedrock-style error classification (`Throttled` / `ContextOverflow` / `Transient` / `Fatal`) so the runtime can retry intelligently. Requires Rust 1.85+.

## Install

```bash
cargo install --locked infinity-provider-models-dev
```

Register with the daemon (`~/.infinity/providers.json` — first provider's first model is the global default, order matters):

```json
{
  "models_dev": { "command": ["infinity-provider-models-dev"], "crate_name": "infinity-provider-models-dev" }
}
```

The binary prints its Unix socket path on stdout and serves the provider over it, same as `infinity-provider-bedrock`.

Set at least one API key (see [Configure](#configure)), e.g. `export GROQ_API_KEY=...`, then start the daemon.

## Library usage

```rust
use infinity_provider_models_dev::MultiProvider;
use infinity_provider_protocol::ModelProvider;

let provider = MultiProvider::from_env(); // reads process env, see below
let models = provider.list_models().await?;
```

See [docs.rs](https://docs.rs/infinity-provider-models-dev) for the full API.

## Configure

Export any of the keys below; the corresponding backend activates automatically with a sensible default endpoint and model list. No other setup needed.

| Backend (`<backend>/`) | API-key env vars | Default endpoint |
|---|---|---|
| `openai/` | `OPENAI_API_KEY` | `https://api.openai.com/v1` |
| `anthropic/` | `ANTHROPIC_API_KEY` (+ `_AUTH_TOKEN`, `_OAUTH_TOKEN`) | `https://api.anthropic.com/v1` (native Messages API) |
| `deepseek/` | `DEEPSEEK_API_KEY` | `https://api.deepseek.com/v1` |
| `groq/` | `GROQ_API_KEY` | `https://api.groq.com/openai/v1` |
| `cerebras/` | `CEREBRAS_API_KEY` | `https://api.cerebras.ai/v1` |
| `xai/` | `XAI_API_KEY` | `https://api.x.ai/v1` |
| `mistral/` | `MISTRAL_API_KEY` | `https://api.mistral.ai/v1` |
| `gemini/` | `GEMINI_API_KEY` / `GOOGLE_GENERATIVE_AI_API_KEY` | `.../v1beta/openai/` |
| `together/` | `TOGETHER_API_KEY` | `https://api.together.xyz/v1` |
| `fireworks/` | `FIREWORKS_API_KEY` | `https://api.fireworks.ai/inference/v1` |
| `openrouter/` | `OPENROUTER_API_KEY` | `https://openrouter.ai/api/v1` |
| `nvidia/` | `NVIDIA_API_KEY` | `https://integrate.api.nvidia.com/v1` |
| `moonshot/` | `MOONSHOT_API_KEY` / `KIMI_API_KEY` | `https://api.moonshot.ai/v1` |
| `zai/` | `ZAI_API_KEY` | `https://api.z.ai/api/coding/paas/v4` |
| `minimax/` | `MINIMAX_API_KEY` / `MINIMAX_CN_API_KEY` | `https://api.minimax.io/v1` |
| `qwen/` | `QWEN_TOKEN_PLAN_API_KEY` / `DASHSCOPE_API_KEY` … | DashScope compat endpoint |
| `huggingface/` | `HF_TOKEN` | `https://router.huggingface.co/v1` |
| `vercel-gateway/` | `AI_GATEWAY_API_KEY` | `https://ai-gateway.vercel.sh/v1` |
| `opencode/` | `OPENCODE_API_KEY` | `https://opencode.ai/inference/openai/v1` (full catalog discovered) |
| `opencode-go/` | `OPENCODE_API_KEY` (Go subscription) | `https://opencode.ai/inference/go/openai/v1` (full catalog discovered) |
| `claude-oauth/` | `INFINITY_CLAUDE_OAUTH=1` + subscription login (see below) | `https://api.anthropic.com` (native Messages API, subscription billing) |
| `azure/` | `AZURE_OPENAI_API_KEY` (or `AZURE_API_KEY`) + `AZURE_OPENAI_BASE_URL` or `AZURE_OPENAI_RESOURCE_NAME` | derived `https://{resource}.openai.azure.com/openai/v1` |
| `baseten/` | `BASETEN_API_KEY` + `BASETEN_BASE_URL` | (endpoint required) |
| `ollama/` `lmstudio/` `vllm/` | none — set `INFINITY_LOCAL=1` or `<PREFIX>_BASE_URL` | `127.0.0.1:11434/1234/8000` |
| `custom/` | `INFINITY_OPENAI_API_KEY` + `INFINITY_OPENAI_BASE_URL` + `INFINITY_OPENAI_MODELS` | any OpenAI-compat proxy/gateway |

Pi extras honored: `OPENROUTER_APP_NAME`/`OPENROUTER_APP_URL`, `MOONSHOT`/`KIMI` CN variants, `MINIMAX_CN_API_KEY`, `QWEN_TOKEN_PLAN_CN_API_KEY`, `XIAOMI_*` via `custom/` + base URL.

OpenCode Go sends a stable per-process `x-opencode-session` id for routing and prompt-cache affinity (as their docs request); override it with `OPENCODE_SESSION_ID` if you need a specific value.

### Per-backend overrides

```bash
# Point any backend at a proxy, and/or advertise different models:
export GROQ_BASE_URL=https://proxy.internal/groq
export GROQ_MODELS=llama-3.3-70b-versatile,openai/gpt-oss-120b
# Single-model shorthand also works:
export OPENAI_MODEL=gpt-5.4
# Any OpenAI-compatible gateway not listed above:
export INFINITY_OPENAI_BASE_URL=https://gateway.internal/v1
export INFINITY_OPENAI_API_KEY=...
export INFINITY_OPENAI_MODELS=coder,planner
# Enable local servers (Ollama/LM Studio/vLLM defaults):
export INFINITY_LOCAL=1
```

Unlisted upstream models are also reachable without configuration: `groq/<any-upstream-id>` routes to the `groq` backend and passes the id through (same for every backend, including `claude-oauth/`), so new model releases work before the built-in list is updated.

## Where the model list comes from

Three layers, in order:

1. **Curated catalog** (`src/catalog_generated.rs`, generated — do not edit): model ids, display names, context windows, and output limits sourced from [anomalyco/models.dev](https://github.com/anomalyco/models.dev) (fork revision pinned in the file header), with limits cross-checked against upstream [models.dev](https://models.dev). Regenerate after pulling the fork with the single-file [Rust script](https://rust-lang.github.io/rfcs/3502-cargo-script.html) via [cargo-run](https://crates.io/crates/cargo-run) (`Scripts.toml`):

   ```bash
   cargo install cargo-run
   git clone https://github.com/anomalyco/models.dev ../models.dev
   curl -sL https://models.dev/api.json -o /tmp/upstream-api.json
   cargo script run sync --models-dev ../models.dev --upstream /tmp/upstream-api.json
   ```

   The usual tasks live in `Scripts.toml` too: `cargo script run {check,lint,fmt,fmt-check,test,build}`, or `cargo script run ci` for the full chain (also `cargo script show` to list, `cargo script validate` to check).

   The script fails on missing files and on models the fork marks deprecated/retired, so curation stays honest — and it cross-checks our key env vars against the fork's `provider.toml` files (that's how `AZURE_API_KEY` was found).

2. **Live discovery** (`GET {base}/models`, OpenAI shape): local servers (`ollama/`, `lmstudio/`, `vllm/`) always probe — you control what is pulled, so everything served is relevant. The `custom/` gateway probes when `INFINITY_OPENAI_MODELS` is unset. Any cloud backend opts in with `<PREFIX>_DISCOVER=1` (e.g. `GROQ_DISCOVER=1`); off by default since cloud catalogs are noisy. Probes are timeout-guarded (10s) and never fail startup — on any error the curated list stands alone. Discovered models get conservative 200K/32K limits.

3. **Passthrough**: `invoke_model` routes any `<backend>/<any-id>` to its backend, so even undiscovered models work if you name them.

## Claude subscription (at your own risk)

The `claude-oauth/` backend bills to a Claude Pro/Max subscription instead of API credits, using the same OAuth flow as the community bridges ([pi-claude-bridge](https://github.com/elidickinson/pi-claude-bridge), [anthropic-auth](https://github.com/cortexkit/anthropic-auth)).

> **Disclaimer.** This project is not affiliated with or endorsed by Anthropic. Using a subscription outside Anthropic's official clients may violate their terms and can lead to account restrictions or suspension. **Use it at your own risk.** The backend stays dormant unless explicitly enabled.

Setup (pick one credential source, then opt in):

```bash
# 1a. Sign in through the browser (PKCE flow, tokens in ~/.infinity/claude-oauth.json):
infinity-provider-models-dev login [--mode max|console]
# 1b. ...or reuse an existing Claude Code login (read-only, never modified):
claude auth login
# 1c. ...or pin a static bearer token (no refresh):
export ANTHROPIC_OAUTH_TOKEN=sk-ant-oauth01-...

# 2. Opt in (required unless ANTHROPIC_OAUTH_TOKEN is set):
export INFINITY_CLAUDE_OAUTH=1
```

What it does: reads credentials from `ANTHROPIC_OAUTH_TOKEN`, `~/.infinity/claude-oauth.json`, or Claude Code's `~/.claude/.credentials.json` (`CLAUDE_CONFIG_DIR` aware); refreshes the bearer automatically (persisting to our own file only); retries once after a `401`; and sends subscription requests with `Authorization: Bearer`, the `oauth-2025-04-20` beta, and Claude Code identity headers — no `x-api-key`, mirroring the bridges. Bearer tokens are treated as secrets: they never appear in logs.

Advertised models (`Opus`/`Sonnet 4.6`, `Haiku 4.5` at 200K context) are a conservative default — plan-dependent 1M windows and newer models are reachable via passthrough (`claude-oauth/<any-id>`) or `CLAUDE_OAUTH_MODELS`. `CLAUDE_OAUTH_BASE_URL` overrides the endpoint.

### Caller overrides

Like Bedrock's `additional_model_request_fields`, `additional_params` in the completion request is deep-merged into the outgoing JSON body, and `max_tokens` overrides the per-model default.

## Development

```bash
cargo script run ci   # fmt-check + clippy (deny-by-default pedantic/nursery) + tests
```

`cargo install cargo-run` provides the `cargo script` subcommand; tasks are defined in `Scripts.toml` (`cargo script show` to list). The model catalog regenerates from [anomalyco/models.dev](https://github.com/anomalyco/models.dev) — see [Where the model list comes from](#where-the-model-list-comes-from).

## License

Licensed under the Apache License, Version 2.0. See [`LICENSE`](https://github.com/ToyVo/infinity-provider-models-dev/blob/main/LICENSE) for the full text.
