//! Terminal colour policy shared by diagnostics and `pretty_print`.

use std::io::IsTerminal;
use std::sync::OnceLock;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ColorChoice {
    /// Colour when stdout is a terminal and `NO_COLOR` is unset.
    #[default]
    Auto,
    Always,
    Never,
}

impl ColorChoice {
    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "auto" => Some(Self::Auto),
            "always" => Some(Self::Always),
            "never" => Some(Self::Never),
            _ => None,
        }
    }
}

static COLOR: OnceLock<bool> = OnceLock::new();

/// Decide the colour policy once, before any output.  Later calls are ignored.
pub fn init(choice: ColorChoice) {
    let _ = COLOR.set(resolve(choice));
}

/// Whether output may use ANSI colour.  Defaults to `Auto` when `init` was
/// never called (embedding, tests).
pub fn color_enabled() -> bool {
    *COLOR.get_or_init(|| resolve(ColorChoice::Auto))
}

fn resolve(choice: ColorChoice) -> bool {
    match choice {
        ColorChoice::Always => true,
        ColorChoice::Never => false,
        ColorChoice::Auto => {
            if std::env::var_os("NO_COLOR").is_some_and(|value| !value.is_empty()) {
                return false;
            }
            if std::env::var_os("CLICOLOR_FORCE").is_some_and(|value| value != "0") {
                return true;
            }
            std::io::stdout().is_terminal()
        }
    }
}
