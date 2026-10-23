use rill::diagnostic::RillError;
use rill::term::ColorChoice;
use std::fs;
use std::io::{IsTerminal, Read};
use std::path::Path;
use std::process::ExitCode;

const USAGE: &str = "\
Usage: rill [OPTIONS] <script.ril> [ARGS...]
       rill [OPTIONS] -e <SOURCE> [ARGS...]
       rill [OPTIONS] - [ARGS...]            read the script from stdin
       rill run <script.ril> [ARGS...]
       rill check [OPTIONS] <script.ril>...   parse and type-check without running

Options:
  -e, --eval <SOURCE>     run SOURCE instead of a file
      --color <WHEN>      auto, always, or never (default auto; NO_COLOR is honoured)
      --no-color          same as --color never
  -q, --quiet             check: only print diagnostics
      --json              check: print diagnostics as JSON lines
  -W, --deny-warnings     check: fail when a warning is reported
  -h, --help              print this help
  -V, --version           print the version

Everything after the script (or after the -e source) is passed to the script as
its arguments, including options such as --help.

Exit status: 0 on success, 1 for any script error, 2 for a usage error, 70 for
an internal error, or the code the script passed to exit().";

#[derive(Debug, Default)]
struct Options {
    color: ColorChoice,
    quiet: bool,
    json: bool,
    deny_warnings: bool,
}

#[derive(Debug)]
enum ScriptSource {
    File(String),
    Stdin,
    Inline(String),
}

#[derive(Debug)]
enum Command {
    Run { source: ScriptSource, args: Vec<String> },
    Check { paths: Vec<String> },
    Help,
    Version,
}

fn main() -> ExitCode {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let (options, command) = match parse_cli(argv) {
        Ok(parsed) => parsed,
        Err(message) => {
            eprintln!("rill: {message}\n\n{USAGE}");
            return ExitCode::from(2);
        }
    };
    rill::term::init(options.color);
    install_panic_hook();
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| dispatch(options, command))) {
        Ok(code) => code,
        Err(_) => ExitCode::from(70),
    }
}

fn install_panic_hook() {
    std::panic::set_hook(Box::new(|info| {
        let message = info
            .payload()
            .downcast_ref::<&str>()
            .map(|s| s.to_string())
            .or_else(|| info.payload().downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "unknown panic".to_string());
        let location = info
            .location()
            .map(|location| format!(" at {}:{}", location.file(), location.line()))
            .unwrap_or_default();
        let error = RillError::new(format!("internal error: {message}{location}"))
            .with_code("rill::internal")
            .with_help(
                "this is a bug in rill; please report it at https://github.com/Kirth/Rill/issues \
                 (run with RUST_BACKTRACE=1 for a backtrace)",
            );
        eprint!("{}", rill::diagnostic::render(&error, rill::term::color_enabled(), 100));
    }));
}

fn parse_cli(argv: Vec<String>) -> Result<(Options, Command), String> {
    let mut options = Options::default();
    let mut rest = argv.into_iter().peekable();
    let mut options_done = false;
    while let Some(arg) = rest.next() {
        if !options_done && arg.starts_with('-') && arg != "-" {
            match arg.as_str() {
                "-h" | "--help" => return Ok((options, Command::Help)),
                "-V" | "--version" => return Ok((options, Command::Version)),
                "-e" | "--eval" => {
                    let source = rest.next().ok_or("missing source after -e")?;
                    return Ok((
                        options,
                        Command::Run { source: ScriptSource::Inline(source), args: rest.collect() },
                    ));
                }
                "--color" => {
                    let value = rest.next().ok_or("--color needs auto, always, or never")?;
                    options.color = ColorChoice::parse(&value)
                        .ok_or_else(|| format!("unknown --color value '{value}'"))?;
                }
                "--no-color" => options.color = ColorChoice::Never,
                "-q" | "--quiet" => options.quiet = true,
                "--json" => options.json = true,
                "-W" | "--deny-warnings" => options.deny_warnings = true,
                "--" => options_done = true,
                other => {
                    if let Some(value) = other.strip_prefix("--eval=") {
                        return Ok((
                            options,
                            Command::Run {
                                source: ScriptSource::Inline(value.to_string()),
                                args: rest.collect(),
                            },
                        ));
                    }
                    if let Some(value) = other.strip_prefix("--color=") {
                        options.color = ColorChoice::parse(value)
                            .ok_or_else(|| format!("unknown --color value '{value}'"))?;
                        continue;
                    }
                    return Err(format!("unknown option '{other}'"));
                }
            }
            continue;
        }
        let command = match arg.as_str() {
            "check" => {
                let mut paths = Vec::new();
                for path in rest {
                    match path.as_str() {
                        "-q" | "--quiet" => options.quiet = true,
                        "--json" => options.json = true,
                        "-W" | "--deny-warnings" => options.deny_warnings = true,
                        "--no-color" => options.color = ColorChoice::Never,
                        "-h" | "--help" => return Ok((options, Command::Help)),
                        _ => paths.push(path),
                    }
                }
                if paths.is_empty() {
                    return Err("check needs at least one script path".to_string());
                }
                Command::Check { paths }
            }
            "run" => {
                let script = rest.next().ok_or("run needs a script path")?;
                let source =
                    if script == "-" { ScriptSource::Stdin } else { ScriptSource::File(script) };
                Command::Run { source, args: rest.collect() }
            }
            "-" => Command::Run { source: ScriptSource::Stdin, args: rest.collect() },
            _ => Command::Run { source: ScriptSource::File(arg), args: rest.collect() },
        };
        return Ok((options, command));
    }
    Err("no script given".to_string())
}

fn dispatch(options: Options, command: Command) -> ExitCode {
    match command {
        Command::Help => {
            println!("{USAGE}");
            ExitCode::SUCCESS
        }
        Command::Version => {
            match option_env!("RILL_GIT_HASH") {
                Some(hash) => println!("rill {} ({hash})", env!("CARGO_PKG_VERSION")),
                None => println!("rill {}", env!("CARGO_PKG_VERSION")),
            }
            ExitCode::SUCCESS
        }
        Command::Check { paths } => check(&options, &paths),
        Command::Run { source, args } => match run(source, args) {
            Ok(code) => code,
            Err(error) => report_failure(&options, error),
        },
    }
}

fn report_failure(options: &Options, error: RillError) -> ExitCode {
    if let Some(code) = error.exit_code {
        return exit_code(code);
    }
    if error.message.starts_with("Usage: ") {
        // Generated `args {}` help requested with --help.
        println!("{}", error.message);
        return ExitCode::SUCCESS;
    }
    if options.json {
        println!("{}", diagnostic_json(&error));
    } else {
        eprint!("{}", rill::diagnostic::render(&error, rill::term::color_enabled(), 100));
    }
    if error.code_name() == "rill::usage" {
        ExitCode::from(2)
    } else {
        ExitCode::from(1)
    }
}

fn exit_code(code: i32) -> ExitCode {
    ExitCode::from(code.clamp(0, 255) as u8)
}

fn load_source(source: &ScriptSource) -> Result<rill::SourceUnit, RillError> {
    let cwd = std::env::current_dir().unwrap_or_else(|_| Path::new(".").to_path_buf());
    match source {
        ScriptSource::Inline(text) => {
            Ok(rill::SourceUnit { name: "<eval>".to_string(), source: text.clone(), base_dir: cwd })
        }
        ScriptSource::Stdin => {
            let mut text = String::new();
            std::io::stdin()
                .read_to_string(&mut text)
                .map_err(|err| RillError::new(format!("failed to read stdin: {err}")))?;
            Ok(rill::SourceUnit { name: "<stdin>".to_string(), source: text, base_dir: cwd })
        }
        ScriptSource::File(path) => {
            let text = fs::read_to_string(path).map_err(|err| {
                RillError::new(format!("failed to read '{path}': {err}"))
                    .with_kind(rill::types::ErrorKind::Io)
            })?;
            Ok(rill::SourceUnit {
                name: path.clone(),
                source: text,
                base_dir: Path::new(path).parent().unwrap_or(Path::new(".")).to_path_buf(),
            })
        }
    }
}

fn run(source: ScriptSource, args: Vec<String>) -> Result<ExitCode, RillError> {
    let unit = load_source(&source)?;
    let (name, text) = (unit.name.clone(), unit.source.clone());
    let program = rill::CompilationSession::new().compile(unit)?;
    let input = script_input(&program, &args, matches!(source, ScriptSource::Stdin))
        .map_err(|err| err.with_source(name.clone(), text.clone()))?;
    let value = rill::run_compiled(&program, args, input)?;
    if let Some(err) = rill::unhandled_error(&value) {
        return Err(err.with_source(name, text));
    }
    Ok(ExitCode::SUCCESS)
}

fn check(options: &Options, paths: &[String]) -> ExitCode {
    let mut failed = false;
    for path in paths {
        let result = load_source(&ScriptSource::File(path.clone()))
            .and_then(|unit| rill::CompilationSession::new().compile(unit));
        match result {
            Ok(_) => {
                if options.json {
                    println!("{}", serde_json::json!({"file": path, "ok": true}));
                } else if !options.quiet {
                    println!("{path}: ok");
                }
            }
            Err(error) => {
                failed = true;
                if options.json {
                    println!("{}", diagnostic_json(&error));
                } else {
                    eprint!(
                        "{}",
                        rill::diagnostic::render(&error, rill::term::color_enabled(), 100)
                    );
                }
            }
        }
    }
    if failed {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    }
}

fn diagnostic_json(error: &RillError) -> serde_json::Value {
    let (line, col) = error.location().unwrap_or((0, 0));
    let (end_line, end_col) = error.end_location().unwrap_or((line, col));
    serde_json::json!({
        "file": error.source_name().unwrap_or(""),
        "line": line,
        "col": col,
        "end_line": end_line,
        "end_col": end_col,
        "severity": match error.severity {
            rill::diagnostic::Severity::Error => "error",
            rill::diagnostic::Severity::Warning => "warning",
        },
        "code": error.code_name(),
        "message": error.message,
        "help": error.help_text(),
    })
}

fn input_for_args(args: &[String], stdin_is_script: bool) -> Result<Option<String>, RillError> {
    if args.is_empty() {
        if stdin_is_script || std::io::stdin().is_terminal() {
            return Ok(None);
        }
        let mut input = String::new();
        std::io::stdin()
            .read_to_string(&mut input)
            .map(|_| Some(input))
            .map_err(|err| RillError::new(format!("failed to read stdin: {err}")))
    } else {
        let mut input = String::new();
        for path in args {
            let contents = fs::read_to_string(path)
                .map_err(|err| RillError::new(format!("failed to read input '{path}': {err}")))?;
            input.push_str(&contents);
            if !contents.ends_with('\n') {
                input.push('\n');
            }
        }
        Ok(Some(input))
    }
}

fn script_input(
    program: &rill::CompiledProgram,
    args: &[String],
    stdin_is_script: bool,
) -> Result<Option<String>, RillError> {
    if program.args.is_some() && args.iter().any(|arg| arg == "--help" || arg == "-h") {
        return Ok(None);
    }
    if !program.requirements.needs_input {
        return Ok(None);
    }
    input_for_args(if program.args.is_some() { &[] } else { args }, stdin_is_script)
}
