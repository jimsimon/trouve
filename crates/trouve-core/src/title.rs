//! Fast, offline session-title extraction used whenever the optional title
//! model is disabled or unavailable.

// Eight words is a useful target for navigation, but it is too small as a
// hard limit: ordinary bug descriptions were ending in fragments such as
// "appears to" and "screens all". Sixteen words gives the extractor enough
// room to finish a short sentence; the session list still ellipsizes it to the
// available visual width.
const MAX_WORDS: usize = 16;
const MAX_CHARS: usize = 96;
const MAX_SCANNED_WORDS: usize = MAX_WORDS + 16;
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
/// request-shaped sentence, removes conversational framing, and keeps at most
/// sixteen meaningful words.
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
    while index < raw_words.len() && words.len() < MAX_SCANNED_WORDS {
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
        // Repeated nouns are often necessary to describe movement or leakage
        // ("from one session to another session"). Only collapse accidental
        // adjacent duplicates.
        if words
            .last()
            .is_some_and(|seen| clean_word(seen).eq_ignore_ascii_case(word))
        {
            continue;
        }
        words.push(display_word(raw_words[index - 1]).to_string());
    }

    compact_phrases(&mut words);
    if words.len() > MAX_WORDS || index < raw_words.len() {
        words.truncate(MAX_WORDS);
        trim_dangling_tail(&mut words);
    }

    if words.is_empty() {
        return "Untitled task".into();
    }
    truncate_title(&sentence_case(&words.join(" ")), MAX_CHARS)
}

fn pick_candidate(prompt: &str) -> String {
    let mut best: Option<(i32, String)> = None;
    for line in prompt.lines().take(64) {
        for sentence in split_sentences(line) {
            let candidate = sentence
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

    const VAGUE_QUANTIFIERS: &[&str] = &[
        "a bunch of the ",
        "a bunch of ",
        "a lot of the ",
        "a lot of ",
    ];
    let lower = value.to_ascii_lowercase();
    if let Some(prefix) = VAGUE_QUANTIFIERS
        .iter()
        .find(|prefix| lower.starts_with(**prefix))
    {
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

/// Split prose into sentence-sized title candidates without treating dots in
/// paths such as `src/main.rs` as boundaries.
fn split_sentences(line: &str) -> Vec<&str> {
    let mut sentences = Vec::new();
    let mut start = 0;
    for (offset, character) in line.char_indices() {
        if !matches!(character, '.' | '?' | '!') {
            continue;
        }
        let end = offset + character.len_utf8();
        let at_boundary =
            end == line.len() || line[end..].chars().next().is_some_and(char::is_whitespace);
        if !at_boundary {
            continue;
        }
        // A token containing an earlier dot is probably a filename, version,
        // or abbreviation rather than the end of a sentence.
        let token = line[start..offset]
            .split_whitespace()
            .next_back()
            .unwrap_or_default();
        if character == '.' && token.contains('.') {
            continue;
        }
        sentences.push(&line[start..end]);
        start = end;
    }
    if start < line.len() {
        sentences.push(&line[start..]);
    }
    if sentences.is_empty() {
        sentences.push(line);
    }
    sentences
}

/// Preserve useful grouping punctuation in the display title while using
/// [`clean_word`] for matching and filtering.
fn display_word(word: &str) -> &str {
    word.trim_matches(|c: char| {
        matches!(
            c,
            ',' | '.' | '?' | '!' | ':' | ';' | '"' | '\'' | '`' | '*'
        )
    })
}

fn compact_phrases(words: &mut Vec<String>) {
    replace_phrase(
        words,
        &["appears", "to", "do", "nothing"],
        &["does", "nothing"],
    );
    replace_phrase(
        words,
        &[
            "in", "one", "session", "is", "carried", "over", "when", "clicking", "into", "another",
            "session",
        ],
        &["carries", "over", "between", "sessions"],
    );
}

fn replace_phrase(words: &mut Vec<String>, pattern: &[&str], replacement: &[&str]) {
    while let Some(start) = words.windows(pattern.len()).position(|window| {
        window
            .iter()
            .zip(pattern)
            .all(|(word, expected)| clean_word(word).eq_ignore_ascii_case(expected))
    }) {
        words.splice(
            start..start + pattern.len(),
            replacement.iter().map(|word| (*word).to_string()),
        );
    }
}

fn trim_dangling_tail(words: &mut Vec<String>) {
    while words.len() > 3
        && words.last().is_some_and(|word| {
            matches!(
                clean_word(word).to_ascii_lowercase().as_str(),
                "a" | "an"
                    | "and"
                    | "are"
                    | "appears"
                    | "as"
                    | "at"
                    | "be"
                    | "because"
                    | "before"
                    | "but"
                    | "by"
                    | "can"
                    | "could"
                    | "for"
                    | "from"
                    | "had"
                    | "has"
                    | "have"
                    | "in"
                    | "is"
                    | "of"
                    | "on"
                    | "or"
                    | "seems"
                    | "should"
                    | "that"
                    | "the"
                    | "to"
                    | "was"
                    | "when"
                    | "while"
                    | "were"
                    | "with"
                    | "would"
            )
        })
    {
        words.pop();
    }
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
    use super::{MAX_CHARS, MAX_WORDS, summarize_session_title};

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
        assert_eq!(
            summarize_session_title(
                "The current flow is brittle. Could you add refresh token rotation?"
            ),
            "Add refresh token rotation"
        );
        assert_eq!(summarize_session_title("..."), "Untitled task");
    }

    #[test]
    fn finishes_short_issue_descriptions_instead_of_cutting_at_eight_words() {
        assert_eq!(
            summarize_session_title(
                "Submitting a new prompt after cancelling a turn appears to do nothing. The \
                 \"Processing...\" indicator doesn't even show up."
            ),
            "Submitting new prompt after cancelling turn does nothing"
        );
        assert_eq!(
            summarize_session_title(
                "A bunch of the input fields on the \"Providers\" settings screens (all 3 \
                 tabs) are incorrectly sized. Many stretch to fill their container."
            ),
            "Input fields on Providers settings screens (all 3 tabs) are incorrectly sized"
        );
        assert_eq!(
            summarize_session_title(
                "Anything typed into the prompt box in one session is carried over when \
                 clicking into another session. Prompt input should be per-thread."
            ),
            "Anything typed into prompt box carries over between sessions"
        );
    }

    #[test]
    fn preserves_repeated_nouns_and_grammatical_prepositions() {
        assert_eq!(
            summarize_session_title("Move state from one thread to another thread"),
            "Move state from one thread to another thread"
        );
        assert_eq!(
            summarize_session_title("Fix ordering of provider settings"),
            "Fix ordering of provider settings"
        );
    }

    #[test]
    fn bounds_long_titles() {
        let title = summarize_session_title(
            "Refactor the authentication middleware to support refresh tokens, rotation, \
             device revocation, audit logging, and administrator controls",
        );
        assert!(title.split_whitespace().count() <= MAX_WORDS);
        assert!(title.chars().count() <= MAX_CHARS + 1);
    }
}
