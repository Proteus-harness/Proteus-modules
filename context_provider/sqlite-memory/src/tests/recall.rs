use super::*;

fn remember_text(conn: &Mutex<Connection>, content: &str) {
    remember_impl(
        conn,
        &serde_json::json!({"kind": "fact", "content": content, "metadata": null}).to_string(),
    )
    .expect("remember");
}

fn recall_texts(conn: &Mutex<Connection>, text: &str) -> Vec<String> {
    let body = recall_impl(
        conn,
        &serde_json::json!({"text": text, "limit": 20}).to_string(),
    )
    .expect("recall");
    let items: Vec<MemoryItem> = serde_json::from_str(&body).expect("memory items");
    items.into_iter().map(|item| item.content).collect()
}

#[test]
fn punctuation_separates_indexed_words_instead_of_joining_them() {
    for (literal, joined) in [
        ("src/config.rs", "srcconfigrs"),
        ("Config::load", "Configload"),
        ("v1.2.3", "v123"),
        ("alpha\"beta", "alphabeta"),
        ("alpha+beta", "alphabeta"),
    ] {
        let conn = fresh_conn();
        remember_text(&conn, literal);
        remember_text(&conn, joined);
        assert_eq!(recall_texts(&conn, literal), vec![literal], "{literal}");
        assert_eq!(recall_texts(&conn, joined), vec![joined], "{joined}");
    }
}

#[test]
fn phrase_prefix_applies_only_to_last_token_and_preserves_adjacency() {
    let conn = fresh_conn();
    for content in [
        "src/config.rs",
        "src/config.rst",
        "src/configuration.rs",
        "src/extra/config.rs",
    ] {
        remember_text(&conn, content);
    }
    let mut actual = recall_texts(&conn, "src/config.r");
    actual.sort();
    assert_eq!(actual, vec!["src/config.rs", "src/config.rst"]);
}

#[test]
fn whitespace_and_and_existing_compound_phrases_keep_their_meaning() {
    let conn = fresh_conn();
    remember_text(&conn, "dark mode");
    assert_eq!(recall_texts(&conn, "mod dark"), vec!["dark mode"]);
    for literal in ["some_name", "some-name"] {
        let conn = fresh_conn();
        remember_text(&conn, literal);
        remember_text(&conn, "some other name");
        assert_eq!(recall_texts(&conn, literal), vec![literal]);
    }
}

#[test]
fn sqlite_owns_combining_mark_tokenization() {
    let conn = fresh_conn();
    let literal = "co\u{301}de";
    remember_text(&conn, literal);
    assert_eq!(recall_texts(&conn, literal), vec![literal]);
    assert_eq!(fts_match_expression(literal), format!("\"{literal}\"*"));
}

#[test]
fn nul_becomes_a_phrase_separator_not_a_merged_token_or_parse_error() {
    let conn = fresh_conn();
    remember_text(&conn, "alpha beta");
    remember_text(&conn, "alphabeta");
    assert_eq!(recall_texts(&conn, "alpha\0beta"), vec!["alpha beta"]);
    assert_eq!(fts_match_expression("alpha\0beta"), "\"alpha beta\"*");
}

#[test]
fn empty_span_gate_preserves_recent_fallback_and_legacy_empty_phrases() {
    let conn = fresh_conn();
    remember_text(&conn, "first");
    remember_text(&conn, "second");
    for query in ["", " \t\n", "!!!", "\"", "\0", "... / ::"] {
        assert_eq!(fts_match_expression(query), "");
        assert_eq!(recall_texts(&conn, query), vec!["second", "first"]);
    }
    for query in ["-", "_"] {
        assert_eq!(fts_match_expression(query), format!("\"{query}\"*"));
        assert!(recall_texts(&conn, query).is_empty());
    }
}
