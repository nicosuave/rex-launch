//! Branch names derived from prompts.

/// Words that carry no meaning in a branch name.
const FILLER: &[&str] = &[
    "a", "about", "an", "and", "any", "are", "as", "at", "be", "by", "can", "could", "do", "for",
    "from", "hey", "i", "in", "into", "is", "it", "its", "just", "let", "lets", "me", "my", "of",
    "on", "or", "our", "please", "pls", "plz", "should", "so", "some", "that", "the", "these",
    "this", "those", "to", "us", "we", "with", "would", "you",
];

const MAX_WORDS: usize = 4;
const MAX_LEN: usize = 40;

/// A short kebab-case branch name built from the first meaningful words of a
/// prompt, such as `fix-login-redirect` for "can you fix the login redirect".
/// Empty when the prompt has no usable words.
pub fn branch_from_prompt(prompt: &str) -> String {
    let words = prompt
        .split(|c: char| !c.is_ascii_alphanumeric())
        .map(str::to_ascii_lowercase)
        .filter(|w| !w.is_empty() && !FILLER.contains(&w.as_str()))
        // Drop the stray letters left by contractions such as "don't".
        .filter(|w| w.len() > 1 || w.chars().all(|c| c.is_ascii_digit()));

    let mut name = String::new();
    for word in words.take(MAX_WORDS) {
        let sep = usize::from(!name.is_empty());
        if name.len() + sep + word.len() > MAX_LEN {
            if name.is_empty() {
                name.push_str(&word[..MAX_LEN]);
            }
            break;
        }
        if sep == 1 {
            name.push('-');
        }
        name.push_str(&word);
    }
    name
}

/// Makes a typed branch name safe to hand to git: whitespace becomes `-`,
/// characters git rejects are dropped, and leading or trailing separators
/// are trimmed.
pub fn sanitize_branch(input: &str) -> String {
    let mut out = String::new();
    for c in input.chars() {
        let c = if c.is_whitespace() { '-' } else { c };
        if !(c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '/')) {
            continue;
        }
        // git forbids "..", "//" and "--" is just noise.
        if let Some(last) = out.chars().last()
            && last == c
            && matches!(c, '.' | '/' | '-')
        {
            continue;
        }
        out.push(c);
    }
    out.trim_matches(|c| matches!(c, '-' | '.' | '/'))
        .trim_end_matches(".lock")
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drops_filler_words() {
        assert_eq!(
            branch_from_prompt("can you fix the login redirect"),
            "fix-login-redirect"
        );
    }

    #[test]
    fn keeps_at_most_four_words() {
        assert_eq!(
            branch_from_prompt("Add retry logic around the S3 uploader for large files"),
            "add-retry-logic-around"
        );
    }

    #[test]
    fn handles_contractions_and_punctuation() {
        assert_eq!(
            branch_from_prompt("Don't crash on empty CSVs!"),
            "don-crash-empty-csvs"
        );
    }

    #[test]
    fn keeps_numbers() {
        assert_eq!(
            branch_from_prompt("bump to v2 and fix #322"),
            "bump-v2-fix-322"
        );
    }

    #[test]
    fn empty_when_nothing_usable() {
        assert_eq!(branch_from_prompt("??? 🙂"), "");
    }

    #[test]
    fn truncates_a_single_huge_word() {
        let name = branch_from_prompt(&"x".repeat(80));
        assert_eq!(name.len(), MAX_LEN);
    }

    #[test]
    fn sanitizes_typed_names() {
        assert_eq!(
            sanitize_branch(" -me/Fix login..now// "),
            "me/Fix-login.now"
        );
        assert_eq!(sanitize_branch("feature.lock"), "feature");
        assert_eq!(sanitize_branch("a  b"), "a-b");
    }
}
