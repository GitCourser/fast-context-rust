use std::cmp::Ordering;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::Path;

use once_cell::sync::Lazy;

static STOPWORDS: Lazy<HashSet<&'static str>> = Lazy::new(|| {
    [
        "the", "a", "an", "is", "are", "was", "were", "be", "been", "being", "have", "has", "had",
        "do", "does", "did", "will", "would", "could", "should", "may", "might", "must", "to",
        "of", "in", "for", "on", "with", "at", "by", "from", "as", "and", "but", "or", "not",
        "this", "that", "these", "those", "get", "set", "use", "used", "using", "make", "return",
        "new", "it", "its", "we", "you", "your",
    ]
    .into_iter()
    .collect()
});

#[derive(Debug, Clone, PartialEq)]
pub struct ScoredPath {
    pub path: String,
    pub score: f64,
}

#[must_use]
pub fn tokenize(text: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let chars: Vec<char> = text.chars().collect();

    for (index, ch) in chars.iter().copied().enumerate() {
        let prev = index.checked_sub(1).and_then(|idx| chars.get(idx)).copied();
        let next = chars.get(index + 1).copied();
        let boundary = should_split(prev, ch, next);

        if boundary && !current.is_empty() {
            push_token(&mut tokens, &current);
            current.clear();
        }

        if ch.is_ascii_alphanumeric() {
            current.push(ch);
        } else if !current.is_empty() {
            push_token(&mut tokens, &current);
            current.clear();
        }
    }

    if !current.is_empty() {
        push_token(&mut tokens, &current);
    }

    tokens
}

#[must_use]
pub fn quick_score(query: &str, path: &str) -> f64 {
    let query_tokens = tokenize(query);
    if query_tokens.is_empty() {
        return 0.0;
    }

    let path_tokens = tokenize(path);
    let path_token_set: HashSet<&str> = path_tokens.iter().map(String::as_str).collect();
    let lower_path = path.to_ascii_lowercase();
    let basename = Path::new(path)
        .file_name()
        .map(|name| name.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();

    let mut score = 0.0;
    for token in &query_tokens {
        if path_token_set.contains(token.as_str()) {
            score += 4.0;
        }
        if basename.contains(token) {
            score += 2.0;
        }
        if lower_path.contains(token) {
            score += 1.0;
        }
    }

    score - (path.matches('/').count() as f64 * 0.05)
}

#[must_use]
pub fn rank_paths(query: &str, paths: &[String]) -> Vec<ScoredPath> {
    let mut scored: Vec<ScoredPath> = paths
        .iter()
        .map(|path| ScoredPath {
            path: path.clone(),
            score: quick_score(query, path),
        })
        .collect();
    scored.sort_by(|left, right| {
        right
            .score
            .partial_cmp(&left.score)
            .unwrap_or(Ordering::Equal)
            .then_with(|| left.path.cmp(&right.path))
    });
    scored
}

#[must_use]
pub fn path_spines(paths: &[String]) -> Vec<ScoredPath> {
    let mut counts: BTreeMap<String, f64> = BTreeMap::new();
    for path in paths {
        let normalized = path.replace('\\', "/");
        let mut cumulative = String::new();
        for segment in normalized.split('/').filter(|part| !part.is_empty()) {
            if cumulative.is_empty() {
                cumulative.push_str(segment);
            } else {
                cumulative.push('/');
                cumulative.push_str(segment);
            }
            *counts.entry(cumulative.clone()).or_insert(0.0) += 1.0;
        }
    }

    let mut scored: Vec<ScoredPath> = counts
        .into_iter()
        .map(|(path, score)| ScoredPath { path, score })
        .collect();
    scored.sort_by(|left, right| {
        right
            .score
            .partial_cmp(&left.score)
            .unwrap_or(Ordering::Equal)
            .then_with(|| left.path.cmp(&right.path))
    });
    scored
}

#[must_use]
pub fn hot_dirs(paths: &[String], top_k: usize) -> Vec<ScoredPath> {
    let mut counts: HashMap<String, f64> = HashMap::new();
    for path in paths {
        let normalized = path.replace('\\', "/");
        let dir = normalized
            .rsplit_once('/')
            .map(|(dir, _)| dir)
            .unwrap_or("");
        if !dir.is_empty() {
            *counts.entry(dir.to_string()).or_insert(0.0) += 1.0;
        }
    }

    let mut scored: Vec<ScoredPath> = counts
        .into_iter()
        .map(|(path, score)| ScoredPath { path, score })
        .collect();
    scored.sort_by(|left, right| {
        right
            .score
            .partial_cmp(&left.score)
            .unwrap_or(Ordering::Equal)
            .then_with(|| left.path.cmp(&right.path))
    });
    scored.truncate(top_k);
    scored
}

fn should_split(prev: Option<char>, current: char, next: Option<char>) -> bool {
    if !current.is_ascii_alphanumeric() {
        return false;
    }
    match prev {
        Some(prev) if !prev.is_ascii_alphanumeric() => true,
        Some(prev) if prev.is_ascii_lowercase() && current.is_ascii_uppercase() => true,
        Some(prev)
            if prev.is_ascii_uppercase()
                && current.is_ascii_uppercase()
                && next.is_some_and(|next| next.is_ascii_lowercase()) =>
        {
            true
        }
        Some(prev) if prev.is_ascii_alphabetic() && current.is_ascii_digit() => true,
        Some(prev) if prev.is_ascii_digit() && current.is_ascii_alphabetic() => true,
        None => true,
        _ => false,
    }
}

fn push_token(tokens: &mut Vec<String>, token: &str) {
    let lower = token.to_ascii_lowercase();
    if lower.len() >= 2 && !STOPWORDS.contains(lower.as_str()) {
        tokens.push(stem(&lower));
    }
}

fn stem(token: &str) -> String {
    for suffix in [
        "ingly", "edly", "ing", "ness", "ment", "able", "ible", "ally", "ly", "ed", "s",
    ] {
        if token.len() > suffix.len() + 2 && token.ends_with(suffix) {
            return token[..token.len() - suffix.len()].to_string();
        }
    }
    token.to_string()
}
