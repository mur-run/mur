use serde::{Deserialize, Serialize};

/// Top-level style preset descriptor, embedded as YAML in the binary for
/// built-ins, or loaded from `~/.mur/hub/presets/<id>.yaml` for user presets.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct StylePreset {
    pub id: String,
    pub display_name: String,
    pub author: String,
    pub version: u32,
    pub family: PresetFamily,
    pub description: String,
    pub llm_image_gen: LlmImageGen,
    pub expressions: Vec<ExpressionDef>,
    pub renderer: RendererConfig,
}

/// Visual style family — determines the render pipeline and default sizes.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum PresetFamily {
    Chibi,
    Pixel,
    Live2d,
    Polaroid,
}

/// LLM image generation parameters baked into the preset.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct LlmImageGen {
    pub base_prompt: String,
    pub negative_prompt: String,
    /// Resolution string, e.g. "512x512".
    pub size: String,
    pub steps: u32,
    /// img2img mode — required for the polaroid family.
    #[serde(default)]
    pub img2img: bool,
    /// Whether the user must supply a source photo before render.
    #[serde(default)]
    pub requires_source_image: bool,
}

/// One of the 12 canonical expression slots with its prompt modifier.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ExpressionDef {
    pub id: String,
    pub prompt_suffix: String,
}

/// Renderer display configuration baked into the preset.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RendererConfig {
    pub family: PresetFamily,
    pub default_size: SizeConfig,
    pub idle_animation: IdleAnimation,
    /// Min / max blink interval in seconds (two-element array).
    pub blink_interval_s: [f32; 2],
    pub crossfade_ms: u32,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub struct SizeConfig {
    pub w: u32,
    pub h: u32,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum IdleAnimation {
    Breathe,
    None,
}
