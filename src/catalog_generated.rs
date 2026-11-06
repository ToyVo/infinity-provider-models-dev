//! Model catalog generated from anomalyco/models.dev.
//! DO NOT EDIT — regenerate with `rust-script scripts/sync-models-dev.rs`.
//! Source revision: anomalyco/models.dev@a276c37.

use crate::preset::PresetModel;
pub const OPENAI: &[PresetModel] = &[
    PresetModel {
        id: "gpt-5.4",
        display_name: "GPT-5.4",
        context_window: 1_050_000,
        max_output_tokens: Some(128_000),
        supports_images: true,
    },
    PresetModel {
        id: "gpt-5-mini",
        display_name: "GPT-5 Mini",
        context_window: 400_000,
        max_output_tokens: Some(128_000),
        supports_images: true,
    },
];
pub const ANTHROPIC: &[PresetModel] = &[
    PresetModel {
        id: "claude-opus-4-6",
        display_name: "Claude Opus 4.6",
        context_window: 1_000_000,
        max_output_tokens: Some(128_000),
        supports_images: true,
    },
    PresetModel {
        id: "claude-sonnet-4-6",
        display_name: "Claude Sonnet 4.6",
        context_window: 1_000_000,
        max_output_tokens: Some(128_000),
        supports_images: true,
    },
    PresetModel {
        id: "claude-haiku-4-5",
        display_name: "Claude Haiku 4.5",
        context_window: 200_000,
        max_output_tokens: Some(64_000),
        supports_images: true,
    },
];
pub const CLAUDE_OAUTH: &[PresetModel] = &[
    PresetModel {
        id: "claude-opus-4-6",
        display_name: "Claude Opus 4.6 (subscription)",
        context_window: 1_000_000,
        max_output_tokens: Some(128_000),
        supports_images: true,
    },
    PresetModel {
        id: "claude-sonnet-4-6",
        display_name: "Claude Sonnet 4.6 (subscription)",
        context_window: 1_000_000,
        max_output_tokens: Some(128_000),
        supports_images: true,
    },
    PresetModel {
        id: "claude-haiku-4-5",
        display_name: "Claude Haiku 4.5 (subscription)",
        context_window: 200_000,
        max_output_tokens: Some(64_000),
        supports_images: true,
    },
];
pub const DEEPSEEK: &[PresetModel] = &[
    PresetModel {
        id: "deepseek-v4-pro",
        display_name: "DeepSeek V4 Pro",
        context_window: 1_048_576,
        max_output_tokens: Some(393_216),
        supports_images: false,
    },
    PresetModel {
        id: "deepseek-flash",
        display_name: "DeepSeek V4.1 Flash 1017",
        context_window: 1_048_576,
        max_output_tokens: Some(393_216),
        supports_images: true,
    },
];
pub const GROQ: &[PresetModel] = &[
    PresetModel {
        id: "openai/gpt-oss-120b",
        display_name: "GPT OSS 120B",
        context_window: 131_072,
        max_output_tokens: Some(65_536),
        supports_images: false,
    },
    PresetModel {
        id: "qwen/qwen3.8-27b",
        display_name: "Qwen3.8 27B",
        context_window: 131_072,
        max_output_tokens: Some(16_384),
        supports_images: true,
    },
];
pub const CEREBRAS: &[PresetModel] = &[
    PresetModel {
        id: "gpt-oss-120b",
        display_name: "GPT OSS 120B",
        context_window: 131_072,
        max_output_tokens: Some(40_960),
        supports_images: false,
    },
    PresetModel {
        id: "qwen-3.8-27b",
        display_name: "Qwen3.8 27B",
        context_window: 131_072,
        max_output_tokens: Some(40_960),
        supports_images: true,
    },
];
pub const XAI: &[PresetModel] = &[
    PresetModel {
        id: "grok-4.6",
        display_name: "Grok 4.6",
        context_window: 500_000,
        max_output_tokens: Some(500_000),
        supports_images: true,
    },
    PresetModel {
        id: "grok-4.7",
        display_name: "Grok 4.7",
        context_window: 500_000,
        max_output_tokens: Some(500_000),
        supports_images: true,
    },
];
pub const MISTRAL: &[PresetModel] = &[
    PresetModel {
        id: "mistral-large-latest",
        display_name: "Mistral Large",
        context_window: 262_144,
        max_output_tokens: Some(262_144),
        supports_images: true,
    },
    PresetModel {
        id: "codestral-latest",
        display_name: "Codestral",
        context_window: 256_000,
        max_output_tokens: Some(4_096),
        supports_images: false,
    },
];
pub const GEMINI: &[PresetModel] = &[
    PresetModel {
        id: "gemini-2.5-pro",
        display_name: "Gemini 2.5 Pro",
        context_window: 1_048_576,
        max_output_tokens: Some(65_536),
        supports_images: true,
    },
    PresetModel {
        id: "gemini-2.5-flash",
        display_name: "Gemini 2.5 Flash",
        context_window: 1_048_576,
        max_output_tokens: Some(65_536),
        supports_images: true,
    },
];
pub const TOGETHER: &[PresetModel] = &[
    PresetModel {
        id: "moonshotai/Kimi-K3",
        display_name: "Kimi K3",
        context_window: 1_048_576,
        max_output_tokens: Some(131_072),
        supports_images: true,
    },
    PresetModel {
        id: "Qwen/Qwen3.6-Plus",
        display_name: "Qwen3.6 Plus",
        context_window: 1_000_000,
        max_output_tokens: Some(65_536),
        supports_images: false,
    },
];
pub const FIREWORKS: &[PresetModel] = &[
    PresetModel {
        id: "accounts/fireworks/models/kimi-k3",
        display_name: "Kimi K3",
        context_window: 1_048_576,
        max_output_tokens: Some(131_072),
        supports_images: true,
    },
    PresetModel {
        id: "accounts/fireworks/models/deepseek-v4p1-flash",
        display_name: "DeepSeek V4.1 Flash",
        context_window: 1_000_000,
        max_output_tokens: Some(384_000),
        supports_images: true,
    },
];
pub const OPENROUTER: &[PresetModel] = &[
    PresetModel {
        id: "anthropic/claude-sonnet-4.6",
        display_name: "Claude Sonnet 4.6",
        context_window: 1_000_000,
        max_output_tokens: Some(128_000),
        supports_images: true,
    },
    PresetModel {
        id: "openai/gpt-5.4",
        display_name: "GPT-5.4",
        context_window: 1_050_000,
        max_output_tokens: Some(128_000),
        supports_images: true,
    },
    PresetModel {
        id: "qwen/qwen3-coder",
        display_name: "Qwen3 Coder 480B A35B",
        context_window: 262_144,
        max_output_tokens: Some(65_536),
        supports_images: false,
    },
];
pub const NVIDIA: &[PresetModel] = &[PresetModel {
    id: "deepseek-ai/deepseek-v4.1-flash",
    display_name: "DeepSeek V4.1 Flash",
    context_window: 1_000_000,
    max_output_tokens: Some(384_000),
    supports_images: true,
}];
pub const MOONSHOT: &[PresetModel] = &[
    PresetModel {
        id: "kimi-k2.7-code",
        display_name: "Kimi K2.7 Code",
        context_window: 262_144,
        max_output_tokens: Some(262_144),
        supports_images: true,
    },
    PresetModel {
        id: "kimi-k3",
        display_name: "Kimi K3",
        context_window: 1_048_576,
        max_output_tokens: Some(1_048_576),
        supports_images: true,
    },
];
pub const ZAI: &[PresetModel] = &[
    PresetModel {
        id: "glm-4.6",
        display_name: "GLM-4.6",
        context_window: 204_800,
        max_output_tokens: Some(131_072),
        supports_images: false,
    },
    PresetModel {
        id: "glm-5",
        display_name: "GLM-5",
        context_window: 204_800,
        max_output_tokens: Some(131_072),
        supports_images: false,
    },
];
pub const MINIMAX: &[PresetModel] = &[
    PresetModel {
        id: "MiniMax-M3",
        display_name: "MiniMax-M3",
        context_window: 1_000_000,
        max_output_tokens: Some(524_288),
        supports_images: true,
    },
    PresetModel {
        id: "MiniMax-M3.1-Flash",
        display_name: "MiniMax-M3.1-Flash",
        context_window: 1_000_000,
        max_output_tokens: Some(524_288),
        supports_images: true,
    },
];
pub const QWEN: &[PresetModel] = &[
    PresetModel {
        id: "qwen3-coder-flash",
        display_name: "Qwen3 Coder Flash",
        context_window: 1_000_000,
        max_output_tokens: Some(65_536),
        supports_images: false,
    },
    PresetModel {
        id: "qwen-max",
        display_name: "Qwen Max",
        context_window: 32_768,
        max_output_tokens: Some(8_192),
        supports_images: false,
    },
];
pub const HUGGINGFACE: &[PresetModel] = &[PresetModel {
    id: "Qwen/Qwen3-Coder-480B-A35B-Instruct",
    display_name: "Qwen3-Coder-480B-A35B-Instruct",
    context_window: 262_144,
    max_output_tokens: Some(65_536),
    supports_images: false,
}];
pub const VERCEL_GATEWAY: &[PresetModel] = &[PresetModel {
    id: "anthropic/claude-sonnet-4.6",
    display_name: "Claude Sonnet 4.6",
    context_window: 1_000_000,
    max_output_tokens: Some(128_000),
    supports_images: true,
}];
pub const OPENCODE: &[PresetModel] = &[
    PresetModel {
        id: "kimi-k2.7-code",
        display_name: "Kimi K2.7 Code",
        context_window: 262_144,
        max_output_tokens: Some(262_144),
        supports_images: true,
    },
    PresetModel {
        id: "claude-sonnet-5",
        display_name: "Claude Sonnet 5",
        context_window: 1_000_000,
        max_output_tokens: Some(128_000),
        supports_images: true,
    },
];
pub const OPENCODE_GO: &[PresetModel] = &[
    PresetModel {
        id: "kimi-k3",
        display_name: "Kimi K3",
        context_window: 1_048_576,
        max_output_tokens: Some(131_072),
        supports_images: true,
    },
    PresetModel {
        id: "deepseek-v4-pro",
        display_name: "DeepSeek V4 Pro",
        context_window: 1_000_000,
        max_output_tokens: Some(384_000),
        supports_images: false,
    },
];
pub const AZURE: &[PresetModel] = &[PresetModel {
    id: "gpt-5.4",
    display_name: "GPT-5.4",
    context_window: 1_050_000,
    max_output_tokens: Some(128_000),
    supports_images: true,
}];
