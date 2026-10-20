//! Deciding whether a message is a job for one agent or for a team: a small, quick guess that
//! never calls a model. A wrong guess costs little: the plan of a team waits for the user's yes,
//! and `/team` or `/solo` in front of a message settles it.

/// Words that ask for something to be built (the start of the word is enough).
const BUILD_STEMS: &[&str] = &[
    "creat", "build", "develop", "implement", "generat", "design", "make", "write", "yarat",
    "hazırla", "düzəlt", "oluştur", "geliştir", "создай", "сделай", "напиши", "разработ",
];
/// Build words that are too short to match by their start.
const BUILD_WORDS: &[&str] = &["qur", "yap"];
/// Things that are built as a whole (these must be the whole word).
const PROJECT_WORDS: &[&str] = &[
    "app", "apps", "application", "applications", "website", "websites", "site", "sites", "webpage",
    "game", "games", "bot", "bots", "api", "apis", "backend", "frontend", "dashboard", "dashboards",
    "program", "programs", "project", "projects", "platform", "platforms", "system", "systems",
    "server", "extension", "plugin", "tool", "tools", "cli",
];
/// The same in languages whose words take endings (the start of the word is enough).
const PROJECT_STEMS: &[&str] = &[
    "tətbiq", "sayt", "oyun", "proqram", "layihə", "sistem", "platforma", "uygulama", "приложен",
    "сайт", "игр", "программ",
];

/// True when the message looks like a job for a team: something is to be built as a whole
/// (an app, a site, a game...), or the message is long or lists many wishes.
pub fn looks_big(text: &str) -> bool {
    let t = text.trim();
    let chars = t.chars().count();
    if chars < 30 {
        return false;
    }
    let lines = t.lines().filter(|l| !l.trim().is_empty()).count();
    if chars >= 350 || lines >= 5 {
        return true;
    }
    let lower = t.to_lowercase();
    let words: Vec<&str> = lower
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .collect();
    let builds = words
        .iter()
        .any(|w| BUILD_WORDS.contains(w) || BUILD_STEMS.iter().any(|s| w.starts_with(s)));
    let whole = words
        .iter()
        .any(|w| PROJECT_WORDS.contains(w) || PROJECT_STEMS.iter().any(|s| w.starts_with(s)));
    builds && whole
}

/// Takes `/team` or `/solo` off the front of a message: the text, and what the user decided
/// (`Some(true)` team, `Some(false)` one agent, `None` not said).
pub fn split_command(text: &str) -> (&str, Option<bool>) {
    let t = text.trim_start();
    for (prefix, team) in [("/team", true), ("/solo", false)] {
        if let Some(rest) = t.strip_prefix(prefix) {
            if rest.is_empty() || rest.starts_with(char::is_whitespace) {
                return (rest.trim(), Some(team));
            }
        }
    }
    (text.trim(), None)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn something_to_build_as_a_whole_goes_to_a_team() {
        for big in [
            "Create a notes app with search and tags, in HTML, CSS and JavaScript",
            "Build a REST API for books with tests and a README",
            "bir todo tətbiqi yarat, axtarış və etiketlər olsun",
            "Сделай сайт для кофейни с меню и формой заказа",
            "Make me a snake game that I can play in the browser",
        ] {
            assert!(looks_big(big), "{big}");
        }
        // Long messages and lists of wishes too.
        assert!(looks_big(&"Add a feature. ".repeat(30)));
        assert!(looks_big("1. login page\n2. logout\n3. profile\n4. settings\n5. tests"));
    }

    #[test]
    fn small_jobs_and_questions_stay_with_one_agent() {
        for small in [
            "Fix the bug in login.py",
            "What does the main function do in this project?",
            "Make the button blue and the header a bit bigger please",
            "Write a function that adds two numbers and test it",
            "bu fayldakı səhvi düzəlt",
            "",
        ] {
            assert!(!looks_big(small), "{small}");
        }
    }

    #[test]
    fn team_and_solo_in_front_of_a_message_settle_it() {
        assert_eq!(split_command("/team build a game"), ("build a game", Some(true)));
        assert_eq!(split_command("  /solo fix it  "), ("fix it", Some(false)));
        assert_eq!(split_command("/teams are fun"), ("/teams are fun", None));
        assert_eq!(split_command("make a game"), ("make a game", None));
        assert_eq!(split_command("/team"), ("", Some(true)));
    }
}
