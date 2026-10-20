//! What the program knows about a model from its name: is it a chat model, can it see
//! images, is it meant for code. The names come from the provider's own model list, so this
//! is a best guess; the model picker can always show every model.

/// A best guess about a model, made from its id.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ModelTraits {
    /// A model that answers chat messages (not speech, embeddings, safety filters, driving...).
    pub chat: bool,
    /// Can look at pictures.
    pub vision: bool,
    pub coding: bool,
    pub reasoning: bool,
    /// Size in billions of parameters when the name tells it (`120b`).
    pub params_b: Option<f32>,
}

/// Pieces of a name that mean "not a chat model".
const NOT_CHAT_PARTS: &[&str] = &[
    "detector",
    "deplot",
    "calibration",
    "riva-translate",
    "kosmos",
    "embed",
    "rerank",
    "retriev",
    "parakeet",
    "canary",
    "speech",
    "voice",
    "noise",
    "guard",
    "safety",
    "moderation",
    "bevformer",
    "sparsedrive",
    "streampetr",
    "cosmos-transfer",
    "whisper",
    "imagen",
    "veo",
    "native-audio",
    "-live",
    "-image",
    "aqa",
    "robotics",
    "computer-use",
    "diffusion",
    "stable-",
    "sdxl",
];

/// Whole words (split at `-`, `.`, `_`) that mean "not a chat model".
const NOT_CHAT_WORDS: &[&str] = &["tts", "asr", "clip", "pose", "kumo", "flux", "bge"];

/// Pieces of a name of a model that can see images.
const VISION_PARTS: &[&str] = &[
    "-vl",
    "vision",
    "omni",
    "multimodal",
    "neva",
    "vila",
    "fuyu",
    "llava",
    "pixtral",
    "paligemma",
    "internvl",
    "minicpm-v",
    "muse-glimmer",
    "llama-4",
    "cosmos-reason",
    "glm-5.3-flash",
    "step-5",
    "deepseek-v4",
    "gemini",
    "gpt-4o",
    "gpt-4.1",
    "gpt-5",
    "claude",
];

const CODING_PARTS: &[&str] = &["coder", "codestral", "starcoder", "codellama", "codegemma", "laguna"];

fn words(name: &str) -> Vec<&str> {
    name.split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|w| !w.is_empty())
        .collect()
}

/// The part of an id after the last `/`: `nvidia/nemotron-3-ultra` -> `nemotron-3-ultra`.
pub fn short_name(id: &str) -> &str {
    id.rsplit('/').next().unwrap_or(id)
}

pub fn traits_of(id: &str) -> ModelTraits {
    let name = short_name(id).to_ascii_lowercase();
    let words = words(&name);
    let has_part = |parts: &[&str]| parts.iter().any(|p| name.contains(p));
    let chat = !has_part(NOT_CHAT_PARTS) && !words.iter().any(|w| NOT_CHAT_WORDS.contains(w));
    let vision = chat && (has_part(VISION_PARTS) || words.contains(&"vl"));
    let coding = has_part(CODING_PARTS) || words.contains(&"code") || words.contains(&"coder");
    let reasoning = name.contains("reason") || name.contains("thinking") || words.contains(&"r1");
    let params_b = words.iter().find_map(|w| {
        let digits = w.strip_suffix('b')?;
        if digits.is_empty() || !digits.chars().all(|c| c.is_ascii_digit()) {
            return None;
        }
        digits.parse::<f32>().ok()
    });
    ModelTraits {
        chat,
        vision,
        coding,
        reasoning,
        params_b,
    }
}

/// True when the model can look at pictures.
pub fn sees_images(id: &str) -> bool {
    traits_of(id).vision
}

/// Short name plus what it is good for, for menus: `nemotron-3.5-super-vl-120b-a12b  [images]`.
pub fn label(id: &str) -> String {
    let t = traits_of(id);
    let mut tags: Vec<&str> = Vec::new();
    if t.vision {
        tags.push("images");
    }
    if t.coding {
        tags.push("code");
    }
    if t.reasoning {
        tags.push("reasoning");
    }
    if !t.chat {
        tags.push("not for chat");
    }
    if tags.is_empty() {
        short_name(id).to_string()
    } else {
        format!("{}  [{}]", short_name(id), tags.join(", "))
    }
}

/// The models to show in a picker: chat models only unless `show_all`, with the model in
/// use always present.
pub fn picker_list(models: &[String], current: &str, show_all: bool) -> Vec<String> {
    let mut out: Vec<String> = models
        .iter()
        .filter(|m| show_all || traits_of(m).chat)
        .cloned()
        .collect();
    if !current.is_empty() && !out.iter().any(|m| m == current) {
        out.insert(0, current.to_string());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A few ids of the NVIDIA list that the free key returns.
    const SEES: &[&str] = &[
        "nvidia/nemotron-3.5-super-vl-120b-a12b",
        "nvidia/nemotron-3-nano-omni-30b-a3b-reasoning",
        "nvidia/cosmos-reason2-8b",
        "nvidia/neva-22b",
        "nvidia/vila",
        "adept/fuyu-8b",
        "meta/llama-3.2-11b-vision-instruct",
        "meta/llama-3.2-90b-vision-instruct",
        "meta/muse-glimmer-30b",
        "microsoft/phi-3-vision-128k-instruct",
        "stepfun-ai/step-5-preview",
        "z-ai/glm-5.3-flash",
        "deepseek-ai/deepseek-v4.1-flash",
        "gemini-2.5-flash",
    ];

    const TEXT_ONLY: &[&str] = &[
        "nvidia/nemotron-3-ultra-550b-a55b",
        "nvidia/nemotron-3-super-120b-a12b",
        "nvidia/nemotron-3.5-lightning-30b-a3b",
        "moonshotai/kimi-k2.6",
        "openai/gpt-oss-20b",
        "z-ai/glm-5.3",
        "google/gemma-4-31b-it",
        "mistralai/mistral-large-2-instruct",
        "poolside/laguna-xs-2.1",
    ];

    const NOT_CHAT: &[&str] = &[
        "nvidia/ai-synthetic-video-detector",
        "google/deplot",
        "nvidia/ising-calibration-1.5-31b",
        "nvidia/riva-translate-4b-instruct",
        "microsoft/kosmos-2",
        "nvidia/nemotron-3-embed-1b",
        "nvidia/magpie-tts-zeroshot",
        "llama-guard-4-12b",
    ];

    #[test]
    fn models_that_see_images_are_recognized() {
        for id in SEES {
            let t = traits_of(id);
            assert!(t.chat && t.vision, "{id}: {t:?}");
        }
        for id in TEXT_ONLY {
            let t = traits_of(id);
            assert!(t.chat && !t.vision, "{id}: {t:?}");
        }
    }

    #[test]
    fn utility_models_are_not_offered_for_chat() {
        for id in NOT_CHAT {
            let t = traits_of(id);
            assert!(!t.chat && !t.vision, "{id}: {t:?}");
        }
    }

    #[test]
    fn size_and_purpose_come_from_the_name() {
        assert_eq!(traits_of("nvidia/nemotron-3.5-super-vl-120b-a12b").params_b, Some(120.0));
        assert_eq!(traits_of("google/gemma-4-31b-it").params_b, Some(31.0));
        assert_eq!(traits_of("nvidia/vila").params_b, None);
        assert!(traits_of("poolside/laguna-xs-2.1").coding);
        assert!(traits_of("mistralai/codestral-22b-instruct-v0.1").coding);
        assert!(traits_of("nvidia/nemotron-3-nano-omni-30b-a3b-reasoning").reasoning);
        assert!(!traits_of("nvidia/nemotron-3-ultra-550b-a55b").reasoning);
    }

    #[test]
    fn menu_labels_show_what_a_model_can_do() {
        assert_eq!(
            label("nvidia/nemotron-3.5-super-vl-120b-a12b"),
            "nemotron-3.5-super-vl-120b-a12b  [images]"
        );
        assert_eq!(label("moonshotai/kimi-k2.6"), "kimi-k2.6");
        assert!(label("google/deplot").contains("not for chat"));
    }

    #[test]
    fn the_picker_hides_utility_models_but_keeps_the_current_one() {
        let all: Vec<String> = ["nvidia/vila", "google/deplot", "moonshotai/kimi-k2.6"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(
            picker_list(&all, "moonshotai/kimi-k2.6", false),
            vec!["nvidia/vila", "moonshotai/kimi-k2.6"]
        );
        assert_eq!(picker_list(&all, "x/own-model", false)[0], "x/own-model");
        assert_eq!(picker_list(&all, "", true).len(), 3);
    }
}
