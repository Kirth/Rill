//! "Did you mean" suggestions for unknown names.

/// The candidate closest to `target`, when it is close enough to be a likely
/// typo.  Exact matches and internal `$`-prefixed names are never suggested.
pub fn closest<'a>(target: &str, candidates: impl IntoIterator<Item = &'a str>) -> Option<&'a str> {
    let mut best: Option<(&'a str, f64)> = None;
    for candidate in candidates {
        if candidate == target || candidate.starts_with('$') {
            continue;
        }
        let similarity = strsim::jaro_winkler(target, candidate);
        let distance = strsim::levenshtein(target, candidate);
        let close = similarity >= 0.85 || distance <= (target.len() / 3).max(1);
        if !close {
            continue;
        }
        let better = match best {
            None => true,
            Some((current, score)) => {
                similarity > score || (similarity == score && candidate.len() < current.len())
            }
        };
        if better {
            best = Some((candidate, similarity));
        }
    }
    best.map(|(candidate, _)| candidate)
}

/// `did you mean \`name\`?` when a close candidate exists.
pub fn did_you_mean<'a>(
    target: &str,
    candidates: impl IntoIterator<Item = &'a str>,
) -> Option<String> {
    closest(target, candidates).map(|candidate| format!("did you mean `{candidate}`?"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn suggests_close_names_only() {
        assert_eq!(closest("hots", ["host", "args", "print"]), Some("host"));
        assert_eq!(closest("lnes", ["lines", "len", "join"]), Some("lines"));
        assert_eq!(closest("zzzz", ["host", "args"]), None);
        assert_eq!(closest("host", ["host"]), None);
    }
}
