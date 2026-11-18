pub(crate) fn extract_search_queries(task: &str) -> Vec<String> {
    let mut queries = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for raw in task.split(|ch: char| !(ch.is_ascii_alphanumeric() || ch == '_' || ch == '-')) {
        let token = raw.trim_matches(|ch: char| ch == '_' || ch == '-');
        if token.len() < 3 {
            continue;
        }
        let looks_code_like =
            token.contains(['_', '-']) || token.chars().any(|ch| ch.is_ascii_uppercase());
        let looks_like_word = token.len() >= 4
            && token.chars().all(|ch| ch.is_ascii_lowercase())
            && !REPO_SEARCH_STOPWORDS.contains(&token);
        if (looks_code_like || looks_like_word) && seen.insert(token.to_ascii_lowercase()) {
            queries.push(token.to_owned());
        }
        if queries.len() >= 4 {
            return queries;
        }
    }
    if queries.is_empty() {
        let fallback = task.trim();
        if !fallback.is_empty() {
            queries.push(fallback.chars().take(80).collect());
        }
    }
    queries
}

const REPO_SEARCH_STOPWORDS: &[&str] = &[
    "about", "after", "also", "before", "between", "could", "does", "done", "from", "have", "into",
    "just", "like", "more", "need", "only", "over", "should", "some", "that", "then", "there",
    "this", "what", "when", "where", "while", "with", "without", "would",
];
