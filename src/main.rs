use rill::diagnostic::RillError;
use std::env;
use std::fs;
use std::io::IsTerminal;
use std::path::Path;
use std::process::ExitCode;

fn main() -> ExitCode {
    match run_cli() {
        Ok(code) => code,
        Err(err) => {
            if let Some(code) = err.exit_code {
                return exit_code(code);
            }
            if err.message.starts_with("Usage: ") {
                println!("{}", err.message);
                return ExitCode::SUCCESS;
            }
            eprintln!("{:?}", miette::Report::new(err));
            ExitCode::from(1)
        }
    }
}

fn exit_code(code: i32) -> ExitCode {
    ExitCode::from(code.clamp(0, 255) as u8)
}

fn run_cli() -> Result<ExitCode, RillError> {
    let mut argv = env::args().skip(1).collect::<Vec<_>>();
    if argv.is_empty() {
        eprintln!("usage: rill <script.rill> [args...]");
        eprintln!("       rill -e <source>");
        eprintln!("       rill check <script.rill>");
        return Ok(ExitCode::from(2));
    }

    let mode = argv.remove(0);
    match mode.as_str() {
        "-e" => {
            let source =
                argv.first().ok_or_else(|| RillError::new("missing source after -e"))?.clone();
            let script_args = argv.into_iter().skip(1).collect::<Vec<_>>();
            let mut session = rill::CompilationSession::new();
            let program = session.compile(rill::SourceUnit {
                name: "<eval>".to_string(),
                source: source.clone(),
                base_dir: std::env::current_dir().unwrap_or_else(|_| Path::new(".").to_path_buf()),
            })?;
            let input = script_input(&program, &script_args)
                .map_err(|err| err.with_source("<eval>", source.clone()))?;
            let value = rill::run_compiled(&program, script_args, input)?;
            if let Some(err) = rill::unhandled_error(&value) {
                return Err(err.with_source("<eval>", source));
            }
            Ok(ExitCode::SUCCESS)
        }
        "check" => {
            let path =
                argv.first().ok_or_else(|| RillError::new("missing script path after check"))?;
            let source = fs::read_to_string(path)
                .map_err(|err| RillError::new(format!("failed to read '{path}': {err}")))?;
            let mut session = rill::CompilationSession::new();
            session.compile(rill::SourceUnit {
                name: path.clone(),
                source,
                base_dir: Path::new(path).parent().unwrap_or(Path::new(".")).to_path_buf(),
            })?;
            println!("{path}: ok");
            Ok(ExitCode::SUCCESS)
        }
        path => {
            let source = fs::read_to_string(path)
                .map_err(|err| RillError::new(format!("failed to read '{path}': {err}")))?;
            let mut session = rill::CompilationSession::new();
            let program = session.compile(rill::SourceUnit {
                name: path.to_string(),
                source: source.clone(),
                base_dir: Path::new(path).parent().unwrap_or(Path::new(".")).to_path_buf(),
            })?;
            let input = script_input(&program, &argv)
                .map_err(|err| err.with_source(path, source.clone()))?;
            let value = rill::run_compiled(&program, argv, input)?;
            if let Some(err) = rill::unhandled_error(&value) {
                return Err(err.with_source(path, source));
            }
            Ok(ExitCode::SUCCESS)
        }
    }
}

fn input_for_args(args: &[String]) -> Result<Option<String>, RillError> {
    if args.is_empty() {
        if std::io::stdin().is_terminal() {
            return Ok(None);
        }
        let mut input = String::new();
        std::io::Read::read_to_string(&mut std::io::stdin(), &mut input)
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
) -> Result<Option<String>, RillError> {
    if program.args.is_some() && args.iter().any(|arg| arg == "--help" || arg == "-h") {
        return Ok(None);
    }
    if !program.requirements.needs_input {
        return Ok(None);
    }
    input_for_args(if program.args.is_some() { &[] } else { args })
}
