//! Fast, offline session-title extraction for clients that create a session
//! and submit its first prompt together.

const MAX_WORDS: usize = 8;
const REQUEST_MARKERS: &[&str] = &[
    "is there a way ",
    "what is the best way to ",
    "could you ",
    "could we ",
    "could the ",
    "would you ",
    "would we ",
    "can you ",
    "can we ",
    "can the ",
    "please ",
    "i need ",
    "i want ",
    "i'd like ",
    "how can ",
    "how do ",
    "how to ",
];

/// Summarize an initial prompt into a concise navigation title without a
/// provider call. The output is rule-based and deterministic: it selects the
/// request-shaped line, removes conversational framing, and keeps at most
/// eight meaningful words.
pub fn summarize_session_title(prompt: &str) -> String {
    let candidate = pick_candidate(prompt);
    let request = strip_request_framing(&candidate);
    let lower = request.to_ascii_lowercase();
    let contrast = [" instead of ", " rather than ", " as opposed to "]
        .iter()
        .filter_map(|marker| lower.find(marker))
        .min();
    let request = contrast
        .filter(|cut| request[..*cut].split_whitespace().count() >= 3)
        .map(|cut| &request[..cut])
        .unwrap_or(&request);

    let raw_words: Vec<&str> = request.split_whitespace().collect();
    let mut words: Vec<String> = Vec::new();
    let mut index = 0;
    while index < raw_words.len() && words.len() < MAX_WORDS {
        let word = clean_word(raw_words[index]);
        let lower = word.to_ascii_lowercase();
        if lower == "based" && next_word_is(&raw_words, index, "on") {
            words.push("from".into());
            index += 2;
            continue;
        }
        if lower == "without"
            && next_word_is(&raw_words, index, "relying")
            && raw_words
                .get(index + 2)
                .is_some_and(|word| clean_word(word).eq_ignore_ascii_case("on"))
        {
            words.push("without".into());
            index += 3;
            continue;
        }
        index += 1;
        if word.is_empty() || is_filler(&lower) {
            continue;
        }
        if words.iter().any(|seen| seen.eq_ignore_ascii_case(word)) {
            continue;
        }
        words.push(word.to_string());
    }

    if words.is_empty() {
        return "Untitled task".into();
    }
    truncate_title(&sentence_case(&words.join(" ")), 60)
}

fn pick_candidate(prompt: &str) -> String {
    let mut best: Option<(i32, String)> = None;
    for line in prompt.lines().take(64) {
        let candidate = line
            .trim()
            .trim_start_matches(['#', '-', '*', '>', '`'])
            .trim();
        if candidate.is_empty() || (candidate.starts_with('<') && candidate.ends_with('>')) {
            continue;
        }
        let lower = candidate.to_ascii_lowercase();
        let first = candidate
            .split_whitespace()
            .next()
            .map(clean_word)
            .unwrap_or_default()
            .to_ascii_lowercase();
        let mut score = 0;
        if REQUEST_MARKERS.iter().any(|marker| lower.contains(marker)) {
            score += 8;
        }
        if is_action(&first) {
            score += 4;
        }
        if candidate
            .split_whitespace()
            .any(|word| is_action(&clean_word(word).to_ascii_lowercase()))
        {
            score += 2;
        }
        if candidate.ends_with('?') {
            score += 1;
        }
        if candidate.split_whitespace().count() <= 2 || candidate.ends_with(':') {
            score -= 2;
        }
        if best
            .as_ref()
            .is_none_or(|(best_score, _)| score > *best_score)
        {
            best = Some((score, candidate.to_string()));
        }
    }
    best.map(|(_, candidate)| candidate)
        .unwrap_or_else(|| "Untitled task".into())
}

fn strip_request_framing(candidate: &str) -> String {
    let lower = candidate.to_ascii_lowercase();
    let start = REQUEST_MARKERS
        .iter()
        .filter_map(|marker| lower.find(marker))
        .min()
        .unwrap_or(0);
    let mut value = candidate[start..].trim();

    const PREFIXES: &[&str] = &[
        "is there a way that ",
        "is there a way ",
        "what is the best way to ",
        "could you please ",
        "would you please ",
        "can you please ",
        "i would like you to ",
        "i'd like you to ",
        "i want you to ",
        "i need you to ",
        "i would like to ",
        "i'd like to ",
        "i want to ",
        "i need to ",
        "how can we ",
        "how can i ",
        "how do we ",
        "how do i ",
        "how to ",
        "could you ",
        "could we ",
        "would you ",
        "would we ",
        "can you ",
        "can we ",
        "we could ",
        "we should ",
        "we need to ",
        "please ",
    ];
    for _ in 0..3 {
        let lower = value.to_ascii_lowercase();
        let Some(prefix) = PREFIXES.iter().find(|prefix| lower.starts_with(**prefix)) else {
            break;
        };
        value = value[prefix.len()..].trim();
    }

    let lower = value.to_ascii_lowercase();
    if ["can ", "could ", "would ", "should "]
        .iter()
        .any(|modal| lower.starts_with(modal))
        && let Some(action) = find_action(value)
    {
        value = action;
    }
    value.to_string()
}

fn find_action(value: &str) -> Option<&str> {
    let mut offset = 0;
    for word in value.split_whitespace().take(7) {
        let start = value[offset..].find(word)? + offset;
        if is_action(&clean_word(word).to_ascii_lowercase()) {
            return Some(&value[start..]);
        }
        offset = start + word.len();
    }
    None
}

fn next_word_is(words: &[&str], index: usize, expected: &str) -> bool {
    words
        .get(index + 1)
        .is_some_and(|word| clean_word(word).eq_ignore_ascii_case(expected))
}

fn clean_word(word: &str) -> &str {
    word.trim_matches(|c: char| {
        matches!(
            c,
            ',' | '.' | '?' | '!' | ':' | ';' | '"' | '\'' | '`' | '*' | '(' | ')' | '[' | ']'
        )
    })
}

fn is_filler(word: &str) -> bool {
    matches!(
        word,
        "a" | "an"
            | "the"
            | "i"
            | "we"
            | "you"
            | "my"
            | "our"
            | "your"
            | "this"
            | "that"
            | "there"
            | "of"
            | "please"
            | "just"
            | "really"
            | "actually"
            | "simply"
    )
}

fn is_action(word: &str) -> bool {
    matches!(
        word,
        "add"
            | "analyze"
            | "build"
            | "change"
            | "configure"
            | "create"
            | "debug"
            | "design"
            | "diagnose"
            | "document"
            | "enable"
            | "explain"
            | "find"
            | "fix"
            | "generate"
            | "implement"
            | "improve"
            | "investigate"
            | "make"
            | "migrate"
            | "optimize"
            | "refactor"
            | "remove"
            | "rename"
            | "replace"
            | "resolve"
            | "review"
            | "simplify"
            | "summarize"
            | "support"
            | "test"
            | "update"
            | "use"
            | "write"
    )
}

fn sentence_case(value: &str) -> String {
    let mut chars = value.chars();
    let mut result = chars
        .next()
        .map(|first| first.to_uppercase().collect::<String>())
        .unwrap_or_default();
    result.push_str(chars.as_str());
    result
}

fn truncate_title(title: &str, max_chars: usize) -> String {
    let Some((end, _)) = title.char_indices().nth(max_chars) else {
        return title.to_string();
    };
    let head = &title[..end];
    let head = head
        .rfind(char::is_whitespace)
        .filter(|cut| *cut > max_chars / 2)
        .map(|cut| &head[..cut])
        .unwrap_or(head)
        .trim_end();
    format!("{head}…")
}

#[cfg(test)]
mod tests {
    use super::{MAX_WORDS, summarize_session_title};

    #[test]
    fn extracts_request_instead_of_copying_prompt() {
        assert_eq!(
            summarize_session_title(
                "When initially naming a new session, can the app create an intelligent \
                 summarized title based on the prompt instead of just using the prompt as-is?"
            ),
            "Create intelligent summarized title from prompt"
        );
        assert_eq!(
            summarize_session_title(
                "Is there a way we could generate the titles without relying on a paid provider?"
            ),
            "Generate titles without paid provider"
        );
    }

    #[test]
    fn prefers_request_lines_and_preserves_technical_tokens() {
        assert_eq!(
            summarize_session_title(
                "# Authentication background\nThe current flow is brittle.\n\
                 Could you add refresh token rotation?"
            ),
            "Add refresh token rotation"
        );
        assert_eq!(
            summarize_session_title("Fix src/main.rs compilation on Windows"),
            "Fix src/main.rs compilation on Windows"
        );
        assert_eq!(summarize_session_title("..."), "Untitled task");
    }

    #[test]
    fn bounds_long_titles() {
        let title = summarize_session_title(
            "Refactor the authentication middleware to support refresh tokens, rotation, \
             device revocation, audit logging, and administrator controls",
        );
        assert!(title.split_whitespace().count() <= MAX_WORDS);
        assert!(title.chars().count() <= 61);
    }
}
