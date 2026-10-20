//! Who used how many tokens: the name of the company behind a service, and short number formats
//! for the "tokens in this session" line.

use std::collections::BTreeMap;

/// The company behind a server address or host name (`https://api.groq.com/openai/v1` is `Groq`).
/// An unknown server is named by its host (`openrouter.ai`).
pub fn company_of(url_or_host: &str) -> String {
    let lower = url_or_host.trim().to_ascii_lowercase();
    let rest = lower.split("://").last().unwrap_or(&lower);
    let host = rest.split(['/', '?', '#']).next().unwrap_or(rest);
    let host = host.rsplit('@').next().unwrap_or(host);
    let host = host.split(':').next().unwrap_or(host);
    if host.contains("nvidia") {
        "NVIDIA".to_string()
    } else if host.contains("groq") {
        "Groq".to_string()
    } else if host.contains("googleapis") || host.contains("google") {
        "Gemini".to_string()
    } else if host == "api.openai.com" {
        "OpenAI".to_string()
    } else if host.is_empty() {
        String::new()
    } else if host == "localhost" || host.starts_with("127.") || host == "0.0.0.0" {
        "Local".to_string()
    } else {
        host.strip_prefix("api.").unwrap_or(host).to_string()
    }
}

/// `950`, `1.2k`, `48k`, `3.4M`.
pub fn fmt_tokens(n: u64) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 10_000 {
        format!("{}k", n / 1000)
    } else if n >= 1000 {
        format!("{:.1}k", n as f64 / 1000.0)
    } else {
        n.to_string()
    }
}

/// `12k tokens · Gemini 8.1k · NVIDIA 4.2k` (largest first), or `None` when nothing was used.
pub fn summary(spent: &BTreeMap<String, u64>) -> Option<String> {
    let total: u64 = spent.values().sum();
    if total == 0 {
        return None;
    }
    let mut parts: Vec<(&String, &u64)> = spent.iter().filter(|(_, n)| **n > 0).collect();
    parts.sort_by(|a, b| b.1.cmp(a.1).then(a.0.cmp(b.0)));
    let mut text = format!("{} tokens", fmt_tokens(total));
    for (company, n) in parts {
        text.push_str(&format!(" · {company} {}", fmt_tokens(*n)));
    }
    Some(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn companies_are_named_by_their_servers() {
        for (url, name) in [
            ("https://integrate.api.nvidia.com/v1", "NVIDIA"),
            ("https://api.groq.com/openai/v1", "Groq"),
            ("https://generativelanguage.googleapis.com/v1beta", "Gemini"),
            ("https://api.openai.com/v1", "OpenAI"),
            ("https://openrouter.ai/api/v1", "openrouter.ai"),
            ("https://api.deepseek.com", "deepseek.com"),
            ("http://localhost:11434/v1", "Local"),
            ("api.groq.com", "Groq"),
            ("", ""),
        ] {
            assert_eq!(company_of(url), name, "{url}");
        }
    }

    #[test]
    fn numbers_are_short() {
        assert_eq!(fmt_tokens(950), "950");
        assert_eq!(fmt_tokens(1234), "1.2k");
        assert_eq!(fmt_tokens(48_900), "48k");
        assert_eq!(fmt_tokens(3_400_000), "3.4M");
    }

    #[test]
    fn the_summary_lists_the_biggest_company_first() {
        let mut spent = BTreeMap::new();
        assert_eq!(summary(&spent), None);
        spent.insert("Groq".to_string(), 3_000);
        spent.insert("Gemini".to_string(), 8_100);
        spent.insert("NVIDIA".to_string(), 0);
        assert_eq!(
            summary(&spent).unwrap(),
            "11k tokens · Gemini 8.1k · Groq 3.0k"
        );
    }
}
