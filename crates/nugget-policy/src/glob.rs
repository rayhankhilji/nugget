//! Case-insensitive glob matching for app ids, hosts, verbs and labels.

/// Match `text` against `pattern`, ignoring case. `*` matches any run of characters (including
/// none), `?` exactly one. A pattern starting with `*.` also matches the bare suffix, so
/// `*.chase.com` matches both `secure.chase.com` and `chase.com`.
pub fn glob_match(pattern: &str, text: &str) -> bool {
    let p: Vec<char> = pattern.to_lowercase().chars().collect();
    let t: Vec<char> = text.to_lowercase().chars().collect();
    if matches(&p, &t) {
        return true;
    }
    p.len() > 2 && p[0] == '*' && p[1] == '.' && matches(&p[2..], &t)
}

/// Iterative wildcard match with single-star backtracking: linear in practice, no recursion.
fn matches(p: &[char], t: &[char]) -> bool {
    let (mut pi, mut ti) = (0usize, 0usize);
    let mut star: Option<(usize, usize)> = None;
    while ti < t.len() {
        if pi < p.len() && (p[pi] == '?' || p[pi] == t[ti]) {
            pi += 1;
            ti += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star = Some((pi, ti));
            pi += 1;
        } else if let Some((sp, st)) = star {
            pi = sp + 1;
            ti = st + 1;
            star = Some((sp, st + 1));
        } else {
            return false;
        }
    }
    while pi < p.len() && p[pi] == '*' {
        pi += 1;
    }
    pi == p.len()
}

/// How specific a pattern is: the number of literal (non-wildcard) characters.
pub(crate) fn specificity(pattern: &str) -> usize {
    pattern.chars().filter(|c| *c != '*' && *c != '?').count()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn literals_and_case() {
        assert!(glob_match("com.apple.Mail", "com.apple.mail"));
        assert!(!glob_match("com.apple.mail", "com.apple.mail2"));
    }

    #[test]
    fn stars_and_questions() {
        assert!(glob_match("*bank*", "bankofamerica.com"));
        assert!(glob_match("*bank*", "com.usbank.mobile"));
        assert!(glob_match("com.*.mail", "com.apple.mail"));
        assert!(glob_match("a?c", "abc"));
        assert!(!glob_match("a?c", "ac"));
        assert!(glob_match("*", ""));
        assert!(glob_match("a*b*c", "aXXbYYc"));
        assert!(!glob_match("a*b*c", "aXXbYY"));
    }

    #[test]
    fn dot_star_prefix_matches_bare_domain() {
        assert!(glob_match("*.chase.com", "chase.com"));
        assert!(glob_match("*.chase.com", "secure.chase.com"));
        assert!(!glob_match("*.chase.com", "notchase.com"));
    }

    #[test]
    fn unicode() {
        assert!(glob_match("*löschen*", "Endgültig LÖSCHEN"));
    }

    #[test]
    fn specificity_counts_literals() {
        assert_eq!(specificity("*"), 0);
        assert_eq!(specificity("*.chase.com"), 10);
    }
}
