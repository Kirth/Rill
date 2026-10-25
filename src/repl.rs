//! An interactive session: one evaluator and one checker whose global scope
//! persists from line to line.

use crate::diagnostic::RillError;
use crate::types::Value;
use std::io::{BufRead, IsTerminal, Write};

/// What the REPL should do with the line it was fed.
#[derive(Debug)]
pub enum Response {
    /// Print nothing (a statement, or a `null` result).
    Silent,
    /// Print this value.
    Value(Value),
    /// Print this text as-is.
    Text(String),
    /// Report this diagnostic.
    Error(RillError),
    /// The input is incomplete; read another line and feed it again.
    Continue,
    Quit,
}

pub struct Repl {
    evaluator: crate::eval::Evaluator,
    session: crate::typeck::Session,
    /// Lines of an input that is not complete yet.
    pending: String,
    line_number: usize,
}

impl Repl {
    pub fn new(args: Vec<String>) -> Self {
        Self {
            evaluator: crate::eval::Evaluator::new(args),
            session: crate::typeck::Session::new(),
            pending: String::new(),
            line_number: 0,
        }
    }

    pub fn is_continuing(&self) -> bool {
        !self.pending.is_empty()
    }

    /// Feed one line of input.
    pub fn feed(&mut self, line: &str) -> Response {
        if !self.is_continuing() {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                return Response::Silent;
            }
            if let Some(command) = trimmed.strip_prefix(':') {
                return self.command(command.trim());
            }
        }
        self.pending.push_str(line);
        self.pending.push('\n');
        let source = self.pending.clone();
        self.line_number += 1;
        let name = format!("<repl:{}>", self.line_number);
        let program = match crate::parser::parse(&source) {
            Ok(program) => program,
            Err(error) if is_incomplete(&error, &source) => return Response::Continue,
            Err(error) => {
                self.pending.clear();
                return Response::Error(error.with_source(name, source));
            }
        };
        self.pending.clear();
        if let Err(error) = self.session.check(&program) {
            return Response::Error(error.with_source(name, source));
        }
        match self.evaluator.eval_program(&program) {
            Ok(Value::Null) => Response::Silent,
            Ok(value) => match crate::unhandled_error(&value) {
                Some(error) => Response::Error(error.with_source(name, source)),
                None => Response::Value(value),
            },
            Err(error) => Response::Error(error.with_source(name, source)),
        }
    }

    fn command(&mut self, command: &str) -> Response {
        let (verb, rest) = command.split_once(' ').unwrap_or((command, ""));
        match verb {
            "q" | "quit" | "exit" => Response::Quit,
            "help" | "h" | "?" => Response::Text(HELP.to_string()),
            "env" => Response::Text(self.evaluator.binding_names().join("\n")),
            "type" | "t" => {
                let source = rest.trim();
                if source.is_empty() {
                    return Response::Text("usage: :type <expression>".to_string());
                }
                let program = match crate::parser::parse(source) {
                    Ok(program) => program,
                    Err(error) => {
                        return Response::Error(error.with_source("<repl>", source.to_string()))
                    }
                };
                let expr = program.stmts.last().and_then(|stmt| match stmt.unspanned() {
                    crate::ast::Stmt::Expr(expr) => Some(expr.clone()),
                    _ => None,
                });
                let Some(expr) = expr else {
                    return Response::Text(":type needs an expression".to_string());
                };
                match self.session.type_of(&expr) {
                    Ok(ty) => Response::Text(ty),
                    Err(error) => Response::Error(error.with_source("<repl>", source.to_string())),
                }
            }
            other => Response::Text(format!("unknown command ':{other}'; try :help")),
        }
    }
}

const HELP: &str = "\
Enter Rill statements; the value of an expression is printed.
Bindings and functions persist for the whole session.
  :type <expr>   show the static type of an expression
  :env           list the names in scope
  :help          this text
  :quit          leave (also Ctrl-D)";

/// A parse error at the end of the input means the user is in the middle of
/// a block, string or expression: keep reading.
pub fn is_incomplete(error: &RillError, source: &str) -> bool {
    let at_end = error.span.is_some_and(|span| span.start >= source.trim_end().len());
    let message = &error.message;
    (at_end && message.contains("end of file"))
        || message.contains("end of file")
        || message.contains("never closed")
        || message.contains("unterminated string")
}

/// Run the interactive loop over stdin until EOF or `:quit`.
pub fn run(args: Vec<String>) -> i32 {
    let interactive = std::io::stdin().is_terminal();
    let mut repl = Repl::new(args);
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout();
    if interactive {
        println!("rill {} (type :help for help, :quit to leave)", env!("CARGO_PKG_VERSION"));
    }
    let color = crate::term::color_enabled();
    loop {
        if interactive {
            let prompt = if repl.is_continuing() { "...  " } else { "rill> " };
            print!("{prompt}");
            let _ = stdout.flush();
        }
        let mut line = String::new();
        match stdin.lock().read_line(&mut line) {
            Ok(0) => {
                if interactive {
                    println!();
                }
                return 0;
            }
            Ok(_) => {}
            Err(_) => return 1,
        }
        let line = line.strip_suffix('\n').unwrap_or(&line).to_string();
        match repl.feed(&line) {
            Response::Silent | Response::Continue => {}
            Response::Value(value) => {
                if color {
                    println!("{}\x1b[0m", crate::eval::pretty_value(&value, 0));
                } else {
                    println!("{}", crate::eval::pretty_value(&value, 0));
                }
            }
            Response::Text(text) => println!("{text}"),
            Response::Error(error) => {
                eprint!("{}", crate::diagnostic::render(&error, color, 100));
            }
            Response::Quit => return 0,
        }
    }
}
