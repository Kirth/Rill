use crate::diagnostic::RillError;
use std::path::PathBuf;

/// Resolves a path template with tilde and variable expansion.
///
/// Expansion order:
/// 1. Leading `~` or `~/` → HOME; `~user` → that user's home directory.
/// 2. `$NAME`, `${NAME}`, `${NAME:-default}` via `lookup` (Rill binding first, then
///    process env at the call site), inserting values literally to prevent
///    double expansion.  Default expressions are themselves expanded recursively.
///
/// A missing variable without a default is a diagnostic error.
/// An unresolvable tilde expression is left literal.
/// Backslash escapes the next character (`\$` → `$`, `\~` → `~`).
pub fn resolve_path(
    template: &str,
    lookup: &dyn Fn(&str) -> Option<String>,
) -> Result<PathBuf, RillError> {
    resolve_with_users(template, lookup, &OsUserLookup)
}

pub fn resolve_with_users(
    template: &str,
    lookup: &dyn Fn(&str) -> Option<String>,
    users: &dyn UserLookup,
) -> Result<PathBuf, RillError> {
    Ok(PathBuf::from(expand_template(template, lookup, users)?))
}

pub trait UserLookup {
    fn home_for(&self, username: &str) -> Option<PathBuf>;
}

pub struct OsUserLookup;

impl UserLookup for OsUserLookup {
    fn home_for(&self, username: &str) -> Option<PathBuf> {
        os_home_for(username)
    }
}

pub fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

fn os_home_for(username: &str) -> Option<PathBuf> {
    use std::ffi::CString;
    let c_name = CString::new(username).ok()?;
    let pw = unsafe { libc::getpwnam(c_name.as_ptr()) };
    if pw.is_null() {
        return None;
    }
    let dir = unsafe { std::ffi::CStr::from_ptr((*pw).pw_dir) };
    Some(PathBuf::from(dir.to_string_lossy().as_ref()))
}

fn expand_template(
    template: &str,
    lookup: &dyn Fn(&str) -> Option<String>,
    users: &dyn UserLookup,
) -> Result<String, RillError> {
    let mut result = String::new();
    let mut chars = template.char_indices().peekable();

    // Step 1: expand leading tilde before variable substitution (spec requirement).
    if matches!(chars.peek(), Some((_, '~'))) {
        chars.next();
        match chars.peek() {
            // `~` alone or `~/`
            None | Some((_, '/')) => match home_dir() {
                Some(home) => result.push_str(&home.to_string_lossy()),
                None => result.push('~'),
            },
            // `~username` – only valid username chars (alphanumeric / _ / -)
            Some((_, c)) if c.is_ascii_alphanumeric() || *c == '_' => {
                let mut username = String::new();
                while let Some((_, c)) = chars.peek() {
                    if *c == '/' || (!c.is_ascii_alphanumeric() && *c != '_' && *c != '-') {
                        break;
                    }
                    username.push(*c);
                    chars.next();
                }
                match users.home_for(&username) {
                    Some(home) => result.push_str(&home.to_string_lossy()),
                    None => {
                        result.push('~');
                        result.push_str(&username);
                    }
                }
            }
            // Not a recognised tilde expression – leave literal.
            _ => result.push('~'),
        }
    }

    // Step 2: expand variables and handle escapes.
    while let Some((_, ch)) = chars.next() {
        match ch {
            '\\' => {
                // Escape: next char is literal.
                match chars.next() {
                    Some((_, next)) => result.push(next),
                    None => result.push('\\'),
                }
            }
            '$' => {
                if matches!(chars.peek(), Some((_, '{'))) {
                    chars.next(); // consume '{'
                    let (name, default) = parse_braced(&mut chars)?;
                    let value = lookup(&name);
                    let nonempty = value.as_deref().is_some_and(|v| !v.is_empty());
                    if nonempty {
                        // Insert literally – no re-expansion of the value.
                        result.push_str(value.unwrap().as_str());
                    } else if let Some(dflt) = default {
                        // Default is itself a template.
                        result.push_str(&expand_template(&dflt, lookup, users)?);
                    } else if value.is_some() {
                        // Set but empty, no default – insert nothing.
                    } else {
                        return Err(RillError::new(format!(
                            "unset variable '${{{}}}' in path",
                            name
                        )));
                    }
                } else {
                    let mut name = String::new();
                    while let Some((_, c)) = chars.peek() {
                        if c.is_ascii_alphanumeric() || *c == '_' {
                            name.push(*c);
                            chars.next();
                        } else {
                            break;
                        }
                    }
                    if name.is_empty() {
                        result.push('$');
                    } else if let Some(value) = lookup(&name) {
                        result.push_str(&value); // literal
                    } else {
                        return Err(RillError::new(format!(
                            "unset variable '${name}' in path"
                        )));
                    }
                }
            }
            other => result.push(other),
        }
    }

    Ok(result)
}

/// Parse `NAME}` or `NAME:-default}` after the opening `{` has been consumed.
fn parse_braced(
    chars: &mut std::iter::Peekable<impl Iterator<Item = (usize, char)>>,
) -> Result<(String, Option<String>), RillError> {
    let mut name = String::new();
    loop {
        match chars.next() {
            None => return Err(RillError::new("unclosed '${'  in path")),
            Some((_, '}')) => return Ok((name, None)),
            Some((_, ':')) => {
                if matches!(chars.peek(), Some((_, '-'))) {
                    chars.next(); // consume '-'
                    return Ok((name, Some(collect_default(chars)?)));
                }
                name.push(':');
            }
            Some((_, ch)) => name.push(ch),
        }
    }
}

/// Collect the default template up to the closing `}`, tracking brace depth.
fn collect_default(
    chars: &mut std::iter::Peekable<impl Iterator<Item = (usize, char)>>,
) -> Result<String, RillError> {
    let mut result = String::new();
    let mut depth = 1usize;
    loop {
        match chars.next() {
            None => return Err(RillError::new("unclosed '${'  in path")),
            Some((_, '{')) => {
                depth += 1;
                result.push('{');
            }
            Some((_, '}')) => {
                depth -= 1;
                if depth == 0 {
                    return Ok(result);
                }
                result.push('}');
            }
            Some((_, ch)) => result.push(ch),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct FakeUsers(Vec<(&'static str, &'static str)>);
    impl UserLookup for FakeUsers {
        fn home_for(&self, username: &str) -> Option<PathBuf> {
            self.0
                .iter()
                .find(|(u, _)| *u == username)
                .map(|(_, h)| PathBuf::from(h))
        }
    }

    fn env_lookup<'a>(vars: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
        move |name: &str| {
            vars.iter()
                .find(|(k, _)| *k == name)
                .map(|(_, v)| v.to_string())
        }
    }

    fn resolve(template: &str, vars: &[(&str, &str)]) -> Result<PathBuf, RillError> {
        resolve_with_users(template, &env_lookup(vars), &FakeUsers(vec![]))
    }

    fn resolve_u(
        template: &str,
        vars: &[(&str, &str)],
        users: &[(&'static str, &'static str)],
    ) -> Result<PathBuf, RillError> {
        resolve_with_users(template, &env_lookup(vars), &FakeUsers(users.to_vec()))
    }

    #[test]
    fn tilde_alone_expands_to_home() {
        // Tilde expansion reads HOME from std::env.  Set a controlled value.
        let old = std::env::var_os("HOME");
        unsafe { std::env::set_var("HOME", "/home/testuser") };
        let r = resolve_with_users("~", &env_lookup(&[]), &FakeUsers(vec![])).unwrap();
        assert_eq!(r, PathBuf::from("/home/testuser"));
        let r2 = resolve_with_users("~/data", &env_lookup(&[]), &FakeUsers(vec![])).unwrap();
        assert_eq!(r2, PathBuf::from("/home/testuser/data"));
        // Restore.
        match old {
            Some(v) => unsafe { std::env::set_var("HOME", v) },
            None => unsafe { std::env::remove_var("HOME") },
        }
    }

    #[test]
    fn dollar_name_substitution() {
        let r = resolve("$FOO/bar", &[("FOO", "/opt")]).unwrap();
        assert_eq!(r, PathBuf::from("/opt/bar"));
    }

    #[test]
    fn braced_dollar_substitution() {
        let r = resolve("${FOO}/bar", &[("FOO", "/opt")]).unwrap();
        assert_eq!(r, PathBuf::from("/opt/bar"));
    }

    #[test]
    fn default_used_when_unset() {
        let r = resolve("${FOO:-/fallback}/bar", &[]).unwrap();
        assert_eq!(r, PathBuf::from("/fallback/bar"));
    }

    #[test]
    fn default_skipped_when_set() {
        let r = resolve("${FOO:-/fallback}/bar", &[("FOO", "/real")]).unwrap();
        assert_eq!(r, PathBuf::from("/real/bar"));
    }

    #[test]
    fn empty_var_uses_default() {
        let r = resolve("${FOO:-/fallback}", &[("FOO", "")]).unwrap();
        assert_eq!(r, PathBuf::from("/fallback"));
    }

    #[test]
    fn nested_default() {
        let r = resolve("${A:-${B:-/deep}}", &[]).unwrap();
        assert_eq!(r, PathBuf::from("/deep"));
    }

    #[test]
    fn value_inserted_literally_no_reexpand() {
        // $VAR contains shell-looking text; it must NOT be re-expanded.
        let r = resolve("$VAR/end", &[("VAR", "$OTHER")]).unwrap();
        assert_eq!(r, PathBuf::from("$OTHER/end"));
    }

    #[test]
    fn missing_var_no_default_is_error() {
        assert!(resolve("$MISSING", &[]).is_err());
    }

    #[test]
    fn backslash_escapes_dollar() {
        let r = resolve("\\$NAME/x", &[]).unwrap();
        assert_eq!(r, PathBuf::from("$NAME/x"));
    }

    #[test]
    fn user_tilde_expansion() {
        let r = resolve_u("~bob/stuff", &[], &[("bob", "/home/bob")]).unwrap();
        assert_eq!(r, PathBuf::from("/home/bob/stuff"));
    }

    #[test]
    fn unknown_user_tilde_left_literal() {
        let r = resolve_u("~nobody/stuff", &[], &[]).unwrap();
        assert_eq!(r, PathBuf::from("~nobody/stuff"));
    }

    #[test]
    fn plain_relative_path_unchanged() {
        let r = resolve("relative/path", &[]).unwrap();
        assert_eq!(r, PathBuf::from("relative/path"));
    }

    #[test]
    fn spaces_in_path_preserved() {
        let r = resolve("/path/with spaces/file", &[]).unwrap();
        assert_eq!(r, PathBuf::from("/path/with spaces/file"));
    }
}
