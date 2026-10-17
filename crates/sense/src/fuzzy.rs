//! Fuzzy matching for pickers: a query matches when its characters appear
//! in order in the text (case aside), and matches rank by how readable they
//! are: runs of consecutive characters and starts of words beat scattered
//! letters, so "son" finds "Claude Sonnet" before "Mistral Large Version".

/// How well `query` matches `text`, or `None` if it doesn't. Higher is
/// better; an empty query matches everything equally.
pub fn score(query: &str, text: &str) -> Option<i64> {
    let query: Vec<char> = query
        .chars()
        .filter(|c| !c.is_whitespace())
        .flat_map(char::to_lowercase)
        .collect();
    if query.is_empty() {
        return Some(0);
    }
    let text: Vec<char> = text.chars().flat_map(char::to_lowercase).collect();
    // Each place the first character occurs could start the best match;
    // from each, the rest is taken greedily.
    (0..text.len())
        .filter(|&start| text[start] == query[0])
        .filter_map(|start| score_from(&query, &text, start))
        .max()
}

fn score_from(query: &[char], text: &[char], start: usize) -> Option<i64> {
    let mut total = 0;
    let mut at = start;
    let mut previous: Option<usize> = None;
    for &wanted in query {
        let found = (at..text.len()).find(|&ix| text[ix] == wanted)?;
        total += 1;
        if previous.is_some_and(|p| p + 1 == found) {
            total += 6;
        }
        if found == 0 || matches!(text[found - 1], ' ' | '/' | '-' | '.' | ':' | '_' | '(') {
            total += 8;
        }
        if let Some(p) = previous {
            total -= (found - p - 1).min(4) as i64;
        }
        previous = Some(found);
        at = found + 1;
    }
    // A match nearer the start reads better.
    Some(total * 4 - start.min(20) as i64)
}

/// The indexes of `items` that match `query`, best first; equal scores keep
/// the items' order.
pub fn rank<'a>(query: &str, items: impl IntoIterator<Item = &'a str>) -> Vec<usize> {
    let mut scored: Vec<(i64, usize)> = items
        .into_iter()
        .enumerate()
        .filter_map(|(ix, text)| Some((score(query, text)?, ix)))
        .collect();
    scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
    scored.into_iter().map(|(_, ix)| ix).collect()
}

#[cfg(test)]
mod tests {
    use super::{rank, score};
    use proptest::prelude::*;

    /// The independent definition of a match: the query's characters, case
    /// and spaces aside, appear in order in the text.
    fn is_subsequence(query: &str, text: &str) -> bool {
        let mut text = text.chars().flat_map(char::to_lowercase);
        query
            .chars()
            .filter(|c| !c.is_whitespace())
            .flat_map(char::to_lowercase)
            .all(|wanted| text.any(|c| c == wanted))
    }

    proptest! {
        #[test]
        fn matches_exactly_the_in_order_queries(query in "[a-cA-C /.]{0,5}", text in "[a-cA-C /.-]{0,12}") {
            prop_assert_eq!(score(&query, &text).is_some(), is_subsequence(&query, &text));
        }

        /// Ranking keeps every match and nothing else, best first, ties in
        /// the list's order.
        #[test]
        fn ranking_is_a_sorted_filter(query in "[a-c ]{0,3}", items in proptest::collection::vec("[a-c/ ]{0,8}", 0..12)) {
            let ranked = rank(&query, items.iter().map(String::as_str));
            let expected: Vec<usize> = (0..items.len()).filter(|&ix| is_subsequence(&query, &items[ix])).collect();
            let mut sorted = ranked.clone();
            sorted.sort();
            prop_assert_eq!(sorted, expected);
            for pair in ranked.windows(2) {
                let (a, b) = (score(&query, &items[pair[0]]).unwrap(), score(&query, &items[pair[1]]).unwrap());
                prop_assert!(a > b || (a == b && pair[0] < pair[1]), "{:?}", ranked);
            }
        }
    }

    #[test]
    fn readable_matches_rank_first() {
        let models = [
            "Mistral Large Version 4 mistralai/mistral-large-4",
            "Claude Sonnet 5.5 anthropic/claude-sonnet-5.5",
            "GPT-6.1 Sol openai/gpt-6.1-sol",
            "Claude Haiku 5.5 anthropic/claude-haiku-5.5",
        ];
        let first = |query: &str| models[rank(query, models)[0]];
        assert!(first("son").starts_with("Claude Sonnet"));
        assert!(first("haiku").starts_with("Claude Haiku"));
        assert!(first("gpt sol").starts_with("GPT-6.1 Sol"));
        assert!(first("cs55").starts_with("Claude Sonnet"));
        assert_eq!(rank("zzz", models), Vec::<usize>::new());
        assert_eq!(rank("", models), vec![0, 1, 2, 3]);
    }
}
