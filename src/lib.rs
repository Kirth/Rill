pub mod ast;
pub mod diagnostic;
pub mod eval;
mod eval_collections;
mod eval_data;
pub mod methods;
pub mod parser;
pub mod pathresolver;
pub mod runtime;
pub mod typeck;
pub mod types;

use diagnostic::RillError;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use types::ErrorKind;

#[derive(Debug, Clone)]
pub struct SourceUnit {
    pub name: String,
    pub source: String,
    pub base_dir: PathBuf,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProgramRequirements {
    pub needs_input: bool,
}

#[derive(Debug, Clone)]
pub struct CompiledProgram {
    program: ast::Program,
    modules: Vec<CompiledModule>,
    pub requirements: ProgramRequirements,
    pub args: Option<ast::ArgGroup>,
    pub source: SourceUnit,
}

#[derive(Debug, Clone)]
struct CompiledModule {
    source: SourceUnit,
    program: ast::Program,
}

#[derive(Debug, Clone)]
struct CachedModule {
    exports: typeck::ModuleExports,
}

#[derive(Debug, Default)]
pub struct CompilationSession {
    cache: BTreeMap<PathBuf, CachedModule>,
    loading: Vec<PathBuf>,
    order: Vec<CompiledModule>,
}

impl CompilationSession {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn compile(&mut self, unit: SourceUnit) -> Result<CompiledProgram, RillError> {
        self.cache.clear();
        self.loading.clear();
        self.order.clear();

        let program = parse_source(&unit)?;
        let root_path = root_path(&unit);
        if let Some(path) = &root_path {
            self.begin_loading(path)?;
        }
        let result = (|| {
            let imports = self
                .compile_imports(&program, &unit.base_dir)
                .map_err(|err| attach_source(err, &unit))?;
            let import_refs = imports.iter().collect::<Vec<_>>();
            typeck::check_module(&program, &import_refs).map_err(|err| {
                err.with_kind(ErrorKind::Type).with_source(unit.name.clone(), unit.source.clone())
            })?;
            Ok(program)
        })();
        if root_path.is_some() {
            self.loading.pop();
        }
        let program = result?;
        let requirements = ProgramRequirements {
            needs_input: program_needs_input(&program.stmts)
                || self.order.iter().any(|module| program_needs_input(&module.program.stmts)),
        };
        let args = program.stmts.iter().find_map(|stmt| match stmt.unspanned() {
            ast::Stmt::Args(group) => Some(group.clone()),
            _ => None,
        });
        Ok(CompiledProgram {
            program,
            modules: self.order.clone(),
            requirements,
            args,
            source: unit,
        })
    }

    fn compile_imports(
        &mut self,
        program: &ast::Program,
        base_dir: &Path,
    ) -> Result<Vec<typeck::ModuleExports>, RillError> {
        let mut paths = Vec::new();
        collect_local_imports(&program.stmts, &mut paths);
        let mut seen = BTreeSet::new();
        let mut exports = Vec::new();
        for import in paths {
            let path = resolve_module_path(base_dir, &import)?;
            if seen.insert(path.clone()) {
                exports.push(self.compile_module(&path)?);
            }
        }
        Ok(exports)
    }

    fn compile_module(&mut self, path: &Path) -> Result<typeck::ModuleExports, RillError> {
        let path = canonical_module_path(path)?;
        if let Some(module) = self.cache.get(&path) {
            return Ok(module.exports.clone());
        }
        self.begin_loading(&path)?;
        let result = (|| {
            let source = fs::read_to_string(&path).map_err(|err| {
                RillError::new(format!("failed to load module '{}': {err}", path.display()))
                    .with_kind(ErrorKind::Io)
            })?;
            let source_unit = SourceUnit {
                name: path.display().to_string(),
                source,
                base_dir: path.parent().unwrap_or(Path::new(".")).to_path_buf(),
            };
            let program = parse_source(&source_unit)?;
            let imports = self
                .compile_imports(&program, &source_unit.base_dir)
                .map_err(|err| attach_source(err, &source_unit))?;
            let import_refs = imports.iter().collect::<Vec<_>>();
            let exports = typeck::check_module(&program, &import_refs).map_err(|err| {
                err.with_kind(ErrorKind::Type)
                    .with_source(source_unit.name.clone(), source_unit.source.clone())
            })?;
            let compiled = CompiledModule { source: source_unit, program };
            self.order.push(compiled.clone());
            self.cache.insert(path.clone(), CachedModule { exports: exports.clone() });
            Ok(exports)
        })();
        self.loading.pop();
        result
    }

    fn begin_loading(&mut self, path: &Path) -> Result<(), RillError> {
        if let Some(index) = self.loading.iter().position(|loaded| loaded == path) {
            let mut chain = self.loading[index..]
                .iter()
                .map(|path| path.display().to_string())
                .collect::<Vec<_>>();
            chain.push(path.display().to_string());
            return Err(RillError::new(format!("cyclic module import: {}", chain.join(" -> ")))
                .with_kind(ErrorKind::Type));
        }
        self.loading.push(path.to_path_buf());
        Ok(())
    }
}

fn parse_source(unit: &SourceUnit) -> Result<ast::Program, RillError> {
    parser::parse(&unit.source).map_err(|err| {
        err.with_kind(ErrorKind::Parse).with_source(unit.name.clone(), unit.source.clone())
    })
}

fn root_path(unit: &SourceUnit) -> Option<PathBuf> {
    let path = Path::new(&unit.name);
    path.is_file().then(|| path.to_path_buf()).and_then(|path| path.canonicalize().ok())
}

fn canonical_module_path(path: &Path) -> Result<PathBuf, RillError> {
    path.canonicalize().map_err(|err| {
        RillError::new(format!("failed to resolve module '{}': {err}", path.display()))
            .with_kind(ErrorKind::Io)
    })
}

fn resolve_module_path(base_dir: &Path, import: &str) -> Result<PathBuf, RillError> {
    canonical_module_path(&base_dir.join(import))
}

fn collect_local_imports(stmts: &[ast::Stmt], imports: &mut Vec<String>) {
    for stmt in stmts {
        match stmt.unspanned() {
            ast::Stmt::Located { stmt, .. } => {
                collect_local_imports(std::slice::from_ref(stmt), imports)
            }
            ast::Stmt::Use(module) if is_local_module(module) => imports.push(module.clone()),
            ast::Stmt::Let { expr, .. }
            | ast::Stmt::Expr(expr)
            | ast::Stmt::Emit(expr)
            | ast::Stmt::Assign { expr, .. }
            | ast::Stmt::AssignEnv { expr, .. }
            | ast::Stmt::Return(Some(expr)) => collect_expr_imports(expr, imports),
            ast::Stmt::IndexAssign { index, expr, .. } => {
                collect_expr_imports(index, imports);
                collect_expr_imports(expr, imports);
            }
            ast::Stmt::If { cond, then_body, else_body, .. } => {
                collect_expr_imports(cond, imports);
                collect_local_imports(then_body, imports);
                collect_local_imports(else_body, imports);
            }
            ast::Stmt::While { cond, body } => {
                collect_expr_imports(cond, imports);
                collect_local_imports(body, imports);
            }
            ast::Stmt::WithEnv { bindings, body } => {
                for (_, expr) in bindings {
                    collect_expr_imports(expr, imports);
                }
                collect_local_imports(body, imports);
            }
            ast::Stmt::On { host, body } => {
                collect_expr_imports(host, imports);
                collect_local_imports(body, imports);
            }
            ast::Stmt::Fn { function, .. } => collect_local_imports(&function.body, imports),
            ast::Stmt::Args(group) => collect_arg_group_imports(group, imports),
            ast::Stmt::Return(None)
            | ast::Stmt::Use(_)
            | ast::Stmt::InputLines
            | ast::Stmt::Continue
            | ast::Stmt::Break => {}
        }
    }
}

fn collect_arg_group_imports(group: &ast::ArgGroup, imports: &mut Vec<String>) {
    for field in &group.fields {
        if let Some(default) = &field.default {
            collect_expr_imports(default, imports);
        }
    }
    for child in &group.subcommands {
        collect_arg_group_imports(child, imports);
    }
}

fn collect_expr_imports(expr: &ast::Expr, imports: &mut Vec<String>) {
    match expr.unspanned() {
        ast::Expr::List(items) | ast::Expr::Tuple(items) => {
            for item in items {
                collect_expr_imports(item, imports);
            }
        }
        ast::Expr::Map(items) => {
            for (_, value) in items {
                collect_expr_imports(value, imports);
            }
        }
        ast::Expr::Call { callee, args } => {
            collect_expr_imports(callee, imports);
            for arg in args {
                collect_expr_imports(arg, imports);
            }
        }
        ast::Expr::Field { base, .. } => collect_expr_imports(base, imports),
        ast::Expr::Binary { left, right, .. } | ast::Expr::Pipe { left, right, .. } => {
            collect_expr_imports(left, imports);
            collect_expr_imports(right, imports);
        }
        ast::Expr::Command(command) => {
            for arg in &command.argv {
                collect_command_arg_imports(arg, imports);
            }
            for redirect in &command.redirects {
                collect_command_arg_imports(&redirect.target, imports);
            }
            for (_, value) in &command.config {
                collect_expr_imports(value, imports);
            }
        }
        ast::Expr::RegexMatch { value, .. }
        | ast::Expr::RegexSubstitution { value, .. }
        | ast::Expr::Unary { operand: value, .. }
        | ast::Expr::Try(value)
        | ast::Expr::NamedArg { value, .. } => collect_expr_imports(value, imports),
        ast::Expr::Index { base, index } => {
            collect_expr_imports(base, imports);
            collect_expr_imports(index, imports);
        }
        ast::Expr::Match { value, arms } => {
            collect_expr_imports(value, imports);
            for arm in arms {
                collect_expr_imports(&arm.expr, imports);
            }
        }
        ast::Expr::For { iter, body, .. } => {
            collect_expr_imports(iter, imports);
            collect_local_imports(body, imports);
        }
        ast::Expr::Block(body) | ast::Expr::Closure { body, .. } => {
            collect_local_imports(body, imports);
        }
        ast::Expr::Located { expr, .. } => collect_expr_imports(expr, imports),
        ast::Expr::Literal(_)
        | ast::Expr::Ident(_)
        | ast::Expr::Implicit
        | ast::Expr::ImplicitField(_) => {}
    }
}

fn collect_command_arg_imports(arg: &ast::CommandArg, imports: &mut Vec<String>) {
    if let ast::CommandArg::Expr(expr) = arg {
        collect_expr_imports(expr, imports);
    }
}

fn is_local_module(module: &str) -> bool {
    module.starts_with("./") || module.starts_with("../")
}

pub fn compile(source: &str) -> Result<CompiledProgram, RillError> {
    CompilationSession::new().compile(SourceUnit {
        name: "<eval>".to_string(),
        source: source.to_string(),
        base_dir: std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
    })
}

pub fn cli_help(group: &ast::ArgGroup) -> String {
    cli_help_named(group, "rill")
}

fn cli_help_named(group: &ast::ArgGroup, usage: &str) -> String {
    let mut help = format!("Usage: {usage} [options] [arguments]\n\nOptions:\n");
    for field in &group.fields {
        let short = field.short.as_ref().map(|value| format!("-{}, ", value)).unwrap_or_default();
        let default = field.default.as_ref().map(|_| " [default]").unwrap_or("");
        help.push_str(&format!(
            "  {}--{}{} {}\n",
            short,
            field.name,
            default,
            field.description.as_deref().unwrap_or("")
        ));
    }
    if !group.subcommands.is_empty() {
        help.push_str("\nSubcommands:\n");
        for child in &group.subcommands {
            help.push_str(&format!("  {}\n", child.name));
            append_group_summary(&mut help, child, 4);
        }
    }
    help.push_str("  -h, --help\n");
    help
}

fn append_group_summary(help: &mut String, group: &ast::ArgGroup, indent: usize) {
    let padding = " ".repeat(indent);
    for field in &group.fields {
        help.push_str(&format!(
            "{padding}--{}{} {}\n",
            field.name,
            if field.default.is_some() { " [default]" } else { "" },
            field.description.as_deref().unwrap_or("")
        ));
    }
    for child in &group.subcommands {
        help.push_str(&format!("{padding}{}\n", child.name));
        append_group_summary(help, child, indent + 2);
    }
}

pub fn run_source(source: &str, args: Vec<String>) -> Result<types::Value, RillError> {
    run_source_with_input(source, args, None)
}

pub fn run_source_with_input(
    source: &str,
    args: Vec<String>,
    input: Option<String>,
) -> Result<types::Value, RillError> {
    let program = compile(source)?;
    run_compiled(&program, args, input)
}

/// Run a source string with an explicit process environment instead of the
/// host's.  Useful for embedding and for deterministic tests.
pub fn run_source_with_environment(
    source: &str,
    args: Vec<String>,
    input: Option<String>,
    environment: BTreeMap<String, String>,
) -> Result<types::Value, RillError> {
    let program = compile(source)?;
    run_compiled_with_environment(&program, args, input, environment)
}

pub fn run_compiled(
    program: &CompiledProgram,
    args: Vec<String>,
    input: Option<String>,
) -> Result<types::Value, RillError> {
    run_compiled_with_environment(program, args, input, eval::process_environment())
}

pub fn run_compiled_with_environment(
    program: &CompiledProgram,
    args: Vec<String>,
    input: Option<String>,
    environment: BTreeMap<String, String>,
) -> Result<types::Value, RillError> {
    // The tree-walking evaluator recurses once per nested Rill call, so deep
    // (but legitimate) recursion needs far more stack than the main thread
    // offers.  The reservation is virtual; pages are only touched when used.
    // Smaller reservations are tried when the platform refuses the large one.
    std::thread::scope(|scope| {
        for stack_size in [eval::EVAL_STACK_SIZE, 256 << 20, 64 << 20] {
            let (args, input, environment) = (args.clone(), input.clone(), environment.clone());
            let job = move || execute_program(program, args, input, environment);
            let spawned = std::thread::Builder::new()
                .name("rill-main".to_string())
                .stack_size(stack_size)
                .spawn_scoped(scope, job);
            if let Ok(handle) = spawned {
                return match handle.join() {
                    Ok(result) => result,
                    Err(payload) => std::panic::resume_unwind(payload),
                };
            }
        }
        Err(RillError::new("failed to start the evaluator thread"))
    })
}

fn execute_program(
    program: &CompiledProgram,
    args: Vec<String>,
    input: Option<String>,
    environment: BTreeMap<String, String>,
) -> Result<types::Value, RillError> {
    let mut evaluator = eval::Evaluator::with_environment(args, input, environment);
    for module in &program.modules {
        evaluator
            .eval_module_program(&module.program)
            .map_err(|error| attach_source(error, &module.source))?;
    }
    evaluator.eval_program(&program.program).map_err(|error| attach_source(error, &program.source))
}

fn attach_source(error: RillError, source: &SourceUnit) -> RillError {
    if error.source.is_some() {
        error
    } else {
        error.with_source(source.name.clone(), source.source.clone())
    }
}

fn program_needs_input(stmts: &[ast::Stmt]) -> bool {
    stmts.iter().any(stmt_needs_input)
}

fn stmt_needs_input(stmt: &ast::Stmt) -> bool {
    match stmt.unspanned() {
        ast::Stmt::InputLines => true,
        ast::Stmt::Let { expr, .. }
        | ast::Stmt::Expr(expr)
        | ast::Stmt::Emit(expr)
        | ast::Stmt::Assign { expr, .. }
        | ast::Stmt::AssignEnv { expr, .. } => expr_needs_input(expr),
        ast::Stmt::IndexAssign { index, expr, .. } => {
            expr_needs_input(index) || expr_needs_input(expr)
        }
        ast::Stmt::If { cond, then_body, else_body, .. } => {
            expr_needs_input(cond)
                || program_needs_input(then_body)
                || program_needs_input(else_body)
        }
        ast::Stmt::While { cond, body } => expr_needs_input(cond) || program_needs_input(body),
        ast::Stmt::WithEnv { bindings, body } => {
            bindings.iter().any(|(_, expr)| expr_needs_input(expr)) || program_needs_input(body)
        }
        ast::Stmt::On { host, body } => expr_needs_input(host) || program_needs_input(body),
        ast::Stmt::Fn { function, .. } => program_needs_input(&function.body),
        ast::Stmt::Return(expr) => expr.as_ref().is_some_and(expr_needs_input),
        _ => false,
    }
}

fn expr_needs_input(expr: &ast::Expr) -> bool {
    match expr.unspanned() {
        ast::Expr::Ident(name) => name == "input",
        ast::Expr::Literal(ast::Literal::Str(text)) => text.contains("input"),
        ast::Expr::List(items) | ast::Expr::Tuple(items) => items.iter().any(expr_needs_input),
        ast::Expr::Map(items) => items.iter().any(|(_, value)| expr_needs_input(value)),
        ast::Expr::Call { callee, args } => {
            expr_needs_input(callee) || args.iter().any(expr_needs_input)
        }
        ast::Expr::Field { base, .. } => expr_needs_input(base),
        ast::Expr::Binary { left, right, .. } | ast::Expr::Pipe { left, right, .. } => {
            expr_needs_input(left) || expr_needs_input(right)
        }
        ast::Expr::Command(command) => {
            command.argv.iter().any(command_arg_needs_input)
                || command
                    .redirects
                    .iter()
                    .any(|redirect| command_arg_needs_input(&redirect.target))
                || command.config.iter().any(|(_, value)| expr_needs_input(value))
        }
        ast::Expr::RegexMatch { value, .. } | ast::Expr::RegexSubstitution { value, .. } => {
            expr_needs_input(value)
        }
        ast::Expr::Match { value, arms } => {
            expr_needs_input(value) || arms.iter().any(|arm| expr_needs_input(&arm.expr))
        }
        ast::Expr::NamedArg { value, .. }
        | ast::Expr::Try(value)
        | ast::Expr::Unary { operand: value, .. } => expr_needs_input(value),
        ast::Expr::For { iter, body, .. } => expr_needs_input(iter) || program_needs_input(body),
        ast::Expr::Index { base, index } => expr_needs_input(base) || expr_needs_input(index),
        ast::Expr::Block(body) | ast::Expr::Closure { body, .. } => program_needs_input(body),
        _ => false,
    }
}

fn command_arg_needs_input(arg: &ast::CommandArg) -> bool {
    match arg {
        ast::CommandArg::Expr(expr) => expr_needs_input(expr),
        ast::CommandArg::Parts(parts) => parts.iter().any(|part| matches!(part, ast::CommandPart::Var(name) | ast::CommandPart::VarDefault(name, _) if name == "input")),
        _ => false,
    }
}

pub fn unhandled_error(value: &types::Value) -> Option<RillError> {
    match value {
        types::Value::Result(Err(error)) => match &**error {
            types::Value::Error(error) => Some(RillError::from_error_value(error)),
            other => Some(
                RillError::new(format!("runtime error: unhandled error: {other}"))
                    .with_kind(ErrorKind::Runtime),
            ),
        },
        _ => None,
    }
}
