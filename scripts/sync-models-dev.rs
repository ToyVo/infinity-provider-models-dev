#!/usr/bin/env -S cargo +nightly -Zscript
---cargo
[package]
edition = "2021"

[dependencies]
serde_json = "1"
toml = "0.8"
---
//!
//! Generate `src/catalog_generated.rs` from model catalogs.
//!
//! Sources:
//! - <https://github.com/anomalyco/models.dev> (checkout via `--models-dev`):
//!   canonical ids, display names, live/deprecated status, image modalities.
//! - Upstream `models.dev` `api.json` (via `--upstream`): complete
//!   `limit.context` / `limit.output` / `tool_call` for every model (the fork
//!   keeps limits per-API or in prose, so it is not machine-readable
//!   everywhere).
//!
//! Model *selection* stays editorial here: each preset lists 1-3 flagships,
//! everything else is reachable via the `<backend>/<any-id>` passthrough.
//!
//! Run with a script runner (e.g. `cargo install rust-script`):
//!
//! ```sh
//! git clone https://github.com/anomalyco/models.dev ../models.dev
//! curl -sL https://models.dev/api.json -o /tmp/upstream-api.json
//! rust-script scripts/sync-models-dev.rs \
//!     --models-dev ../models.dev --upstream /tmp/upstream-api.json
//! ```
//!
//! Fails loudly on missing files and deprecated models so curation stays
//! honest. The generated file records both source revisions.

use std::path::{Path, PathBuf};
use std::process::Command;

struct Preset {
    id: &'static str,
    provider: &'static str,
    models: &'static [&'static str],
    suffix: &'static str,
    key_envs: &'static [&'static str],
}

// Keep in sync with `preset.rs`: which fork provider and curated models back
// each preset, plus the key env vars we read (cross-checked against the
// fork's `provider.toml`).
const CATALOG: &[Preset] = &[
    Preset {
        id: "openai",
        provider: "openai",
        models: &["gpt-5.4", "gpt-5-mini"],
        suffix: "",
        key_envs: &["OPENAI_API_KEY"],
    },
    Preset {
        id: "anthropic",
        provider: "anthropic",
        models: &["claude-opus-4-6", "claude-sonnet-4-6", "claude-haiku-4-5"],
        suffix: "",
        key_envs: &[
            "ANTHROPIC_API_KEY",
            "ANTHROPIC_AUTH_TOKEN",
            "ANTHROPIC_OAUTH_TOKEN",
        ],
    },
    Preset {
        id: "claude-oauth",
        provider: "anthropic",
        models: &["claude-opus-4-6", "claude-sonnet-4-6", "claude-haiku-4-5"],
        suffix: " (subscription)",
        key_envs: &[],
    },
    Preset {
        id: "deepseek",
        provider: "deepseek",
        models: &["deepseek-v4-pro", "deepseek-flash"],
        suffix: "",
        key_envs: &["DEEPSEEK_API_KEY"],
    },
    Preset {
        id: "groq",
        provider: "groq",
        models: &["openai/gpt-oss-120b", "qwen/qwen3.8-27b"],
        suffix: "",
        key_envs: &["GROQ_API_KEY"],
    },
    Preset {
        id: "cerebras",
        provider: "cerebras",
        models: &["gpt-oss-120b", "qwen-3.8-27b"],
        suffix: "",
        key_envs: &["CEREBRAS_API_KEY"],
    },
    Preset {
        id: "xai",
        provider: "xai",
        models: &["grok-4.6", "grok-4.7"],
        suffix: "",
        key_envs: &["XAI_API_KEY"],
    },
    Preset {
        id: "mistral",
        provider: "mistral",
        models: &["mistral-large-latest", "codestral-latest"],
        suffix: "",
        key_envs: &["MISTRAL_API_KEY"],
    },
    Preset {
        id: "gemini",
        provider: "google",
        models: &["gemini-2.5-pro", "gemini-2.5-flash"],
        suffix: "",
        key_envs: &[
            "GEMINI_API_KEY",
            "GOOGLE_GENERATIVE_AI_API_KEY",
            "GOOGLE_API_KEY",
        ],
    },
    Preset {
        id: "together",
        provider: "togetherai",
        models: &["moonshotai/Kimi-K3", "Qwen/Qwen3.6-Plus"],
        suffix: "",
        key_envs: &["TOGETHER_API_KEY"],
    },
    Preset {
        id: "fireworks",
        provider: "fireworks-ai",
        models: &[
            "accounts/fireworks/models/kimi-k3",
            "accounts/fireworks/models/deepseek-v4p1-flash",
        ],
        suffix: "",
        key_envs: &["FIREWORKS_API_KEY"],
    },
    Preset {
        id: "openrouter",
        provider: "openrouter",
        models: &[
            "anthropic/claude-sonnet-4.6",
            "openai/gpt-5.4",
            "qwen/qwen3-coder",
        ],
        suffix: "",
        key_envs: &["OPENROUTER_API_KEY"],
    },
    Preset {
        id: "nvidia",
        provider: "nvidia",
        models: &["deepseek-ai/deepseek-v4.1-flash"],
        suffix: "",
        key_envs: &["NVIDIA_API_KEY"],
    },
    Preset {
        id: "moonshot",
        provider: "moonshotai",
        models: &["kimi-k2.7-code", "kimi-k3"],
        suffix: "",
        key_envs: &["MOONSHOT_API_KEY", "KIMI_API_KEY"],
    },
    Preset {
        id: "zai",
        provider: "zai",
        models: &["glm-4.6", "glm-5"],
        suffix: "",
        key_envs: &["ZAI_API_KEY"],
    },
    Preset {
        id: "minimax",
        provider: "minimax",
        models: &["MiniMax-M3", "MiniMax-M3.1-Flash"],
        suffix: "",
        key_envs: &["MINIMAX_API_KEY", "MINIMAX_CN_API_KEY"],
    },
    Preset {
        id: "qwen",
        provider: "alibaba",
        models: &["qwen3-coder-flash", "qwen-max"],
        suffix: "",
        key_envs: &[
            "QWEN_TOKEN_PLAN_API_KEY",
            "QWEN_TOKEN_PLAN_CN_API_KEY",
            "DASHSCOPE_API_KEY",
        ],
    },
    Preset {
        id: "huggingface",
        provider: "huggingface",
        models: &["Qwen/Qwen3-Coder-480B-A35B-Instruct"],
        suffix: "",
        key_envs: &["HF_TOKEN", "HUGGINGFACE_API_KEY"],
    },
    Preset {
        id: "vercel-gateway",
        provider: "vercel",
        models: &["anthropic/claude-sonnet-4.6"],
        suffix: "",
        key_envs: &["AI_GATEWAY_API_KEY"],
    },
    Preset {
        id: "opencode-zen",
        provider: "opencode",
        models: &["claude-sonnet-4-6"],
        suffix: "",
        key_envs: &["OPENCODE_API_KEY"],
    },
    Preset {
        id: "azure",
        provider: "azure",
        models: &["gpt-5.4"],
        suffix: "",
        key_envs: &["AZURE_OPENAI_API_KEY", "AZURE_API_KEY"],
    },
];

const DEAD_STATUSES: &[&str] = &["deprecated", "removed", "retired"];

fn rust_string(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}

/// Format with `_` separators (matches `clippy::unreadable_literal`).
fn rust_int(n: i64) -> String {
    let digits = n.unsigned_abs().to_string();
    let chars: Vec<char> = digits.chars().rev().collect();
    let mut grouped = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, c) in chars.iter().enumerate() {
        if i > 0 && i % 3 == 0 {
            grouped.push('_');
        }
        grouped.push(*c);
    }
    let grouped: String = grouped.chars().rev().collect();
    if n < 0 {
        format!("-{grouped}")
    } else {
        grouped
    }
}

/// Last-resort display name derived from an id.
fn humanize(model_id: &str) -> String {
    let short = model_id.rsplit('/').next().unwrap_or(model_id);
    short
        .split(['-', '_'])
        .filter(|part| !part.is_empty())
        .map(|chunk| {
            let mut chars = chunk.chars();
            match chars.next() {
                Some(first) if first.is_alphabetic() => {
                    first.to_uppercase().collect::<String>() + chars.as_str()
                }
                _ => chunk.to_owned(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// One generated `PresetModel` entry (expanded, `rustfmt`-stable layout).
fn render_entry(mid: &str, name: &str, context: i64, output: &str, images: bool) -> String {
    format!(
        "    PresetModel {{\n        id: {},\n        display_name: {},\n        context_window: {},\n        max_output_tokens: {output},\n        supports_images: {},\n    }},",
        rust_string(mid),
        rust_string(name),
        rust_int(context),
        if images { "true" } else { "false" },
    )
}

fn git_rev(path: &Path) -> String {
    Command::new("git")
        .args([
            "-C",
            &path.to_string_lossy(),
            "rev-parse",
            "--short",
            "HEAD",
        ])
        .output()
        .ok()
        .and_then(|out| {
            if out.status.success() {
                Some(String::from_utf8_lossy(&out.stdout).trim().to_owned())
            } else {
                None
            }
        })
        .unwrap_or_else(|| "unknown".to_owned())
}

fn table<'a>(value: &'a toml::Value, key: &str) -> Option<&'a toml::map::Map<String, toml::Value>> {
    value.get(key)?.as_table()
}

fn usages(value: &toml::Value) -> Vec<String> {
    value
        .get("input")
        .and_then(toml::Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(toml::Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

struct Args {
    models_dev: PathBuf,
    upstream: PathBuf,
    out: PathBuf,
}

fn parse_args() -> Result<Args, String> {
    let mut raw = std::env::args().skip(1);
    let mut models_dev = None;
    let mut upstream = None;
    let mut out = PathBuf::from("src/catalog_generated.rs");
    while let Some(arg) = raw.next() {
        // Runners (e.g. `cargo-run`) may insert a `--` separator before
        // forwarded script arguments.
        if arg == "--" {
            continue;
        }
        match arg.as_str() {
            "--models-dev" => {
                models_dev = raw.next().map(PathBuf::from);
            }
            "--upstream" => {
                upstream = raw.next().map(PathBuf::from);
            }
            "--out" => {
                out = raw
                    .next()
                    .map(PathBuf::from)
                    .ok_or("`--out` needs a value")?;
            }
            "--help" | "-h" => {
                return Err(
                    "usage: sync-models-dev.rs --models-dev PATH --upstream api.json [--out PATH]"
                        .to_owned(),
                );
            }
            other => return Err(format!("unknown argument: {other}")),
        }
    }
    Ok(Args {
        models_dev: models_dev.ok_or("missing required `--models-dev PATH`")?,
        upstream: upstream.ok_or("missing required `--upstream api.json`")?,
        out,
    })
}

fn main() {
    if let Err(e) = run() {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), String> {
    let args = parse_args().map_err(|e| {
        format!("{e}\nusage: sync-models-dev.rs --models-dev PATH --upstream api.json [--out PATH]")
    })?;
    let providers = args.models_dev.join("providers");
    if !providers.is_dir() {
        return Err(format!(
            "{} not found; clone https://github.com/anomalyco/models.dev",
            providers.display()
        ));
    }
    let upstream_text = std::fs::read_to_string(&args.upstream)
        .map_err(|e| format!("read {}: {e}", args.upstream.display()))?;
    let upstream: serde_json::Value = serde_json::from_str(&upstream_text)
        .map_err(|e| format!("parse {}: {e}", args.upstream.display()))?;
    let rev = git_rev(&args.models_dev);

    let mut failures = 0;
    let mut chunks = Vec::new();
    for preset in CATALOG {
        let prov_dir = providers.join(preset.provider);
        if !prov_dir.is_dir() {
            println!(
                "error: [{}] provider dir {} missing",
                preset.id,
                prov_dir.display()
            );
            failures += 1;
            continue;
        }
        match std::fs::read_to_string(prov_dir.join("provider.toml")) {
            Ok(text) => match text.parse::<toml::Value>() {
                Ok(doc) => {
                    let envs: Vec<String> = doc
                        .get("env")
                        .and_then(toml::Value::as_array)
                        .map(|items| {
                            items
                                .iter()
                                .filter_map(toml::Value::as_str)
                                .map(str::to_owned)
                                .collect()
                        })
                        .unwrap_or_default();
                    for ours in preset.key_envs {
                        if !envs.iter().any(|e| e == ours) {
                            println!(
                                "note: [{}] our key {ours} not in fork provider.toml env {envs:?}",
                                preset.id
                            );
                        }
                    }
                    for theirs in &envs {
                        if !preset.key_envs.contains(&theirs.as_str()) {
                            println!(
                                "note: [{}] fork lists env {theirs} we do not read",
                                preset.id
                            );
                        }
                    }
                }
                Err(e) => println!("note: [{}] cannot parse provider.toml: {e}", preset.id),
            },
            Err(_) => println!("note: [{}] no provider.toml", preset.id),
        }
        let up_models = upstream
            .get(preset.provider)
            .and_then(|p| p.get("models"))
            .and_then(serde_json::Value::as_object);
        let mut entries = Vec::new();
        for mid in preset.models {
            let path = prov_dir.join("models").join(format!("{mid}.toml"));
            let text = match std::fs::read_to_string(&path) {
                Ok(text) => text,
                Err(_) => {
                    println!("error: [{}] {mid}: {} not found", preset.id, path.display());
                    failures += 1;
                    continue;
                }
            };
            let doc: toml::Value = match text.parse() {
                Ok(doc) => doc,
                Err(e) => {
                    println!("error: [{}] {mid}: invalid TOML: {e}", preset.id);
                    failures += 1;
                    continue;
                }
            };
            let status = doc
                .get("status")
                .and_then(toml::Value::as_str)
                .unwrap_or("live");
            if DEAD_STATUSES.contains(&status) {
                println!(
                    "error: [{}] {mid}: status={status} — replace this model",
                    preset.id
                );
                failures += 1;
                continue;
            }
            let fork_limit = table(&doc, "limit");
            let fork_ctx = fork_limit
                .and_then(|l| l.get("context"))
                .and_then(toml::Value::as_integer);
            let fork_out = fork_limit
                .and_then(|l| l.get("output"))
                .and_then(toml::Value::as_integer);
            let up_entry = up_models.and_then(|models| models.get(*mid));
            let up_limit = up_entry.and_then(|e| e.get("limit"));
            let context = fork_ctx.or_else(|| {
                up_limit
                    .and_then(|l| l.get("context"))
                    .and_then(serde_json::Value::as_i64)
            });
            let output = fork_out.or_else(|| {
                up_limit
                    .and_then(|l| l.get("output"))
                    .and_then(serde_json::Value::as_i64)
            });
            let Some(context) = context else {
                println!(
                    "error: [{}] {mid}: no limit.context in fork or upstream",
                    preset.id
                );
                failures += 1;
                continue;
            };
            let fork_tools = table(&doc, "capabilities")
                .and_then(|c| c.get("tools"))
                .and_then(|t| t.as_table())
                .and_then(|t| t.get("supported"))
                .and_then(toml::Value::as_bool);
            let up_tools = up_entry
                .and_then(|e| e.get("tool_call"))
                .and_then(serde_json::Value::as_bool);
            if fork_tools != Some(true) && up_tools != Some(true) {
                println!(
                    "warn: [{0}] {mid}: no function calling (fork={fork_tools:?} upstream={up_tools:?})",
                    preset.id
                );
            }
            let mut images: Option<bool> = table(&doc, "modalities").map(|m| {
                usages(&toml::Value::Table(m.clone()))
                    .iter()
                    .any(|item| item.contains("image"))
            });
            if images.is_none() {
                images = up_entry
                    .and_then(|e| e.get("modalities"))
                    .and_then(|m| m.get("input"))
                    .and_then(serde_json::Value::as_array)
                    .map(|items| {
                        items
                            .iter()
                            .filter_map(serde_json::Value::as_str)
                            .any(|item| item == "image" || item.starts_with("image/"))
                    });
            }
            let images = images.unwrap_or(true);
            let mut name = doc
                .get("name")
                .and_then(toml::Value::as_str)
                .map(str::to_owned)
                .or_else(|| {
                    up_entry
                        .and_then(|e| e.get("name"))
                        .and_then(serde_json::Value::as_str)
                        .map(str::to_owned)
                })
                .unwrap_or_else(|| humanize(mid));
            if let Some(stripped) = name.strip_suffix(" (latest)") {
                name = stripped.to_owned();
            }
            name.push_str(preset.suffix);
            let output =
                output.map_or_else(|| "None".to_owned(), |v| format!("Some({})", rust_int(v)));
            entries.push(render_entry(mid, &name, context, &output, images));
        }
        let constant = preset.id.to_uppercase().replace('-', "_");
        if entries.len() == 1 {
            // `rustfmt` keeps single-entry arrays compact.
            let compact = entries[0]
                .replace("\n        ", "\n    ")
                .replace("    PresetModel", "PresetModel")
                .replace("\n    },", "\n}]");
            chunks.push(format!(
                "pub const {constant}: &[PresetModel] = &[{compact};"
            ));
        } else {
            chunks.push(format!(
                "pub const {constant}: &[PresetModel] = &[\n{}\n];",
                entries.join("\n")
            ));
        }
    }

    if failures > 0 {
        println!("{failures} problem(s); not writing output");
        std::process::exit(1);
    }

    let header = format!(
        "//! Model catalog generated from anomalyco/models.dev.\n//! DO NOT EDIT — regenerate with `rust-script scripts/sync-models-dev.rs`.\n//! Fork revision: anomalyco/models.dev@{rev}.\n\nuse crate::preset::PresetModel;\n"
    );
    let contents = header + &chunks.join("\n") + "\n";
    std::fs::write(&args.out, contents)
        .map_err(|e| format!("write {}: {e}", args.out.display()))?;
    println!(
        "wrote {} from anomalyco/models.dev@{rev}",
        args.out.display()
    );
    Ok(())
}
