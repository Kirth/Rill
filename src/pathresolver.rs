use crate::diagnostic::RillError;
use std::path::PathBuf;

/// Resolves a path written as source text: a leading `~` or `~/` becomes
/// `lookup("HOME")` and `~user` becomes that user's home directory.  Variable
/// references are not expanded here; string interpolation already did that at
/// parse time, so values are never expanded twice.  An unresolvable tilde
/// expression is left literal.
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
            None | Some((_, '/')) => match lookup("HOME") {
                Some(home) if !home.is_empty() => result.push_str(&home),
                _ => result.push('~'),
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

    // Everything after the tilde prefix is literal.
    result.extend(chars.map(|(_, ch)| ch));

    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct FakeUsers(Vec<(&'static str, &'static str)>);
    impl UserLookup for FakeUsers {
        fn home_for(&self, username: &str) -> Option<PathBuf> {
            self.0.iter().find(|(u, _)| *u == username).map(|(_, h)| PathBuf::from(h))
        }
    }

    fn env_lookup<'a>(vars: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
        move |name: &str| vars.iter().find(|(k, _)| *k == name).map(|(_, v)| v.to_string())
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
        // Tilde expansion reads HOME through the lookup, never from std::env.
        let home = env_lookup(&[("HOME", "/home/testuser")]);
        let r = resolve_with_users("~", &home, &FakeUsers(vec![])).unwrap();
        assert_eq!(r, PathBuf::from("/home/testuser"));
        let r2 = resolve_with_users("~/data", &home, &FakeUsers(vec![])).unwrap();
        assert_eq!(r2, PathBuf::from("/home/testuser/data"));
        let r3 = resolve_with_users("~/data", &env_lookup(&[]), &FakeUsers(vec![])).unwrap();
        assert_eq!(r3, PathBuf::from("~/data"));
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
