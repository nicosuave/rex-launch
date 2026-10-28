//! Subsequence matching for the pickers.

/// Scores `candidate` against `query`, or `None` when the query's characters
/// don't all appear in order. Consecutive matches and matches at the start of
/// a word score higher.
pub fn score(query: &str, candidate: &str) -> Option<i32> {
    let query: Vec<char> = query
        .chars()
        .filter(|c| !c.is_whitespace())
        .flat_map(char::to_lowercase)
        .collect();
    if query.is_empty() {
        return Some(0);
    }
    let chars: Vec<char> = candidate.chars().flat_map(char::to_lowercase).collect();

    let mut matched = 0;
    let mut score = 0;
    let mut prev: Option<usize> = None;
    for (i, &c) in chars.iter().enumerate() {
        if matched == query.len() {
            break;
        }
        if c != query[matched] {
            continue;
        }
        score += 1;
        if i > 0 && prev == Some(i - 1) {
            score += 3;
        }
        if i == 0 || !chars[i - 1].is_alphanumeric() {
            score += 4;
        }
        prev = Some(i);
        matched += 1;
    }
    (matched == query.len()).then_some(score)
}

/// The indices of `items` that match `query`, best first. Ties keep their
/// original order.
pub fn rank<'a>(query: &str, items: impl Iterator<Item = &'a str>) -> Vec<usize> {
    let mut scored: Vec<(usize, i32)> = items
        .enumerate()
        .filter_map(|(i, text)| score(query, text).map(|s| (i, s)))
        .collect();
    scored.sort_by_key(|&(_, score)| std::cmp::Reverse(score));
    scored.into_iter().map(|(i, _)| i).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_query_keeps_order() {
        assert_eq!(rank("", ["b", "a"].into_iter()), [0, 1]);
    }

    #[test]
    fn requires_characters_in_order() {
        assert!(score("abk", "api-backend").is_some());
        assert!(score("kba", "api-backend").is_none());
    }

    #[test]
    fn prefers_word_starts_and_runs() {
        let items = ["infrastructure studio", "mobile-ios studio", "data studio"];
        assert_eq!(rank("ios", items.into_iter())[0], 1);
        assert_eq!(rank("infra", items.into_iter())[0], 0);
    }

    #[test]
    fn ignores_case_and_spaces() {
        assert!(score("Web App", "webapp-main").is_some());
    }
}
