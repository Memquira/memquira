//! Natural-language question → full-text query.
//!
//! Previously the keyword leg matched the whole question as one substring, so "What items does
//! John collect?" never matched a stored fact. Here the question becomes a set of terms that are
//! searched with OR, so facts rank by how many (and how rare) the matched terms are, not whether the
//! question appears verbatim:
//!
//! - words are split on anything that is not a letter or digit and lower-cased;
//! - English stop words and one-letter words are dropped;
//! - capitalised multi-word spans ("Sunrise Beach", "C. S. Lewis" → "s lewis" is dropped as too
//!   short) are added once more as phrases, so a fact naming the whole span ranks above one that
//!   only shares a word with it.
//!
//! Every term is made of letters and digits only, so rendering it into a backend query language
//! cannot inject operators (`OR`, `NEAR`, `*`, quotes, column filters).

use std::collections::HashSet;

/// English stop words (the common NLTK list plus common question words).
const STOP_WORDS: &[&str] = &[
    "a",
    "about",
    "above",
    "after",
    "again",
    "against",
    "all",
    "also",
    "am",
    "an",
    "and",
    "any",
    "are",
    "as",
    "at",
    "be",
    "because",
    "been",
    "before",
    "being",
    "below",
    "between",
    "both",
    "but",
    "by",
    "can",
    "could",
    "did",
    "do",
    "does",
    "doing",
    "done",
    "down",
    "during",
    "each",
    "else",
    "ever",
    "few",
    "for",
    "from",
    "further",
    "had",
    "has",
    "have",
    "having",
    "he",
    "her",
    "here",
    "hers",
    "herself",
    "him",
    "himself",
    "his",
    "how",
    "i",
    "if",
    "in",
    "into",
    "is",
    "it",
    "its",
    "itself",
    "just",
    "kind",
    "likely",
    "may",
    "me",
    "might",
    "more",
    "most",
    "must",
    "my",
    "myself",
    "no",
    "nor",
    "not",
    "now",
    "of",
    "off",
    "on",
    "once",
    "only",
    "or",
    "other",
    "our",
    "ours",
    "ourselves",
    "out",
    "over",
    "own",
    "same",
    "shall",
    "she",
    "should",
    "so",
    "some",
    "such",
    "than",
    "that",
    "the",
    "their",
    "theirs",
    "them",
    "themselves",
    "then",
    "there",
    "these",
    "they",
    "this",
    "those",
    "through",
    "to",
    "too",
    "under",
    "until",
    "up",
    "very",
    "was",
    "we",
    "were",
    "what",
    "when",
    "where",
    "which",
    "while",
    "who",
    "whom",
    "whose",
    "why",
    "will",
    "with",
    "would",
    "you",
    "your",
    "yours",
    "yourself",
    "yourselves",
];

/// Terms and phrases to search for, in first-occurrence order and without duplicates.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FulltextQuery {
    pub terms: Vec<String>,
    pub phrases: Vec<Vec<String>>,
}

impl FulltextQuery {
    /// Builds the query for `text`. An empty result means nothing is worth searching for.
    pub fn parse(text: &str) -> Self {
        let stop: HashSet<&str> = STOP_WORDS.iter().copied().collect();
        let mut terms: Vec<String> = Vec::new();
        for word in words(text) {
            let lower = word.to_lowercase();
            if lower.chars().count() < 2 || stop.contains(lower.as_str()) {
                continue;
            }
            if !terms.contains(&lower) {
                terms.push(lower);
            }
        }
        let mut phrases: Vec<Vec<String>> = Vec::new();
        for phrase in capitalised_spans(text) {
            let kept: Vec<String> = phrase
                .iter()
                .map(|word| word.to_lowercase())
                .filter(|word| word.chars().count() >= 2 && !stop.contains(word.as_str()))
                .collect();
            if kept.len() >= 2 && !phrases.contains(&kept) {
                phrases.push(kept);
            }
        }
        Self { terms, phrases }
    }

    /// Like [`parse`](Self::parse), for a search scoped to one user who goes by `user_names`.
    ///
    /// Every fact in that scope is from or about that user, so the user's own name in the question
    /// ("What did Priya buy?", searched in Priya's memories) matches a large share of the scope and
    /// says nothing about which fact is wanted. A term equal to a word of one of `user_names` is
    /// dropped, and so is a phrase made only of such words. Other people's names, and phrases that
    /// extend the user's name ("Priya Sharma" when only "Priya" is given), are kept.
    pub fn parse_scoped(text: &str, user_names: &[String]) -> Self {
        let mut query = Self::parse(text);
        let own: HashSet<String> = user_names
            .iter()
            .flat_map(|name| words(name).map(str::to_lowercase).collect::<Vec<_>>())
            .collect();
        if own.is_empty() {
            return query;
        }
        query.terms.retain(|term| !own.contains(term));
        query
            .phrases
            .retain(|phrase| !phrase.iter().all(|word| own.contains(word)));
        query
    }

    pub fn is_empty(&self) -> bool {
        self.terms.is_empty()
    }

    /// SQLite FTS5 `MATCH` expression: every term and phrase quoted, joined with `OR`.
    pub fn to_fts5_match(&self) -> String {
        self.terms
            .iter()
            .map(|term| quote_fts5(term))
            .chain(
                self.phrases
                    .iter()
                    .map(|phrase| quote_fts5(&phrase.join(" "))),
            )
            .collect::<Vec<_>>()
            .join(" OR ")
    }

    /// Postgres `websearch_to_tsquery` input: terms joined with `or`, phrases in double quotes.
    pub fn to_websearch(&self) -> String {
        self.terms
            .iter()
            .cloned()
            .chain(
                self.phrases
                    .iter()
                    .map(|phrase| format!("\"{}\"", phrase.join(" "))),
            )
            .collect::<Vec<_>>()
            .join(" or ")
    }

    /// Plain-Rust relevance used by the in-memory store: matched terms, plus one per matched phrase.
    pub fn overlap_score(&self, text: &str) -> usize {
        let haystack: Vec<String> = words(text).map(|word| word.to_lowercase()).collect();
        let term_hits = self
            .terms
            .iter()
            .filter(|term| haystack.contains(term))
            .count();
        let phrase_hits = self
            .phrases
            .iter()
            .filter(|phrase| {
                haystack
                    .windows(phrase.len())
                    .any(|window| window == phrase.as_slice())
            })
            .count();
        term_hits + phrase_hits
    }
}

/// Words: maximal runs of letters and digits.
fn words(text: &str) -> impl Iterator<Item = &str> {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|word| !word.is_empty())
}

/// Runs of two or more consecutive words that start with an upper-case letter.
fn capitalised_spans(text: &str) -> Vec<Vec<&str>> {
    let mut spans = Vec::new();
    let mut current: Vec<&str> = Vec::new();
    for word in words(text) {
        if word.chars().next().is_some_and(char::is_uppercase) {
            current.push(word);
        } else {
            if current.len() >= 2 {
                spans.push(std::mem::take(&mut current));
            }
            current.clear();
        }
    }
    if current.len() >= 2 {
        spans.push(current);
    }
    spans
}

fn quote_fts5(text: &str) -> String {
    format!("\"{}\"", text.replace('"', "\"\""))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn questions_become_content_terms() {
        let query = FulltextQuery::parse("What items does Priya collect?");
        assert_eq!(query.terms, vec!["items", "priya", "collect"]);
        assert!(query.phrases.is_empty());
    }

    #[test]
    fn capitalised_spans_become_phrases() {
        let query =
            FulltextQuery::parse("When did Omar visit Lake Tahoe with the Blue Ridge club?");
        assert_eq!(
            query.phrases,
            vec![vec!["lake", "tahoe"], vec!["blue", "ridge"]]
        );
        assert!(query.terms.contains(&"omar".to_string()));
        // A span that starts the sentence keeps only its content words.
        let query = FulltextQuery::parse("Did Omar Haddad move?");
        assert_eq!(query.phrases, vec![vec!["omar", "haddad"]]);
    }

    #[test]
    fn stop_words_and_single_letters_are_dropped() {
        let query = FulltextQuery::parse("Is it a big one, or was it?");
        assert_eq!(query.terms, vec!["big", "one"]);
        assert!(FulltextQuery::parse("what is it?").is_empty());
        assert!(FulltextQuery::parse("   ").is_empty());
    }

    #[test]
    fn operators_and_quotes_cannot_reach_the_backend_syntax() {
        let query = FulltextQuery::parse(r#"foo" OR bar* NEAR(baz) content:secret -- ; DROP"#);
        assert_eq!(
            query.terms,
            vec!["foo", "bar", "near", "baz", "content", "secret", "drop"]
        );
        let fts = query.to_fts5_match();
        assert_eq!(
            fts,
            r#""foo" OR "bar" OR "near" OR "baz" OR "content" OR "secret" OR "drop""#
        );
        assert!(!query.to_websearch().contains('-'));
    }

    #[test]
    fn non_english_text_is_tokenised_without_panicking() {
        let query = FulltextQuery::parse("¿Dónde vive Zoë en München? 東京で会った");
        assert!(query.terms.contains(&"dónde".to_string()));
        assert!(query.terms.contains(&"münchen".to_string()));
        assert!(!query.to_fts5_match().is_empty());
    }

    #[test]
    fn renders_for_both_backends() {
        let query = FulltextQuery::parse("Where is Sunrise Beach?");
        assert_eq!(
            query.to_fts5_match(),
            r#""sunrise" OR "beach" OR "sunrise beach""#
        );
        assert_eq!(
            query.to_websearch(),
            r#"sunrise or beach or "sunrise beach""#
        );
    }

    #[test]
    fn the_scoped_users_own_name_is_not_a_search_term() {
        let names = vec!["Priya".to_string()];
        let query = FulltextQuery::parse_scoped("What did Priya buy for Omar's birthday?", &names);
        assert_eq!(query.terms, vec!["buy", "omar", "birthday"]);
        // Case and possessives do not matter; another person's name stays.
        let query = FulltextQuery::parse_scoped("PRIYA's trip with Lena", &names);
        assert_eq!(query.terms, vec!["trip", "lena"]);
    }

    #[test]
    fn phrases_are_dropped_only_when_made_entirely_of_the_scoped_name() {
        let query = FulltextQuery::parse_scoped(
            "Did Mary Jane meet Mary Jane Watson at Sunrise Beach?",
            &["Mary Jane".to_string()],
        );
        assert!(!query.terms.contains(&"mary".to_string()));
        assert!(!query.terms.contains(&"jane".to_string()));
        assert!(query.terms.contains(&"watson".to_string()));
        assert!(
            !query
                .phrases
                .contains(&vec!["mary".to_string(), "jane".to_string()])
        );
        assert!(query.phrases.contains(&vec![
            "mary".to_string(),
            "jane".to_string(),
            "watson".to_string()
        ]));
        assert!(
            query
                .phrases
                .contains(&vec!["sunrise".to_string(), "beach".to_string()])
        );
    }

    #[test]
    fn without_scope_names_the_query_is_unchanged() {
        let text = "When did Omar visit Lake Tahoe?";
        assert_eq!(
            FulltextQuery::parse_scoped(text, &[]),
            FulltextQuery::parse(text)
        );
        assert_eq!(
            FulltextQuery::parse_scoped(text, &["  ".to_string()]),
            FulltextQuery::parse(text)
        );
        // A question that is only the user's name leaves nothing to search for.
        assert!(FulltextQuery::parse_scoped("Priya?", &["Priya".to_string()]).is_empty());
    }

    #[test]
    fn overlap_score_counts_terms_and_phrases() {
        let query = FulltextQuery::parse("Where is Sunrise Beach?");
        assert_eq!(query.overlap_score("Priya surfed at Sunrise Beach."), 3);
        assert_eq!(query.overlap_score("The beach was quiet."), 1);
        assert_eq!(query.overlap_score("Nothing here."), 0);
    }
}
