//! What is known about the AI services and their models: how good each one is for the work of
//! a coding agent, how fast it is likely to be, and what its free plan allows. These are guesses
//! made from the names; the program corrects them with what it sees (reply times, used-up
//! quotas, models that do not exist), see [`crate::modelstats`].

use crate::config::Provider;
use crate::models;

/// A sentence about a service's free plan, for the lead's instructions and the menus.
pub fn service_note(provider: Provider) -> &'static str {
    match provider {
        Provider::Gemini => {
            "large context and sees images; the free plan allows few requests a day, so keep it for the hard parts"
        }
        Provider::Nvidia => {
            "many models; free credits and about 40 requests a minute for the whole account; some models queue for minutes"
        }
        Provider::Groq => {
            "extremely fast; the free plan limits tokens per minute and requests per day, so give it small tasks"
        }
        Provider::Openai => "a server chosen by the user",
    }
}

/// How good a model is for the work of a coding agent, from 0 to 10.
pub fn quality(provider: Provider, model: &str) -> f32 {
    let name = models::short_name(model).to_ascii_lowercase();
    let has = |s: &str| name.contains(s);
    let known = match provider {
        Provider::Gemini => {
            if has("flash-lite") {
                Some(6.5)
            } else if has("flash") {
                Some(if has("3") { 9.0 } else { 8.0 })
            } else if has("pro") {
                Some(if has("3") { 10.0 } else { 9.5 })
            } else {
                None
            }
        }
        Provider::Nvidia | Provider::Groq => {
            const TABLE: &[(&str, f32)] = &[
                ("nemotron-3-ultra", 9.0),
                ("super-vl", 8.5),
                ("nemotron-3-super", 8.0),
                ("lightning", 7.0),
                ("step-5", 8.0),
                ("glm-5", 8.5),
                ("deepseek-v4", 8.5),
                ("kimi", 8.5),
                ("gpt-oss-120b", 8.0),
                ("gpt-oss-20b", 5.0),
                ("laguna", 7.0),
                ("muse-glimmer", 6.5),
                ("llama-4-maverick", 7.5),
                ("llama-4-scout", 6.5),
                ("llama-3.3-70b", 7.5),
                ("llama-3.2-90b", 6.0),
                ("llama-3.2-11b", 4.5),
                ("llama-3.1-8b", 4.5),
                ("qwen3-32b", 7.0),
            ];
            TABLE.iter().find(|(k, _)| has(k)).map(|(_, q)| *q)
        }
        Provider::Openai => None,
    };
    known.unwrap_or_else(|| match models::traits_of(model).params_b {
        Some(b) if b < 10.0 => 4.5,
        Some(b) if b < 35.0 => 6.0,
        Some(b) if b < 80.0 => 7.0,
        Some(_) => 8.0,
        None => 6.0,
    })
}

/// How fast a model is likely to answer, as a bonus (or a penalty) for a model that has not been
/// timed yet. The numbers come from timing the free plans: on NVIDIA's free endpoint some models
/// answer in a second and others queue for two minutes.
pub fn speed_prior(provider: Provider, model: &str) -> f32 {
    let name = models::short_name(model).to_ascii_lowercase();
    let has = |s: &str| name.contains(s);
    match provider {
        Provider::Groq => 1.5,
        Provider::Gemini => 0.0,
        Provider::Nvidia => {
            const TABLE: &[(&str, f32)] = &[
                ("glm-5", -3.0),
                ("deepseek-v4", -3.0),
                ("gemma-4", -3.0),
                ("kimi", -2.0),
                ("nemotron-3-ultra", -1.5),
                ("llama-3.2-90b", -1.5),
                ("nemotron-3-super", -0.5),
                ("super-vl", 1.0),
                ("lightning", 0.5),
                ("laguna", 0.5),
            ];
            TABLE.iter().find(|(k, _)| has(k)).map_or(0.0, |(_, v)| *v)
        }
        Provider::Openai => 0.0,
    }
}

/// The biggest request (in tokens) that is worth sending: the free plan of some services
/// refuses a request that is bigger than its per-minute token limit.
pub fn max_prompt_tokens(provider: Provider) -> Option<u32> {
    match provider {
        Provider::Groq => Some(8_000),
        _ => None,
    }
}

/// One line about a model for the lead: who it is and what it is good for.
pub fn blurb(provider: Provider, model: &str) -> String {
    let t = models::traits_of(model);
    let mut s = format!(
        "{} ({}), quality {:.0} of 10",
        models::short_name(model),
        provider.label(),
        quality(provider, model)
    );
    if let Some(b) = t.params_b {
        s.push_str(&format!(", {b:.0}B parameters"));
    }
    if t.vision {
        s.push_str(", sees images");
    }
    if t.coding {
        s.push_str(", made for code");
    }
    if t.reasoning {
        s.push_str(", reasoning model");
    }
    if let Some(secs) = crate::modelstats::typical_secs(model) {
        s.push_str(&format!(", answers in about {secs:.0} s"));
    }
    s.push_str(&format!("; {}", service_note(provider)));
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stronger_models_score_higher() {
        assert!(quality(Provider::Gemini, "gemini-2.5-pro") > quality(Provider::Gemini, "gemini-2.5-flash"));
        assert!(quality(Provider::Gemini, "gemini-2.5-flash") > quality(Provider::Gemini, "gemini-2.5-flash-lite"));
        assert!(
            quality(Provider::Nvidia, "nvidia/nemotron-3-ultra-550b-a55b")
                > quality(Provider::Nvidia, "nvidia/nemotron-3.5-lightning-30b-a3b")
        );
        // The small model of the same family is worse, on either service.
        assert!(quality(Provider::Groq, "openai/gpt-oss-120b") > quality(Provider::Groq, "openai/gpt-oss-20b"));
        // An unknown name falls back on its size.
        assert!(quality(Provider::Openai, "acme/big-120b") > quality(Provider::Openai, "acme/tiny-3b"));
    }

    #[test]
    fn models_that_queue_on_the_free_plan_are_expected_to_be_slow() {
        assert!(speed_prior(Provider::Nvidia, "z-ai/glm-5.3") < 0.0);
        assert!(speed_prior(Provider::Nvidia, "nvidia/nemotron-3.5-super-vl-120b-a12b") > 0.0);
        assert!(speed_prior(Provider::Groq, "llama-3.3-70b-versatile") > speed_prior(Provider::Nvidia, "x/unknown"));
    }

    #[test]
    fn the_lead_gets_a_line_about_every_model() {
        let line = blurb(Provider::Nvidia, "nvidia/nemotron-3.5-super-vl-120b-a12b");
        assert!(line.contains("NVIDIA") && line.contains("sees images") && line.contains("120B"), "{line}");
        assert!(blurb(Provider::Groq, "llama-3.3-70b-versatile").contains("small tasks"));
        assert_eq!(max_prompt_tokens(Provider::Groq), Some(8_000));
        assert_eq!(max_prompt_tokens(Provider::Gemini), None);
    }
}
