//! Fuzzy matching for Goto-File and the
//! directory-view filter.
//!
//! Three modes:
//! `Default` = case-insensitive prefix,
//! `Partial` = case-insensitive substring,
//! `Fuzzy`  = subsequence with scoring.

/// How the Goto-File popup matches what the user types.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FuzzyMode {
    Default,
    Partial,
    Fuzzy,
}

/// Result of a successful match.
#[derive(Clone, Debug)]
pub struct FuzzyMatch {
    /// Higher is better; used to rank results.
    pub score: i64,
    /// Byte indices of the matched characters
    /// (for highlight rendering).
    pub positions: Vec<usize>,
}

fn lower(text: &str) -> Vec<char> {
    text.chars().map(|c| c.to_ascii_lowercase()).collect()
}

/// Match `pattern` against `text` in the given
/// mode; `None` when there is no match.
pub fn fuzzy_match(pattern: &str, text: &str, mode: FuzzyMode) -> Option<FuzzyMatch> {
    if pattern.is_empty() {
        return Some(FuzzyMatch { score: 0, positions: Vec::new() });
    }
    let pat: Vec<char> = lower(pattern);
    let txt: Vec<char> = lower(text);
    match mode {
        FuzzyMode::Default => {
            if txt.len() >= pat.len() && txt[..pat.len()] == pat[..] {
                Some(FuzzyMatch {
                    score: 1000 - txt.len() as i64,
                    positions: (0..pat.len()).collect(),
                })
            } else {
                None
            }
        }
        FuzzyMode::Partial => {
            if pat.len() > txt.len() {
                return None;
            }
            for start in 0..=(txt.len() - pat.len()) {
                if txt[start..start + pat.len()] == pat[..] {
                    // earlier match + shorter text ranks higher
                    let score = 500 - start as i64 - txt.len() as i64 / 10;
                    return Some(FuzzyMatch {
                        score,
                        positions: (start..start + pat.len()).collect(),
                    });
                }
            }
            None
        }
        FuzzyMode::Fuzzy => subsequence_score(&pat, &txt),
    }
}

/// Subsequence match with a small scoring
/// scheme: consecutive matches and
/// word-boundary starts score higher.
fn subsequence_score(pat: &[char], txt: &[char]) -> Option<FuzzyMatch> {
    // Greedy left-to-right subsequence scan:
    // each pattern char matches the next equal
    // character at or after the previous match.
    let mut positions = Vec::with_capacity(pat.len());
    let mut ti = 0usize;
    for &pc in pat {
        let mut found = None;
        while ti < txt.len() {
            if txt[ti] == pc {
                found = Some(ti);
                break;
            }
            ti += 1;
        }
        match found {
            Some(p) => {
                positions.push(p);
                ti = p + 1;
            }
            None => return None,
        }
    }
    // Score: base per char, bonus for consecutive
    // runs and for matching at a word start.
    let mut score: i64 = 0;
    let mut prev: Option<usize> = None;
    for &p in positions.iter() {
        score += 1;
        if let Some(prev_p) = prev {
            if p == prev_p + 1 {
                score += 3; // consecutive run
            }
        }
        let at_start = p == 0
            || txt[p - 1] == '_'
            || txt[p - 1] == '-'
            || txt[p - 1] == '.'
            || txt[p - 1] == ' '
            || txt[p - 1] == '/'
            || txt[p - 1] == '\\'
            || (txt[p - 1].is_lowercase() && txt[p].is_uppercase());
        if at_start {
            score += 2;
        }
        prev = Some(p);
    }
    // prefer shorter texts
    score -= txt.len() as i64 / 20;
    Some(FuzzyMatch { score, positions })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_mode_prefix() {
        assert!(fuzzy_match("dg", "dg-core", FuzzyMode::Default).is_some());
        assert!(fuzzy_match("core", "dg-core", FuzzyMode::Default).is_none());
        assert!(fuzzy_match("DG", "dg-core", FuzzyMode::Default).is_some());
    }

    #[test]
    fn partial_mode_substring() {
        assert!(fuzzy_match("core", "dg-core", FuzzyMode::Partial).is_some());
        assert!(fuzzy_match("zzz", "dg-core", FuzzyMode::Partial).is_none());
        // earlier substring wins
        let a = fuzzy_match("e", "ae-be", FuzzyMode::Partial).unwrap();
        assert_eq!(a.positions[0], 1);
        let b = fuzzy_match("e", "ea-be", FuzzyMode::Partial).unwrap();
        assert_eq!(b.positions[0], 0);
    }

    #[test]
    fn fuzzy_mode_subsequence() {
        let m = fuzzy_match("end", "dg-render", FuzzyMode::Fuzzy).unwrap();
        // e-n-d subsequence exists
        assert_eq!(m.positions.len(), 3);
        assert!(fuzzy_match("zzz", "dg-render", FuzzyMode::Fuzzy).is_none());
    }

    #[test]
    fn fuzzy_scores_consecutive_higher() {
        let consecutive = fuzzy_match("abc", "xxabcxx", FuzzyMode::Fuzzy).unwrap();
        let spread = fuzzy_match("abc", "axbxcxx", FuzzyMode::Fuzzy).unwrap();
        assert!(consecutive.score > spread.score);
    }

    #[test]
    fn empty_pattern_matches_all() {
        let m = fuzzy_match("", "anything", FuzzyMode::Fuzzy).unwrap();
        assert_eq!(m.score, 0);
    }
}
