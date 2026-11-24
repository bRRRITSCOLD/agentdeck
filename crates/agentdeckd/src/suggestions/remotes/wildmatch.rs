//! git's `wildmatch` with `WM_PATHNAME`, which `includeIf` conditions match with (spec §3
//! Suggestions): `*`, `?` and `[…]` never match a `/`; `**` does as `**/` at the start, `/**/`
//! or a trailing `/**`, and is a `*` anywhere else. `\` takes the next character literally.

/// Whether the whole of `text` matches `pattern`, ignoring ASCII case when `fold`.
pub fn wildmatch(pattern: &str, text: &str, fold: bool) -> bool {
    let pattern: Vec<char> = pattern.chars().collect();
    let text: Vec<char> = text.chars().collect();
    matches(&pattern, 0, &text, 0, fold)
}

/// Whether `text[t..]` matches `pattern[p..]`.
fn matches(pattern: &[char], mut p: usize, text: &[char], mut t: usize, fold: bool) -> bool {
    while let Some(&c) = pattern.get(p) {
        match c {
            '*' => {
                let mut end = p;
                while pattern.get(end) == Some(&'*') {
                    end += 1;
                }
                let after_slash = p == 0 || pattern[p - 1] == '/';
                if end - p >= 2 && after_slash {
                    match pattern.get(end) {
                        // A trailing `/**` (or a lone `**`) takes everything left.
                        None => return true,
                        // `**/` takes no folders, or any number of whole ones.
                        Some('/') => {
                            return matches(pattern, end + 1, text, t, fold)
                                || (t..text.len()).any(|i| {
                                    text[i] == '/' && matches(pattern, end + 1, text, i + 1, fold)
                                });
                        }
                        Some(_) => {}
                    }
                }
                // Anything else is a `*`: any run of characters but `/`.
                let mut i = t;
                loop {
                    if matches(pattern, end, text, i, fold) {
                        return true;
                    }
                    if i == text.len() || text[i] == '/' {
                        return false;
                    }
                    i += 1;
                }
            }
            '?' => {
                if text.get(t).is_none_or(|&c| c == '/') {
                    return false;
                }
                p += 1;
                t += 1;
            }
            '[' => {
                let Some(&got) = text.get(t) else {
                    return false;
                };
                match class(pattern, p + 1, got, fold) {
                    Some((true, next)) if got != '/' => {
                        p = next;
                        t += 1;
                    }
                    _ => return false,
                }
            }
            c => {
                let (want, next) = match c {
                    '\\' if p + 1 < pattern.len() => (pattern[p + 1], p + 2),
                    c => (c, p + 1),
                };
                if !text.get(t).is_some_and(|&got| same(want, got, fold)) {
                    return false;
                }
                p = next;
                t += 1;
            }
        }
    }
    t == text.len()
}

/// Whether the class starting at `pattern[p..]` (just after its `[`) holds `got`, and where the
/// pattern goes on after its `]`; `None` for a class with no `]`.
fn class(pattern: &[char], mut p: usize, got: char, fold: bool) -> Option<(bool, usize)> {
    let negated = matches!(pattern.get(p), Some('!' | '^'));
    if negated {
        p += 1;
    }
    let mut held = false;
    let mut first = true;
    loop {
        let mut c = *pattern.get(p)?;
        if c == ']' && !first {
            return Some((held != negated, p + 1));
        }
        first = false;
        if c == '\\' {
            p += 1;
            c = *pattern.get(p)?;
        }
        if pattern.get(p + 1) == Some(&'-') && pattern.get(p + 2).is_some_and(|&e| e != ']') {
            let mut end = pattern[p + 2];
            p += 3;
            if end == '\\' {
                end = *pattern.get(p)?;
                p += 1;
            }
            let within = |g: char| c <= g && g <= end;
            held |= within(got)
                || (fold && (within(got.to_ascii_lowercase()) || within(got.to_ascii_uppercase())));
        } else {
            held |= same(c, got, fold);
            p += 1;
        }
    }
}

fn same(want: char, got: char, fold: bool) -> bool {
    want == got || (fold && want.eq_ignore_ascii_case(&got))
}

#[cfg(test)]
mod tests {
    use super::*;

    mod wildmatch {
        use super::*;

        #[test]
        fn should_match_literal_text_whole() {
            assert!(wildmatch("a/b", "a/b", false));
            assert!(!wildmatch("a/b", "a/bc", false));
            assert!(!wildmatch("a/bc", "a/b", false));
        }

        #[test]
        fn should_not_let_a_star_or_question_mark_cross_a_slash() {
            assert!(wildmatch("a/*.git", "a/repo.git", false));
            assert!(!wildmatch("a/*", "a/b/c", false));
            assert!(wildmatch("a/?", "a/b", false));
            assert!(!wildmatch("a?b", "a/b", false));
        }

        #[test]
        fn should_let_a_double_star_between_slashes_cross_them() {
            assert!(wildmatch("**/work/**", "/home/me/work/repo/.git", false));
            assert!(wildmatch("/home/**/.git", "/home/me/work/repo/.git", false));
            assert!(wildmatch("a/**/b", "a/b", false));
            assert!(!wildmatch(
                "/home/me/work/**",
                "/home/me/workshop/.git",
                false
            ));
        }

        #[test]
        fn should_read_a_double_star_inside_a_name_as_a_star() {
            assert!(wildmatch("a/b**", "a/bcd", false));
            assert!(!wildmatch("a/b**", "a/b/c", false));
        }

        #[test]
        fn should_match_a_bracket_class_with_ranges_and_negation() {
            assert!(wildmatch("v[0-9]", "v7", false));
            assert!(!wildmatch("v[!0-9]", "v7", false));
            assert!(wildmatch("v[^0-9]", "vx", false));
            assert!(wildmatch("[]x]", "]", false));
            assert!(!wildmatch("a[/]b", "a/b", false));
        }

        #[test]
        fn should_take_an_escaped_character_literally() {
            assert!(wildmatch(r"a\*", "a*", false));
            assert!(!wildmatch(r"a\*", "ab", false));
        }

        #[test]
        fn should_ignore_case_only_when_folding() {
            assert!(wildmatch("/Home/*", "/home/x", true));
            assert!(!wildmatch("/Home/*", "/home/x", false));
            assert!(wildmatch("[A-Z]", "q", true));
            assert!(!wildmatch("[A-Z]", "q", false));
        }
    }
}
