use crate::ast::*;
use crate::diagnostic::RillError;
use crate::eval_data::{json_to_value, value_to_json};
use crate::runtime::{
    run_command, run_command_cancellable, run_pipeline, run_pipeline_cancellable,
    ssh_control_options, CommandIo, ProcessRuntime, ProcessSpec, StreamMode, TokioProcessRuntime,
};
use crate::types::{Binding, ErrorKind, ErrorValue, FunctionValue, Map, Scope, Slot, Value};
use rayon::prelude::*;
use std::collections::BTreeMap;
use std::fs;
use std::num::NonZeroUsize;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};
use tokio_util::sync::CancellationToken;

#[derive(Clone)]
pub struct ParallelExecutor {
    pool: Arc<rayon::ThreadPool>,
}

impl ParallelExecutor {
    pub fn new(parallelism: NonZeroUsize) -> Result<Self, RillError> {
        rayon::ThreadPoolBuilder::new()
            .num_threads(parallelism.get())
            .thread_name(|index| format!("rill-worker-{index}"))
            .build()
            .map(|pool| Self { pool: Arc::new(pool) })
            .map_err(|error| {
                RillError::new(format!("failed to create parallel executor: {error}"))
                    .with_kind(ErrorKind::Runtime)
            })
    }

    fn shared() -> Self {
        static EXECUTOR: OnceLock<ParallelExecutor> = OnceLock::new();
        EXECUTOR
            .get_or_init(|| {
                let parallelism = std::thread::available_parallelism().unwrap_or(NonZeroUsize::MIN);
                Self::new(parallelism).expect("failed to create shared parallel executor")
            })
            .clone()
    }

    pub fn parallelism(&self) -> usize {
        self.pool.current_num_threads()
    }
}

/// The host process environment as the evaluator's initial environment map.
pub fn process_environment() -> BTreeMap<String, String> {
    std::env::vars_os()
        .map(|(name, value)| {
            (name.to_string_lossy().into_owned(), value.to_string_lossy().into_owned())
        })
        .collect()
}

#[derive(Debug, Clone)]
struct Env {
    scopes: Vec<Arc<Scope>>,
    /// Scopes below this index belong to an enclosing evaluator.  A parallel
    /// worker may read them but must not assign into them.
    frozen: usize,
}

impl Env {
    fn new(args: Vec<String>, input: Option<String>) -> Self {
        let mut root = Scope::new();
        root.insert(
            "args".to_string(),
            Binding::cell(Value::List(args.into_iter().map(Value::Str).collect())),
        );
        let lines =
            input.unwrap_or_default().lines().map(|line| Value::Str(line.to_string())).collect();
        root.insert(
            "input".to_string(),
            Binding::cell(Value::Map(Map::from_iter([("lines".to_string(), Value::List(lines))]))),
        );
        Self { scopes: vec![Arc::new(root)], frozen: 0 }
    }

    fn push(&mut self) {
        self.scopes.push(Arc::new(Scope::new()));
    }

    fn pop(&mut self) {
        self.scopes.pop();
    }

    fn current(&mut self) -> &mut Scope {
        Arc::make_mut(self.scopes.last_mut().expect("env always has root scope"))
    }

    /// Declare a mutable binding in the innermost scope.
    fn set(&mut self, name: String, value: Value) {
        self.current().insert(name, Binding::cell(value));
    }

    fn bind(&mut self, name: String, value: Value, mutable: bool) {
        let binding = if mutable { Binding::cell(value) } else { Binding::value(value) };
        self.current().insert(name, binding);
    }

    /// Reserve a slot for a function declared later in the current block.
    fn hoist(&mut self, name: &str) {
        self.current()
            .insert(name.to_string(), Binding { slot: Slot::Hoisted(Arc::new(OnceLock::new())) });
    }

    /// Define a function in the current scope.  `make` receives the scope
    /// chain the function closes over, which already contains the function's
    /// own slot so that recursion works.
    fn define_function(
        &mut self,
        name: &str,
        make: impl FnOnce(Vec<Arc<Scope>>) -> Arc<FunctionValue>,
    ) {
        let lock = match self.scopes.last().and_then(|scope| scope.get(name)) {
            Some(Binding { slot: Slot::Hoisted(lock) }) if lock.get().is_none() => lock.clone(),
            _ => {
                let lock = Arc::new(OnceLock::new());
                self.current()
                    .insert(name.to_string(), Binding { slot: Slot::Hoisted(lock.clone()) });
                lock
            }
        };
        let function = make(self.scopes.clone());
        let _ = lock.set(function);
    }

    fn find(&self, name: &str) -> Option<(usize, &Binding)> {
        self.scopes
            .iter()
            .enumerate()
            .rev()
            .find_map(|(index, scope)| scope.get(name).map(|binding| (index, binding)))
    }

    /// Look a name up, distinguishing "unknown" from "declared below".
    fn lookup(&self, name: &str) -> Result<Option<Value>, RillError> {
        let Some((_, binding)) = self.find(name) else {
            return Ok(None);
        };
        Ok(Some(match &binding.slot {
            Slot::Value(value) => value.clone(),
            Slot::Cell(cell) => cell.read().unwrap_or_else(|error| error.into_inner()).clone(),
            Slot::Hoisted(lock) => match lock.get() {
                Some(function) => Value::Function(function.clone()),
                None => {
                    return Err(RillError::new(format!(
                        "function '{name}' is used before its definition"
                    )))
                }
            },
        }))
    }

    fn get(&self, name: &str) -> Option<Value> {
        self.lookup(name).ok().flatten()
    }

    /// Mutate a `var` binding in place.  Returns `Ok(false)` when the name is
    /// not declared at all.
    fn update(
        &mut self,
        name: &str,
        mutate: impl FnOnce(&mut Value) -> Result<(), RillError>,
    ) -> Result<bool, RillError> {
        let Some((index, binding)) = self.find(name) else {
            return Ok(false);
        };
        let cell = match &binding.slot {
            Slot::Cell(cell) => cell.clone(),
            Slot::Value(_) => {
                return Err(RillError::new(format!(
                    "cannot assign to immutable variable '{name}' (declared with 'let'; use 'var' for mutable variables)"
                )))
            }
            Slot::Hoisted(_) => {
                return Err(RillError::new(format!("cannot assign to function '{name}'")))
            }
        };
        if index < self.frozen {
            return Err(RillError::new(format!(
                "cannot assign to outer variable '{name}' from a parallel body; collect results with emit or parallel_map"
            )));
        }
        let mut guard = cell.write().unwrap_or_else(|error| error.into_inner());
        mutate(&mut guard)?;
        Ok(true)
    }
}

#[derive(Debug)]
enum Flow {
    Value(Value),
    Return(Value),
    Propagate(Value),
    Break,
    Continue,
    Emit(Value),
}

#[derive(Debug)]
enum EvalError {
    Runtime(RillError),
    Control(Flow),
    Cancelled,
}

#[derive(Debug)]
enum ParallelTerminal {
    Runtime(RillError),
    Control(Flow),
}

#[derive(Debug)]
enum RaceOutcome {
    Value(Value),
    Terminal(ParallelTerminal),
}

struct RaceState {
    outcome: Mutex<Option<RaceOutcome>>,
    cancellation: CancellationToken,
}

impl RaceState {
    fn finish(&self, outcome: RaceOutcome) -> bool {
        let mut slot = self.outcome.lock().unwrap_or_else(|error| error.into_inner());
        if slot.is_some() {
            return false;
        }
        *slot = Some(outcome);
        self.cancellation.cancel();
        true
    }
}

fn is_truthy(value: &Value) -> Result<bool, EvalError> {
    match value {
        Value::Bool(b) => Ok(*b),
        Value::Null => Ok(false),
        other => Err(RillError::new(format!("expected Bool, got {}", other.type_name())).into()),
    }
}

impl From<RillError> for EvalError {
    fn from(error: RillError) -> Self {
        Self::Runtime(error)
    }
}

impl EvalError {
    fn diagnostic(self) -> RillError {
        match self {
            Self::Runtime(error) => error,
            Self::Control(_) => RillError::new("control flow escaped its evaluation boundary"),
            Self::Cancelled => RillError::new("evaluation cancelled").with_kind(ErrorKind::Runtime),
        }
    }
}

#[derive(Clone)]
pub struct Evaluator {
    env: Env,
    /// The process environment seen by `env.NAME`, `set_env`, `with env`, and
    /// child processes.  Evaluator-local; the host process is never mutated.
    environment: BTreeMap<String, String>,
    function_depth: usize,
    remote_prefix: Vec<Vec<String>>,
    executor: ParallelExecutor,
    cancellation: CancellationToken,
    cancellation_active: bool,
}

impl Evaluator {
    fn bind_pattern(&mut self, pattern: &LetPattern, value: Value) -> Result<(), EvalError> {
        match pattern {
            LetPattern::Ident(name) => self.env.set(name.clone(), value),
            LetPattern::List(names) => {
                let items = match value {
                    Value::List(v) => v,
                    other => {
                        return Err(RillError::new(format!(
                            "list destructuring requires a list, got {}",
                            other.type_name()
                        ))
                        .into())
                    }
                };
                for (i, name) in names.iter().enumerate() {
                    if name != "_" {
                        self.env.set(name.clone(), items.get(i).cloned().unwrap_or(Value::Null));
                    }
                }
            }
            LetPattern::Tuple(names) => {
                let items = match value {
                    Value::Tuple(items) => items,
                    other => {
                        return Err(RillError::new(format!(
                            "tuple destructuring requires a Tuple, got {}",
                            other.type_name()
                        ))
                        .into())
                    }
                };
                if items.len() != names.len() {
                    return Err(RillError::new(format!(
                        "tuple destructuring needs {} bindings, got {}",
                        items.len(),
                        names.len()
                    ))
                    .into());
                }
                for (name, item) in names.iter().zip(items) {
                    if name != "_" {
                        self.env.set(name.clone(), item);
                    }
                }
            }
            LetPattern::Map(pairs) => {
                let map = match value {
                    Value::Map(m) => m,
                    other => {
                        return Err(RillError::new(format!(
                            "map destructuring requires a map, got {}",
                            other.type_name()
                        ))
                        .into())
                    }
                };
                for (key, alias) in pairs {
                    let binding = alias.as_deref().unwrap_or(key.as_str());
                    self.env.set(binding.to_string(), map.get(key).cloned().unwrap_or(Value::Null));
                }
            }
        }
        Ok(())
    }

    fn matching_bindings(
        pattern: &LetPattern,
        value: Value,
    ) -> Result<Option<Vec<(String, Value)>>, EvalError> {
        let mut bindings = Vec::new();
        match pattern {
            LetPattern::Ident(name) => bindings.push((name.clone(), value)),
            LetPattern::List(names) => {
                let Value::List(items) = value else { return Ok(None) };
                for (i, name) in names.iter().enumerate() {
                    if name != "_" {
                        bindings.push((name.clone(), items.get(i).cloned().unwrap_or(Value::Null)));
                    }
                }
            }
            LetPattern::Tuple(names) => {
                let Value::Tuple(items) = value else { return Ok(None) };
                if items.len() != names.len() {
                    return Ok(None);
                }
                for (name, item) in names.iter().zip(items) {
                    if name != "_" {
                        bindings.push((name.clone(), item));
                    }
                }
            }
            LetPattern::Map(pairs) => {
                let Value::Map(map) = value else { return Ok(None) };
                for (key, alias) in pairs {
                    let binding = alias.as_deref().unwrap_or(key.as_str());
                    bindings
                        .push((binding.to_string(), map.get(key).cloned().unwrap_or(Value::Null)));
                }
            }
        }
        Ok(Some(bindings))
    }

    pub fn new(args: Vec<String>) -> Self {
        Self::with_input(args, None)
    }

    pub fn with_input(args: Vec<String>, input: Option<String>) -> Self {
        Self::with_environment(args, input, process_environment())
    }

    pub fn with_environment(
        args: Vec<String>,
        input: Option<String>,
        environment: BTreeMap<String, String>,
    ) -> Self {
        Self::with_executor(args, input, environment, ParallelExecutor::shared())
    }

    pub fn with_executor(
        args: Vec<String>,
        input: Option<String>,
        environment: BTreeMap<String, String>,
        executor: ParallelExecutor,
    ) -> Self {
        Self {
            env: Env::new(args, input),
            environment,
            function_depth: 0,
            remote_prefix: Vec::new(),
            executor,
            cancellation: CancellationToken::new(),
            cancellation_active: false,
        }
    }

    /// A child evaluator for a parallel worker.  It shares every binding
    /// visible at the fork point but may not assign into them.
    fn fork(&self, cancellation: CancellationToken) -> Self {
        let mut env = self.env.clone();
        env.frozen = env.scopes.len();
        Self {
            env,
            environment: self.environment.clone(),
            function_depth: self.function_depth,
            remote_prefix: self.remote_prefix.clone(),
            executor: self.executor.clone(),
            cancellation,
            cancellation_active: true,
        }
    }

    /// Run `body` in a fresh scope that is popped even when `body` fails.
    fn scoped<T>(
        &mut self,
        body: impl FnOnce(&mut Self) -> Result<T, EvalError>,
    ) -> Result<T, EvalError> {
        self.env.push();
        let result = body(self);
        self.env.pop();
        result
    }

    fn make_function(&self, function: Function, name: Option<String>) -> Value {
        Value::Function(Arc::new(FunctionValue {
            function: Arc::new(function),
            captured: self.env.scopes.clone(),
            name,
        }))
    }

    fn environment_value(&self) -> Value {
        Value::Map(
            self.environment
                .iter()
                .map(|(name, value)| (name.clone(), Value::Str(value.clone())))
                .collect(),
        )
    }

    fn check_cancelled(&self) -> Result<(), EvalError> {
        if self.cancellation.is_cancelled() {
            Err(EvalError::Cancelled)
        } else {
            Ok(())
        }
    }

    fn sleep_cancellable(&self, duration: std::time::Duration) -> Result<(), EvalError> {
        let deadline = std::time::Instant::now() + duration;
        loop {
            self.check_cancelled()?;
            let remaining = deadline.saturating_duration_since(std::time::Instant::now());
            if remaining.is_zero() {
                return Ok(());
            }
            std::thread::sleep(remaining.min(std::time::Duration::from_millis(5)));
        }
    }

    fn executor_for_parallelism(
        &mut self,
        workers: Option<&Expr>,
    ) -> Result<ParallelExecutor, EvalError> {
        let Some(workers) = workers else {
            return Ok(self.executor.clone());
        };
        let value = self.eval_expr(workers)?;
        let Value::Int(count) = value else {
            return Err(RillError::new("parallel worker count needs Int").into());
        };
        let count = usize::try_from(count)
            .ok()
            .and_then(NonZeroUsize::new)
            .ok_or_else(|| RillError::new("parallel worker count must be greater than zero"))?;
        ParallelExecutor::new(count).map_err(EvalError::Runtime)
    }

    fn execute_command(
        &self,
        argv: &[String],
        input: Option<Vec<u8>>,
        io: &CommandIo,
    ) -> Result<crate::types::ProcessResult, RillError> {
        let io = self.command_io_with_environment(io.clone())?;
        if self.cancellation_active {
            run_command_cancellable(argv, input, &io, &self.cancellation)
        } else {
            run_command(argv, input, &io)
        }
    }

    fn execute_pipeline(
        &self,
        argv: &[Vec<String>],
        io: &[CommandIo],
    ) -> Result<crate::types::ProcessResult, RillError> {
        let io = io
            .iter()
            .cloned()
            .map(|config| self.command_io_with_environment(config))
            .collect::<Result<Vec<_>, _>>()?;
        if self.cancellation_active {
            run_pipeline_cancellable(argv, &io, &self.cancellation)
        } else {
            run_pipeline(argv, &io)
        }
    }

    fn lookup_environment_value(&self, name: &str) -> Option<Value> {
        self.env
            .get(name)
            .or_else(|| self.environment.get(name).map(|value| Value::Str(value.clone())))
    }

    fn command_io_with_environment(&self, mut io: CommandIo) -> Result<CommandIo, RillError> {
        let explicit = std::mem::take(&mut io.env);
        io.env = self.environment.clone();
        io.env.extend(explicit);
        Ok(io)
    }

    fn apply_remote_prefix(&self, argv: Vec<String>) -> Vec<String> {
        if self.remote_prefix.is_empty() {
            return argv;
        }
        let mut result: Vec<String> = self.remote_prefix.iter().flatten().cloned().collect();
        result.extend(argv);
        result
    }

    /// Resolve a path template using Rill bindings first, then process env.
    fn eval_resolve_path(&self, template: &str) -> Result<PathBuf, RillError> {
        crate::pathresolver::resolve_path(template, &|name| {
            self.lookup_environment_value(name).map(|v| v.as_string_lossy())
        })
    }

    pub fn eval_program(&mut self, program: &Program) -> Result<Value, RillError> {
        if let Some(group) = program.stmts.iter().find_map(|stmt| match stmt {
            Stmt::Args(group) => Some(group),
            _ => None,
        }) {
            self.configure_args(group).map_err(EvalError::diagnostic)?;
        }
        match self.eval_stmts(&program.stmts).map_err(EvalError::diagnostic)? {
            Flow::Value(value) | Flow::Return(value) => Ok(value),
            Flow::Propagate(value) => Err(RillError::new(format!(
                "cannot propagate {} outside a function",
                value.type_name()
            ))),
            Flow::Break => Err(RillError::new("break outside loop")),
            Flow::Continue => Err(RillError::new("continue outside loop")),
            Flow::Emit(_) => Err(RillError::new("emit outside for expression")),
        }
    }

    pub(crate) fn eval_module_program(&mut self, program: &Program) -> Result<(), RillError> {
        match self.eval_stmts(&program.stmts).map_err(EvalError::diagnostic)? {
            Flow::Value(_) | Flow::Return(_) => Ok(()),
            Flow::Propagate(value) => Err(RillError::new(format!(
                "cannot propagate {} outside a function",
                value.type_name()
            ))),
            Flow::Break => Err(RillError::new("break outside loop")),
            Flow::Continue => Err(RillError::new("continue outside loop")),
            Flow::Emit(_) => Err(RillError::new("emit outside for expression")),
        }
    }

    /// Reserve slots for every `fn` in `stmts` so that functions can call
    /// each other regardless of declaration order.
    fn hoist_functions(&mut self, stmts: &[Stmt]) {
        for stmt in stmts {
            if let Stmt::Fn { name, .. } = stmt {
                self.env.hoist(name);
            }
        }
    }

    fn eval_stmts(&mut self, stmts: &[Stmt]) -> Result<Flow, EvalError> {
        self.hoist_functions(stmts);
        let mut last = Value::Null;
        for stmt in stmts {
            let flow = match self.eval_stmt(stmt) {
                Ok(flow) => flow,
                Err(EvalError::Control(flow)) => return Ok(flow),
                Err(error) => return Err(error),
            };
            match flow {
                Flow::Value(value) => last = value,
                flow => return Ok(flow),
            }
        }
        Ok(Flow::Value(last))
    }

    fn eval_stmts_collecting_emits(
        &mut self,
        stmts: &[Stmt],
    ) -> Result<(Flow, Vec<Value>), EvalError> {
        self.hoist_functions(stmts);
        let mut last = Value::Null;
        let mut emits = Vec::new();
        for stmt in stmts {
            let flow = match self.eval_stmt(stmt) {
                Ok(flow) => flow,
                Err(EvalError::Control(Flow::Emit(value))) => {
                    emits.push(value);
                    continue;
                }
                Err(EvalError::Control(flow)) => return Ok((flow, emits)),
                Err(e) => return Err(e),
            };
            match flow {
                Flow::Value(v) => last = v,
                Flow::Emit(v) => emits.push(v),
                flow => return Ok((flow, emits)),
            }
        }
        Ok((Flow::Value(last), emits))
    }

    fn eval_for_parallel(
        &mut self,
        workers: Option<&Expr>,
        pattern: &LetPattern,
        items: Vec<Value>,
        body: &[Stmt],
    ) -> Result<Value, EvalError> {
        let cancellation = self.cancellation.child_token();
        let terminal = Arc::new(Mutex::new(None::<ParallelTerminal>));
        let executor = self.executor_for_parallelism(workers)?;
        let mut template = self.fork(cancellation.clone());
        template.executor = executor.clone();
        let results = executor.pool.install(|| {
            items
                .into_par_iter()
                .map(|item| {
                    if cancellation.is_cancelled() {
                        return None;
                    }
                    let mut child = template.clone();
                    child.env.push();
                    if let Err(error) = child.bind_pattern(pattern, item) {
                        record_parallel_terminal(
                            &terminal,
                            &cancellation,
                            ParallelTerminal::Runtime(match error {
                                EvalError::Runtime(error) => error,
                                other => RillError::new(format!("{other:?}")),
                            }),
                        );
                        return None;
                    }
                    match catch_parallel_job(|| child.eval_stmts_collecting_emits(body)) {
                        Ok((Flow::Value(_), emits))
                        | Ok((Flow::Break, emits))
                        | Ok((Flow::Continue, emits)) => Some(emits),
                        Ok((flow @ (Flow::Return(_) | Flow::Propagate(_)), _)) => {
                            record_parallel_terminal(
                                &terminal,
                                &cancellation,
                                ParallelTerminal::Control(flow),
                            );
                            None
                        }
                        Ok((Flow::Emit(_), _)) => unreachable!(),
                        Err(EvalError::Cancelled) => None,
                        Err(EvalError::Runtime(error)) => {
                            record_parallel_terminal(
                                &terminal,
                                &cancellation,
                                ParallelTerminal::Runtime(error),
                            );
                            None
                        }
                        Err(EvalError::Control(flow)) => {
                            record_parallel_terminal(
                                &terminal,
                                &cancellation,
                                ParallelTerminal::Control(flow),
                            );
                            None
                        }
                    }
                })
                .collect::<Vec<_>>()
        });
        if let Some(outcome) = terminal.lock().unwrap_or_else(|error| error.into_inner()).take() {
            return Err(parallel_terminal_error(outcome));
        }
        Ok(Value::List(results.into_iter().flatten().flatten().collect()))
    }

    fn eval_for_race(
        &mut self,
        workers: Option<&Expr>,
        pattern: &LetPattern,
        items: Vec<Value>,
        body: &[Stmt],
    ) -> Result<Value, EvalError> {
        let cancellation = self.cancellation.child_token();
        let state =
            Arc::new(RaceState { outcome: Mutex::new(None), cancellation: cancellation.clone() });
        let executor = self.executor_for_parallelism(workers)?;
        let mut template = self.fork(cancellation.clone());
        template.executor = executor.clone();
        executor.pool.install(|| {
            items.into_par_iter().for_each(|item| {
                if cancellation.is_cancelled() {
                    return;
                }
                let mut child = template.clone();
                child.env.push();
                if let Err(error) = child.bind_pattern(pattern, item) {
                    state.finish(RaceOutcome::Terminal(ParallelTerminal::Runtime(match error {
                        EvalError::Runtime(error) => error,
                        other => RillError::new(format!("{other:?}")),
                    })));
                    return;
                }
                if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    child.eval_stmts_for_race(body, &state);
                }))
                .is_err()
                {
                    state.finish(RaceOutcome::Terminal(ParallelTerminal::Runtime(
                        RillError::new("race for: iteration panicked")
                            .with_kind(ErrorKind::Runtime),
                    )));
                }
            });
        });
        let outcome = state.outcome.lock().unwrap_or_else(|error| error.into_inner()).take();
        match outcome {
            Some(RaceOutcome::Value(value)) => Ok(value),
            Some(RaceOutcome::Terminal(outcome)) => Err(parallel_terminal_error(outcome)),
            None => Ok(Value::Null),
        }
    }

    fn eval_stmts_for_race(&mut self, stmts: &[Stmt], state: &RaceState) {
        self.hoist_functions(stmts);
        for stmt in stmts {
            if self.cancellation.is_cancelled() {
                return;
            }
            match self.eval_stmt(stmt) {
                Ok(Flow::Value(_)) => {}
                Ok(Flow::Break) | Ok(Flow::Continue) => return,
                Ok(Flow::Emit(value)) => {
                    state.finish(RaceOutcome::Value(value));
                    return;
                }
                Ok(flow @ (Flow::Return(_) | Flow::Propagate(_))) => {
                    state.finish(RaceOutcome::Terminal(ParallelTerminal::Control(flow)));
                    return;
                }
                Err(EvalError::Cancelled) => return,
                Err(EvalError::Runtime(error)) => {
                    state.finish(RaceOutcome::Terminal(ParallelTerminal::Runtime(error)));
                    return;
                }
                Err(EvalError::Control(Flow::Emit(value))) => {
                    state.finish(RaceOutcome::Value(value));
                    return;
                }
                Err(EvalError::Control(flow)) => {
                    state.finish(RaceOutcome::Terminal(ParallelTerminal::Control(flow)));
                    return;
                }
            }
        }
    }

    fn eval_stmt(&mut self, stmt: &Stmt) -> Result<Flow, EvalError> {
        self.check_cancelled()?;
        match stmt {
            Stmt::Use(module) => {
                let _ = module;
                Ok(Flow::Value(Value::Null))
            }
            Stmt::Let { pattern, expr, mutable, .. } => {
                let value = self.eval_expr(expr)?;
                match pattern {
                    LetPattern::Ident(name) => self.env.bind(name.clone(), value, *mutable),
                    LetPattern::List(names) => {
                        let items = match value {
                            Value::List(v) => v,
                            other => {
                                return Err(RillError::new(format!(
                                    "list destructuring requires a list, got {}",
                                    other.type_name()
                                ))
                                .into())
                            }
                        };
                        for (i, name) in names.iter().enumerate() {
                            if name == "_" {
                                continue;
                            }
                            self.env.bind(
                                name.clone(),
                                items.get(i).cloned().unwrap_or(Value::Null),
                                *mutable,
                            );
                        }
                    }
                    LetPattern::Tuple(names) => {
                        let items = match value {
                            Value::Tuple(items) => items,
                            other => {
                                return Err(RillError::new(format!(
                                    "tuple destructuring requires a Tuple, got {}",
                                    other.type_name()
                                ))
                                .into())
                            }
                        };
                        if items.len() != names.len() {
                            return Err(RillError::new(format!(
                                "tuple destructuring needs {} bindings, got {}",
                                items.len(),
                                names.len()
                            ))
                            .into());
                        }
                        for (name, item) in names.iter().zip(items) {
                            if name != "_" {
                                self.env.bind(name.clone(), item, *mutable);
                            }
                        }
                    }
                    LetPattern::Map(pairs) => {
                        let map = match value {
                            Value::Map(m) => m,
                            other => {
                                return Err(RillError::new(format!(
                                    "map destructuring requires a map, got {}",
                                    other.type_name()
                                ))
                                .into())
                            }
                        };
                        for (key, alias) in pairs {
                            let binding = alias.as_deref().unwrap_or(key.as_str());
                            self.env.bind(
                                binding.to_string(),
                                map.get(key.as_str()).cloned().unwrap_or(Value::Null),
                                *mutable,
                            );
                        }
                    }
                }
                Ok(Flow::Value(Value::Null))
            }
            Stmt::AssignEnv { name, expr } => {
                let value = self.eval_expr(expr)?.as_string_lossy();
                self.set_environment(name, value)?;
                Ok(Flow::Value(Value::Null))
            }
            Stmt::Assign { name, op, expr } => {
                let rhs = self.eval_expr(expr)?;
                let op = *op;
                if !self.env.update(name, |current| apply_compound(current, rhs, op))? {
                    return Err(EvalError::Runtime(RillError::new(format!(
                        "assignment to undeclared variable '{name}'"
                    ))));
                }
                Ok(Flow::Value(Value::Null))
            }
            Stmt::IndexAssign { name, index, op, expr } => {
                let idx_val = self.eval_expr(index)?;
                let rhs = self.eval_expr(expr)?;
                let literal_index = constant_int_expr(index);
                let op = *op;
                let updated = self.env.update(name, |collection| {
                    index_assign_in_place(collection, idx_val, literal_index, op, rhs)
                })?;
                if !updated {
                    return Err(EvalError::Runtime(RillError::new(format!(
                        "assignment to undeclared variable '{name}'"
                    ))));
                }
                Ok(Flow::Value(Value::Null))
            }
            Stmt::Args(_) | Stmt::InputLines => Ok(Flow::Value(Value::Null)),
            Stmt::Fn { name, function } => {
                let function = Arc::new(function.clone());
                self.env.define_function(name, |captured| {
                    Arc::new(FunctionValue { function, captured, name: Some(name.clone()) })
                });
                Ok(Flow::Value(Value::Null))
            }
            Stmt::Emit(expr) => {
                let value = self.eval_expr(expr)?;
                Ok(Flow::Emit(value))
            }
            Stmt::If { cond, pattern, then_body, else_body } => {
                if let Some(pattern) = pattern {
                    let value = self.eval_expr(cond)?;
                    let bindings = Self::matching_bindings(pattern, value)?;
                    self.env.push();
                    let flow = if let Some(bindings) = bindings {
                        for (name, value) in bindings {
                            self.env.set(name, value);
                        }
                        self.eval_stmts(then_body)
                    } else {
                        self.eval_stmts(else_body)
                    };
                    self.env.pop();
                    flow
                } else if let Expr::RegexMatch { value, pattern } = cond.unspanned() {
                    let input = self.eval_expr(value)?.as_string_lossy();
                    let captures = regex_captures(&input, pattern)?;
                    if let Some(captures) = captures {
                        self.env.push();
                        for (name, value) in captures {
                            self.env.set(name, Value::Str(value));
                        }
                        let flow = self.eval_stmts(then_body);
                        self.env.pop();
                        flow
                    } else {
                        self.env.push();
                        let flow = self.eval_stmts(else_body);
                        self.env.pop();
                        flow
                    }
                } else {
                    let cond = self.eval_expr(cond)?;
                    self.env.push();
                    let flow = if cond.truthy() {
                        self.eval_stmts(then_body)
                    } else {
                        self.eval_stmts(else_body)
                    };
                    self.env.pop();
                    flow
                }
            }
            Stmt::WithEnv { bindings, body } => {
                let values = bindings
                    .iter()
                    .map(|(name, expr)| Ok((name.clone(), self.eval_expr(expr)?.as_string_lossy())))
                    .collect::<Result<Vec<_>, EvalError>>()?;
                let saved = self.environment.clone();
                for (name, value) in values {
                    self.environment.insert(name, value);
                }
                let result = self.scoped(|evaluator| evaluator.eval_stmts(body));
                self.environment = saved;
                result
            }
            Stmt::On { host, body } => {
                let host_val = self.eval_expr(host)?;
                let prefix = match host_val {
                    Value::Str(h) => {
                        let mut prefix = vec!["ssh".to_string()];
                        prefix.extend(ssh_control_options()?);
                        prefix.push(h);
                        prefix.push("--".to_string());
                        prefix
                    }
                    Value::List(items) => items.into_iter().map(|v| v.as_string_lossy()).collect(),
                    other => {
                        return Err(RillError::new(format!(
                            "on needs Str or List[Str], got {}",
                            other.type_name()
                        ))
                        .into())
                    }
                };
                self.remote_prefix.push(prefix);
                let result = self.eval_stmts(body);
                self.remote_prefix.pop();
                result
            }
            Stmt::Return(expr) => {
                let value = match expr {
                    Some(expr) => self.eval_expr(expr)?,
                    None => Value::Null,
                };
                Ok(Flow::Return(value))
            }
            Stmt::Continue => Ok(Flow::Continue),
            Stmt::Break => Ok(Flow::Break),
            Stmt::While { cond, body } => {
                loop {
                    if !self.eval_expr(cond)?.truthy() {
                        break;
                    }
                    match self.eval_stmts(body)? {
                        Flow::Break => break,
                        Flow::Continue | Flow::Value(_) => {}
                        flow => return Err(EvalError::Control(flow)),
                    }
                }
                Ok(Flow::Value(Value::Null))
            }
            Stmt::Expr(expr) => Ok(Flow::Value(self.eval_expr(expr)?)),
        }
    }

    fn eval_expr(&mut self, expr: &Expr) -> Result<Value, EvalError> {
        self.check_cancelled()?;
        match expr {
            Expr::Located { expr, span } => self.eval_expr(expr).map_err(|error| match error {
                EvalError::Runtime(error) => EvalError::Runtime(error.with_span(*span)),
                flow => flow,
            }),
            Expr::Literal(value) => self.eval_literal(value),
            Expr::Ident(name) => {
                if name == "none" {
                    return Ok(Value::Option(None));
                }
                if let Some(value) = self.env.lookup(name)? {
                    return Ok(value);
                }
                if name == "env" {
                    return Ok(self.environment_value());
                }
                Err(RillError::new(format!("unknown identifier '{name}'")).into())
            }
            Expr::List(items) => items
                .iter()
                .map(|item| self.eval_expr(item))
                .collect::<Result<Vec<_>, _>>()
                .map(Value::List),
            Expr::Tuple(items) => items
                .iter()
                .map(|item| self.eval_expr(item))
                .collect::<Result<Vec<_>, _>>()
                .map(Value::Tuple),
            Expr::Map(pairs) => {
                let mut map = Map::new();
                for (key, expr) in pairs {
                    map.insert(key.clone(), self.eval_expr(expr)?);
                }
                Ok(Value::Map(map))
            }
            Expr::Call { callee, args } => self.eval_call(callee, args),
            Expr::Index { base, index } => {
                let base_val = self.eval_expr(base)?;
                let idx_val = self.eval_expr(index)?;
                match base_val {
                    Value::List(items) => {
                        let idx = match idx_val {
                            Value::Int(i) => i,
                            other => {
                                return Err(RillError::new(format!(
                                    "list index must be Int, got {}",
                                    other.type_name()
                                ))
                                .into())
                            }
                        };
                        let len = items.len() as i64;
                        let i = if idx < 0 { len + idx } else { idx };
                        items.into_iter().nth(i as usize).ok_or_else(|| {
                            RillError::new(format!("index {idx} out of bounds (len {len})")).into()
                        })
                    }
                    Value::Tuple(items) => {
                        let idx = match idx_val {
                            Value::Int(index) => index,
                            other => {
                                return Err(RillError::new(format!(
                                    "tuple index must be Int, got {}",
                                    other.type_name()
                                ))
                                .into())
                            }
                        };
                        let position = normalize_runtime_index(idx, items.len(), "tuple")?;
                        Ok(items[position].clone())
                    }
                    Value::Str(s) => {
                        let idx = match idx_val {
                            Value::Int(i) => i,
                            other => {
                                return Err(RillError::new(format!(
                                    "string index must be Int, got {}",
                                    other.type_name()
                                ))
                                .into())
                            }
                        };
                        let chars: Vec<char> = s.chars().collect();
                        let len = chars.len() as i64;
                        let i = if idx < 0 { len + idx } else { idx };
                        chars.get(i as usize).map(|c| Value::Str(c.to_string())).ok_or_else(|| {
                            RillError::new(format!("index {idx} out of bounds (len {len})")).into()
                        })
                    }
                    Value::Map(map) => {
                        let key = match idx_val {
                            Value::Str(s) => s,
                            Value::Int(i) => i.to_string(),
                            other => {
                                return Err(RillError::new(format!(
                                    "map key must be Str or Int, got {}",
                                    other.type_name()
                                ))
                                .into())
                            }
                        };
                        map.get(&key)
                            .cloned()
                            .ok_or_else(|| RillError::new(format!("key '{key}' not in map")).into())
                    }
                    other => {
                        Err(RillError::new(format!("cannot index into {}", other.type_name()))
                            .into())
                    }
                }
            }
            Expr::Field { base, name } => {
                if let Expr::Ident(module) = base.unspanned() {
                    if crate::methods::is_namespace(module) && self.env.get(module).is_none() {
                        if crate::methods::namespace_method(module, name).is_some() {
                            let call = Expr::Call {
                                callee: Box::new(Expr::Field {
                                    base: Box::new(Expr::Ident(module.to_string())),
                                    name: name.clone(),
                                }),
                                args: vec![Expr::Ident("_arg".to_string())],
                            };
                            return Ok(self.make_function(
                                Function {
                                    params: vec![Param {
                                        name: "_arg".to_string(),
                                        type_name: None,
                                    }],
                                    return_type: None,
                                    body: vec![Stmt::Return(Some(call))],
                                },
                                None,
                            ));
                        }
                        return Err(
                            RillError::new(format!("{module} has no function '{name}'")).into()
                        );
                    }
                }
                self.eval_field(base, name)
            }
            Expr::Binary { op: BinaryOp::Or, left, right } => {
                let lv = self.eval_expr(left)?;
                if is_truthy(&lv)? {
                    return Ok(Value::Bool(true));
                }
                Ok(Value::Bool(is_truthy(&self.eval_expr(right)?)?))
            }
            Expr::Binary { op: BinaryOp::And, left, right } => {
                let lv = self.eval_expr(left)?;
                if !is_truthy(&lv)? {
                    return Ok(Value::Bool(false));
                }
                Ok(Value::Bool(is_truthy(&self.eval_expr(right)?)?))
            }
            Expr::Binary { op, left, right } => self.eval_binary(*op, left, right),
            Expr::Unary { op: UnaryOp::Not, operand } => {
                let v = self.eval_expr(operand)?;
                Ok(Value::Bool(!is_truthy(&v)?))
            }
            Expr::Pipe { kind, left, right } => self.eval_pipe(*kind, left, right),
            Expr::Command(command) => self.eval_command(command, None),
            Expr::RegexMatch { value, pattern } => {
                let value = self.eval_expr(value)?.as_string_lossy();
                Ok(Value::Bool(regex_is_match(&value, pattern)?))
            }
            Expr::RegexSubstitution { value, pattern, replacement, all } => {
                let value = self.eval_expr(value)?.as_string_lossy();
                Ok(regex_replace(&value, pattern, replacement, *all).map(Value::Str)?)
            }
            Expr::Match { value, arms } => self.eval_match(value, arms),
            Expr::NamedArg { value, .. } => self.eval_expr(value),
            Expr::For { mode, workers, pattern, iter, body } => {
                let iter_val = self.eval_expr(iter)?;
                if let Value::Stream(handle) = iter_val {
                    if !matches!(mode, ForMode::Sequential) {
                        return Err(
                            RillError::new("parallel/race for not supported on streams").into()
                        );
                    }
                    let mut collected: Vec<Value> = Vec::new();
                    loop {
                        match handle.next() {
                            None => break,
                            Some(item) => {
                                let result = self.scoped(|evaluator| {
                                    evaluator.bind_pattern(pattern, item)?;
                                    evaluator.eval_stmts_collecting_emits(body)
                                });
                                let (flow, emits) = result?;
                                collected.extend(emits);
                                match flow {
                                    Flow::Value(_) | Flow::Continue => {}
                                    Flow::Break => break,
                                    Flow::Return(v) => {
                                        return Err(EvalError::Control(Flow::Return(v)))
                                    }
                                    Flow::Propagate(v) => {
                                        return Err(EvalError::Control(Flow::Propagate(v)))
                                    }
                                    Flow::Emit(_) => unreachable!(),
                                }
                            }
                        }
                    }
                    return Ok(Value::List(collected));
                }
                let list = match iter_val {
                    Value::List(items) => items,
                    other => {
                        return Err(RillError::new(format!(
                            "for loop needs List, got {}",
                            other.type_name()
                        ))
                        .into())
                    }
                };
                match mode {
                    ForMode::Sequential => {
                        let mut collected: Vec<Value> = Vec::new();
                        for item in list {
                            let result = self.scoped(|evaluator| {
                                evaluator.bind_pattern(pattern, item)?;
                                evaluator.eval_stmts_collecting_emits(body)
                            });
                            let (flow, emits) = result?;
                            collected.extend(emits);
                            match flow {
                                Flow::Value(_) | Flow::Continue => {}
                                Flow::Break => break,
                                Flow::Return(v) => return Err(EvalError::Control(Flow::Return(v))),
                                Flow::Propagate(v) => {
                                    return Err(EvalError::Control(Flow::Propagate(v)))
                                }
                                Flow::Emit(_) => unreachable!(),
                            }
                        }
                        Ok(Value::List(collected))
                    }
                    ForMode::Parallel => {
                        self.eval_for_parallel(workers.as_deref(), pattern, list, body)
                    }
                    ForMode::Race => self.eval_for_race(workers.as_deref(), pattern, list, body),
                }
            }
            Expr::Block(stmts) => {
                self.env.push();
                let flow = self.eval_stmts(stmts);
                self.env.pop();
                match flow? {
                    Flow::Value(value) => Ok(value),
                    flow => Err(EvalError::Control(flow)),
                }
            }
            Expr::Closure { params, body } => Ok(self.make_function(
                Function {
                    params: params
                        .iter()
                        .map(|name| Param { name: name.clone(), type_name: None })
                        .collect(),
                    return_type: None,
                    body: body.clone(),
                },
                None,
            )),
            Expr::Try(inner) => match self.eval_expr(inner)? {
                Value::Option(Some(value)) => Ok(*value),
                Value::Option(None) => self.propagate(Value::Option(None)),
                Value::Result(Ok(value)) => Ok(*value),
                Value::Result(Err(error)) => self.propagate(Value::Result(Err(error))),
                other => Err(RillError::new(format!(
                    "? needs Option or Result, got {}",
                    other.type_name()
                ))
                .into()),
            },
            Expr::Implicit => Ok(self
                .env
                .get("it")
                .ok_or_else(|| RillError::new("implicit argument used outside a callback"))?),
            Expr::ImplicitField(name) => {
                let value = self
                    .env
                    .get("it")
                    .ok_or_else(|| RillError::new("implicit argument used outside a callback"))?;
                Ok(field_value(value, name)?)
            }
        }
    }

    fn eval_literal(&mut self, value: &Literal) -> Result<Value, EvalError> {
        match value {
            Literal::Str(text) => self.interpolate_string(text),
            Literal::Null => Ok(Value::Null),
            Literal::Bool(value) => Ok(Value::Bool(*value)),
            Literal::Int(value) => Ok(Value::Int(*value)),
            Literal::Float(value) => Ok(Value::Float(*value)),
            Literal::Duration(value) => Ok(Value::Duration(*value)),
            Literal::Regex(value) => Ok(Value::Regex(value.clone())),
        }
    }

    fn set_environment(&mut self, name: &str, value: String) -> Result<(), EvalError> {
        self.check_cancelled()?;
        self.environment.insert(name.to_string(), value);
        Ok(())
    }

    fn configure_args(&mut self, group: &ArgGroup) -> Result<(), EvalError> {
        let raw = match self.env.get("args") {
            Some(Value::List(values)) => {
                values.into_iter().map(|value| value.as_string_lossy()).collect::<Vec<_>>()
            }
            _ => Vec::new(),
        };
        let values = self.parse_arg_group(group, &raw)?;
        self.env.set("args".to_string(), Value::Map(values));
        Ok(())
    }

    fn parse_arg_group(&mut self, group: &ArgGroup, raw: &[String]) -> Result<Map, EvalError> {
        let mut values = Map::new();
        let mut positional = Vec::new();
        let mut child = None;
        let mut options = true;
        let mut index = 0;
        while index < raw.len() {
            let arg = &raw[index];
            if options && arg == "--" {
                options = false;
                index += 1;
                continue;
            }
            if options && (arg == "--help" || arg == "-h") {
                return Err(RillError::new(crate::cli_help(group)).into());
            }
            if options && arg.starts_with("--") {
                let name = &arg[2..];
                let field = group
                    .fields
                    .iter()
                    .find(|field| field.name == name)
                    .ok_or_else(|| RillError::new(format!("unknown option '--{name}'")))?;
                if matches!(field.type_name, TypeName::Bool) {
                    values.insert(field.name.clone(), Value::Bool(true));
                } else {
                    index += 1;
                    let value = raw.get(index).ok_or_else(|| {
                        RillError::new(format!("option '--{name}' needs a value"))
                    })?;
                    values.insert(
                        field.name.clone(),
                        parse_arg_value(value, &field.type_name, &|name| {
                            self.lookup_environment_value(name).map(|value| value.as_string_lossy())
                        })?,
                    );
                }
            } else if options && arg.starts_with('-') && arg != "-" {
                let short = &arg[1..];
                let field = group
                    .fields
                    .iter()
                    .find(|field| field.short.as_deref() == Some(short))
                    .ok_or_else(|| RillError::new(format!("unknown option '-{short}'")))?;
                if matches!(field.type_name, TypeName::Bool) {
                    values.insert(field.name.clone(), Value::Bool(true));
                } else {
                    index += 1;
                    let value = raw.get(index).ok_or_else(|| {
                        RillError::new(format!("option '-{short}' needs a value"))
                    })?;
                    values.insert(
                        field.name.clone(),
                        parse_arg_value(value, &field.type_name, &|name| {
                            self.lookup_environment_value(name).map(|value| value.as_string_lossy())
                        })?,
                    );
                }
            } else if options {
                let command =
                    group.subcommands.iter().find(|child| child.name == *arg).or_else(|| {
                        group.subcommands.iter().find(|wrapper| wrapper.name == "command").and_then(
                            |wrapper| wrapper.subcommands.iter().find(|child| child.name == *arg),
                        )
                    });
                if let Some(command) = command {
                    child = Some((command.clone(), index + 1));
                    break;
                }
                positional.push(arg.clone());
            } else {
                positional.push(arg.clone());
            }
            index += 1;
        }
        let mut position = 0;
        for field in &group.fields {
            if !values.contains_key(&field.name) {
                if let Some(value) = &field.default {
                    values.insert(field.name.clone(), self.eval_expr(value)?);
                } else if matches!(field.type_name, TypeName::Option(_)) {
                    values.insert(field.name.clone(), Value::Option(None));
                } else if let Some(value) = positional.get(position) {
                    values.insert(
                        field.name.clone(),
                        parse_arg_value(value, &field.type_name, &|name| {
                            self.lookup_environment_value(name).map(|value| value.as_string_lossy())
                        })?,
                    );
                    position += 1;
                } else {
                    return Err(RillError::new(format!(
                        "missing required argument '{}'",
                        field.name
                    ))
                    .into());
                }
            }
        }
        if let Some((command, next_index)) = child {
            let command_values = self.parse_arg_group(&command, &raw[next_index..])?;
            values.insert(
                "command".to_string(),
                Value::Map(Map::from_iter([(command.name.clone(), Value::Map(command_values))])),
            );
        }
        if position < positional.len() {
            return Err(RillError::new("too many positional arguments").into());
        }
        Ok(values)
    }

    fn propagate(&mut self, value: Value) -> Result<Value, EvalError> {
        if self.function_depth == 0 {
            Err(RillError::new("? used outside a function").into())
        } else {
            Err(EvalError::Control(Flow::Propagate(value)))
        }
    }

    fn interpolate_string(&mut self, text: &str) -> Result<Value, EvalError> {
        let mut out = String::new();
        let mut chars = text.char_indices().peekable();
        while let Some((idx, ch)) = chars.next() {
            match ch {
                '{' => {
                    let start = idx + ch.len_utf8();
                    let mut end = None;
                    for (next_idx, next) in chars.by_ref() {
                        if next == '}' {
                            end = Some(next_idx);
                            break;
                        }
                    }
                    let Some(end) = end else {
                        out.push_str(&text[idx..]);
                        break;
                    };
                    out.push_str(&self.eval_interpolation(text[start..end].trim())?);
                }
                '$' => {
                    if matches!(chars.peek(), Some((_, '{'))) {
                        chars.next(); // consume '{'
                                      // Collect name and optional `:-default`.
                        let mut name = String::new();
                        let mut default: Option<String> = None;
                        'braced: loop {
                            match chars.next() {
                                None | Some((_, '}')) => break 'braced,
                                Some((_, ':')) if matches!(chars.peek(), Some((_, '-'))) => {
                                    chars.next(); // consume '-'
                                    let mut d = String::new();
                                    let mut depth = 1usize;
                                    loop {
                                        match chars.next() {
                                            None => break,
                                            Some((_, '{')) => {
                                                depth += 1;
                                                d.push('{');
                                            }
                                            Some((_, '}')) => {
                                                depth -= 1;
                                                if depth == 0 {
                                                    break;
                                                }
                                                d.push('}');
                                            }
                                            Some((_, ch)) => d.push(ch),
                                        }
                                    }
                                    default = Some(d);
                                    break 'braced;
                                }
                                Some((_, ch)) => name.push(ch),
                            }
                        }
                        if name.is_empty() && default.is_none() {
                            out.push_str("${}");
                        } else if let Some(dflt) = default {
                            // Rill binding first, then evaluator environment, then default.
                            let val = self.lookup_environment_value(&name);
                            let s = val.as_ref().map(|v| v.as_string_lossy());
                            if s.as_deref().is_some_and(|v| !v.is_empty()) {
                                out.push_str(&s.unwrap());
                            } else {
                                let expanded = self.interpolate_string(&dflt)?.as_string_lossy();
                                out.push_str(&expanded);
                            }
                        } else {
                            // `${NAME}` – same as `$NAME` (with process-env fallback).
                            out.push_str(&self.eval_interpolation(&name)?);
                        }
                    } else {
                        let mut name = String::new();
                        while let Some((_, next)) = chars.peek().copied() {
                            if next.is_ascii_alphanumeric() || next == '_' {
                                chars.next();
                                name.push(next);
                            } else {
                                break;
                            }
                        }
                        if name.is_empty() {
                            out.push('$');
                        } else {
                            out.push_str(&self.eval_interpolation(&name)?);
                        }
                    }
                }
                other => out.push(other),
            }
        }
        Ok(Value::Str(out))
    }

    fn eval_interpolation(&mut self, expr: &str) -> Result<String, EvalError> {
        let mut parts = expr.split('.').collect::<Vec<_>>();
        if parts.is_empty() {
            return Ok(String::new());
        }
        let first = parts.remove(0);
        // Rill binding first, then evaluator environment.
        let mut value = self
            .lookup_environment_value(first)
            .ok_or_else(|| RillError::new(format!("unknown interpolation '{first}'")))?;
        for part in parts {
            value = if let Some(name) = part.strip_suffix("()") {
                self.env.push();
                self.env.set("$interpolation_receiver".into(), value);
                let result =
                    self.eval_method(&Expr::Ident("$interpolation_receiver".into()), name, &[]);
                self.env.pop();
                result?
            } else {
                field_value(value, part)?
            };
        }
        let text = match value {
            Value::Option(_) => value.to_string(),
            _ => value.as_string_lossy(),
        };
        Ok(text)
    }

    fn eval_replace_method(&mut self, base: &Expr, args: &[Expr]) -> Result<Value, EvalError> {
        if !(args.len() == 2 || args.len() == 3) {
            return Err(RillError::new(
                "replace needs pattern, replacement, and optional all=true",
            )
            .into());
        }
        let base = self.eval_expr(base)?.as_string_lossy();
        let pattern = match self.eval_expr(&args[0])? {
            Value::Regex(pattern) | Value::Str(pattern) => pattern,
            other => {
                return Err(RillError::new(format!(
                    "replace pattern must be Regex or Str, got {}",
                    other.type_name()
                ))
                .into())
            }
        };
        let replacement = self.eval_expr(&args[1])?.as_string_lossy();
        let mut all = false;
        if let Some(arg) = args.get(2) {
            match arg {
                Expr::NamedArg { name, value } if name == "all" => {
                    let Value::Bool(value) = self.eval_expr(value)? else {
                        return Err(RillError::new("replace all= argument must be Bool").into());
                    };
                    all = value;
                }
                Expr::NamedArg { name, .. } => {
                    return Err(RillError::new(format!("unknown replace argument '{name}'")).into());
                }
                _ => return Err(RillError::new("third replace argument must be all=true").into()),
            }
        }
        Ok(regex_replace(&base, &pattern, &replacement, all).map(Value::Str)?)
    }

    fn eval_match(&mut self, value: &Expr, arms: &[MatchArm]) -> Result<Value, EvalError> {
        let val = self.eval_expr(value)?;
        for arm in arms {
            // Try to match structurally; returns Some(Option<(name, bound_value)>) on match.
            let structural = match (&arm.pattern, &val) {
                (MatchPattern::Ok(binding), Value::Result(Ok(inner))) => {
                    Some(binding.as_deref().map(|n| (n.to_string(), *inner.clone())))
                }
                (MatchPattern::Err(binding), Value::Result(Err(inner))) => {
                    Some(binding.as_deref().map(|n| (n.to_string(), *inner.clone())))
                }
                (MatchPattern::Some(binding), Value::Option(Some(inner))) => {
                    Some(binding.as_deref().map(|n| (n.to_string(), *inner.clone())))
                }
                (MatchPattern::None, Value::Option(None)) => Some(None),
                (MatchPattern::Wildcard, _) => Some(None),
                (
                    MatchPattern::Regex(_)
                    | MatchPattern::Ok(_)
                    | MatchPattern::Err(_)
                    | MatchPattern::Some(_)
                    | MatchPattern::None,
                    _,
                ) => None,
            };
            if let Some(binding) = structural {
                self.env.push();
                if let Some((name, bound_val)) = binding {
                    self.env.set(name, bound_val);
                }
                let result = self.eval_expr(&arm.expr);
                self.env.pop();
                return result;
            }
            // Fall back to regex matching (stringifies the value).
            if let MatchPattern::Regex(pattern) = &arm.pattern {
                let s = val.as_string_lossy();
                if let Some(captures) = regex_captures(&s, pattern)? {
                    self.env.push();
                    for (name, value) in captures {
                        self.env.set(name, Value::Str(value));
                    }
                    let result = self.eval_expr(&arm.expr);
                    self.env.pop();
                    return result;
                }
            }
        }
        Ok(Value::Null)
    }

    fn eval_call(&mut self, callee: &Expr, args: &[Expr]) -> Result<Value, EvalError> {
        if let Expr::Ident(name) = callee.unspanned() {
            if let Some(method) = crate::methods::moved_function(name) {
                if let Some(function) = self.env.get(name) {
                    let values = args
                        .iter()
                        .map(|arg| self.eval_expr(arg))
                        .collect::<Result<Vec<_>, _>>()?;
                    return self.call_callable(function, values);
                } else {
                    return Err(RillError::new(format!(
                        "{name}(...) was removed; use receiver.{method}(...)"
                    ))
                    .into());
                }
            }
        }
        if let Expr::ImplicitField(name) = callee.unspanned() {
            return self.eval_call(
                &Expr::Field { base: Box::new(Expr::Implicit), name: name.clone() },
                args,
            );
        }
        if let Expr::Field { base, name } = callee.unspanned() {
            if let Expr::Ident(module) = base.unspanned() {
                if crate::methods::is_namespace(module) && self.env.get(module).is_none() {
                    if let Some(call) = crate::methods::namespace_method(module, name) {
                        return self.eval_namespace_method(module, name, call, args);
                    }
                    return Err(RillError::new(format!("{module} has no function '{name}'")).into());
                }
            }
            if crate::methods::lookup(name).is_some() {
                return self.eval_method(base, name, args);
            }
        }

        let name = match callee.unspanned() {
            Expr::Ident(name) => Some(name.as_str()),
            _ => None,
        };

        if let Some(name) = name {
            return self.eval_builtin_or_function(name, args);
        }

        let value = self.eval_expr(callee)?;
        let values = args.iter().map(|arg| self.eval_expr(arg)).collect::<Result<Vec<_>, _>>()?;
        self.call_callable(value, values)
    }

    fn eval_namespace_method(
        &mut self,
        module: &str,
        name: &str,
        call: crate::methods::NamespaceCall,
        args: &[Expr],
    ) -> Result<Value, EvalError> {
        let signature = call.method;
        if !((signature.min_args + 1)..=(signature.max_args + 1)).contains(&args.len()) {
            return Err(RillError::new(format!(
                "{module}.{name} needs {}{} args, got {}",
                signature.min_args + 1,
                if signature.min_args == signature.max_args {
                    String::new()
                } else {
                    format!(" to {}", signature.max_args + 1)
                },
                args.len()
            ))
            .into());
        }
        // Evaluate in namespace argument order, including regex.is_match(pattern, text).
        let mut values =
            args.iter().map(|arg| self.eval_expr(arg)).collect::<Result<Vec<_>, _>>()?;
        match call.adapter {
            crate::methods::ReceiverAdapter::Direct => {}
            crate::methods::ReceiverAdapter::Path => {
                values[0] = match &values[0] {
                    Value::Path(path) => Value::Path(path.clone()),
                    Value::Str(path) => {
                        Value::Path(self.eval_resolve_path(path).map_err(EvalError::from)?)
                    }
                    other => {
                        return Err(RillError::new(format!(
                            "{module}.{name} needs a Str or Path, got {}",
                            other.type_name()
                        ))
                        .into())
                    }
                };
            }
            crate::methods::ReceiverAdapter::Duration => {
                values[0] = match &values[0] {
                    Value::Duration(duration) => Value::Duration(*duration),
                    Value::Int(_) => Value::Duration(std::time::Duration::from_secs(as_usize(
                        &values[0],
                    )?
                        as u64)),
                    other => {
                        return Err(RillError::new(format!(
                            "{module}.{name} needs Int seconds or Duration, got {}",
                            other.type_name()
                        ))
                        .into())
                    }
                };
            }
            crate::methods::ReceiverAdapter::SwapFirstTwo => values.swap(0, 1),
        }
        self.env.push();
        let bound = values
            .into_iter()
            .enumerate()
            .map(|(index, value)| {
                let key = format!("$namespace_arg_{index}");
                self.env.set(key.clone(), value);
                Expr::Ident(key)
            })
            .collect::<Vec<_>>();
        let result = self.eval_method(&bound[0], signature.name, &bound[1..]);
        self.env.pop();
        result
    }

    fn eval_method(&mut self, base: &Expr, name: &str, args: &[Expr]) -> Result<Value, EvalError> {
        use crate::methods::Target;
        let receiver = self.eval_expr(base)?;
        let method = crate::methods::lookup(name)
            .ok_or_else(|| RillError::new(format!("unknown method .{name}()")))?;
        let kind = method_kind(&receiver);
        method.validate(kind, args.len()).map_err(RillError::new)?;
        if let Target::Structural(operation) = method.target {
            return self.eval_structural_method(operation, receiver, args);
        }
        self.env.push();
        // These names cannot occur in source. Binding evaluated values avoids both
        // repeated receiver evaluation and re-interpolating returned strings.
        self.env.set("$method_receiver".into(), receiver.clone());
        let result = (|| {
            let mut bound = Vec::new();
            for (index, arg) in args.iter().enumerate() {
                if method.argument(index) == crate::methods::Argument::Function
                    && contains_implicit(arg)
                {
                    if matches!(receiver, Value::Map(_)) {
                        return Err(RillError::new(format!(
                            ".{name}() callback needs 2 args, got 1"
                        ))
                        .into());
                    }
                    bound.push(arg.clone());
                    continue;
                }
                let (named, expr) = match arg.unspanned() {
                    Expr::NamedArg { name, value } => (Some(name.clone()), value.as_ref()),
                    _ => (None, arg),
                };
                match (&named, method.argument(index)) {
                    (Some(label), crate::methods::Argument::NamedBool(expected))
                        if label == expected => {}
                    (Some(label), _) => {
                        return Err(RillError::new(format!(
                            "unknown .{name}() argument '{label}'"
                        ))
                        .into());
                    }
                    (None, crate::methods::Argument::NamedBool(label)) => {
                        return Err(RillError::new(format!(
                            ".{name}() argument {} must be {label}=...",
                            index + 1
                        ))
                        .into());
                    }
                    _ => {}
                }
                let value = self.eval_expr(expr)?;
                if !method.accepts_argument(kind, index, method_kind(&value)) {
                    return Err(RillError::new(format!(
                        "invalid .{name}() argument {}: {}",
                        index + 1,
                        value.type_name()
                    ))
                    .into());
                }
                if method.argument(index) == crate::methods::Argument::Function {
                    let actual = match &value {
                        Value::Function(function) => function.function.params.len(),
                        _ => unreachable!("callback kind was validated"),
                    };
                    let expected = if matches!(receiver, Value::Map(_)) { 2 } else { 1 };
                    if actual != expected {
                        return Err(RillError::new(format!(
                            ".{name}() callback needs {expected} args, got {actual}"
                        ))
                        .into());
                    }
                }
                let key = format!("$method_arg_{index}");
                self.env.set(key.clone(), value);
                let expr = Expr::Ident(key);
                bound.push(match named {
                    Some(name) => Expr::NamedArg { name, value: Box::new(expr) },
                    None => expr,
                });
            }
            let base = Expr::Ident("$method_receiver".into());
            let mut all = vec![base.clone()];
            all.extend(bound.iter().cloned());
            match method.target {
                Target::Builtin(op) => self.eval_builtin_or_function(op.name(), &all),
                Target::Module("json", op) => self.eval_json(op, &all),
                Target::Module(module, op) => self.eval_stdlib(module, op, &all),
                Target::Replace => self.eval_replace_method(&base, &bound),
                Target::Match => {
                    let other = self.eval_expr(&bound[0])?;
                    let (text, pattern) = if matches!(receiver, Value::Regex(_)) {
                        (other.as_string_lossy(), receiver.as_string_lossy())
                    } else {
                        (receiver.as_string_lossy(), other.as_string_lossy())
                    };
                    Ok(Value::Bool(regex_is_match(&text, &pattern)?))
                }
                Target::Sleep => {
                    let Value::Duration(duration) = receiver else { unreachable!() };
                    self.sleep_cancellable(duration)?;
                    Ok(Value::Null)
                }
                Target::Read(yaml) => {
                    let Value::Path(path) = &receiver else { unreachable!() };
                    let result = fs::read_to_string(path)
                        .map_err(|err| ErrorValue {
                            kind: ErrorKind::Io,
                            message: format!("failed to read '{}': {err}", path.display()),
                            operation: Some(name.into()),
                            status: None,
                            span: None,
                        })
                        .and_then(|text| {
                            if !yaml {
                                return Ok(Value::Str(text));
                            }
                            serde_yaml::from_str::<serde_yaml::Value>(&text)
                                .map_err(|err| err.to_string())
                                .and_then(|value| {
                                    serde_json::to_value(value).map_err(|err| err.to_string())
                                })
                                .map(json_to_value)
                                .map_err(|err| ErrorValue {
                                    kind: ErrorKind::Parse,
                                    message: format!(
                                        "failed to parse YAML '{}': {err}",
                                        path.display()
                                    ),
                                    operation: Some(name.into()),
                                    status: None,
                                    span: None,
                                })
                        });
                    Ok(Value::Result(
                        result.map(Box::new).map_err(|err| Box::new(Value::Error(err))),
                    ))
                }
                Target::Structural(_) => unreachable!("structural methods dispatch before binding"),
            }
        })();
        self.env.pop();
        result
    }

    fn eval_structural_method(
        &mut self,
        operation: crate::methods::Structural,
        receiver: Value,
        args: &[Expr],
    ) -> Result<Value, EvalError> {
        use crate::methods::Structural;
        match (operation, receiver) {
            (Structural::IsOk, Value::Result(value)) => Ok(Value::Bool(value.is_ok())),
            (Structural::IsErr, Value::Result(value)) => Ok(Value::Bool(value.is_err())),
            (Structural::Ok, Value::Result(Ok(value))) => Ok(Value::Option(Some(value))),
            (Structural::Ok, Value::Result(Err(_))) => Ok(Value::Option(None)),
            (Structural::Err, Value::Result(Err(error))) => Ok(Value::Option(Some(error))),
            (Structural::Err, Value::Result(Ok(_))) => Ok(Value::Option(None)),
            (Structural::IsSome, Value::Option(value)) => Ok(Value::Bool(value.is_some())),
            (Structural::IsNone, Value::Option(value)) => Ok(Value::Bool(value.is_none())),
            (Structural::Unwrap, Value::Result(Ok(value)))
            | (Structural::Unwrap, Value::Option(Some(value))) => Ok(*value),
            (Structural::Unwrap, Value::Result(Err(error))) => {
                Err(RillError::new(format!("unwrap called on err: {error}")).into())
            }
            (Structural::Unwrap, Value::Option(None)) => {
                Err(RillError::new("unwrap called on none").into())
            }
            (Structural::UnwrapOr, Value::Result(value)) => {
                let default = self.eval_expr(&args[0])?;
                Ok(value.map(|value| *value).unwrap_or(default))
            }
            (Structural::UnwrapOr, Value::Option(value)) => {
                let default = self.eval_expr(&args[0])?;
                Ok(value.map(|value| *value).unwrap_or(default))
            }
            _ => unreachable!("registry validated structural receiver"),
        }
    }

    fn eval_json(&mut self, name: &str, args: &[Expr]) -> Result<Value, EvalError> {
        if args.len() != 1 {
            return Err(RillError::new(format!("json.{name} needs 1 arg")).into());
        }
        match name {
            "parse" => {
                let text = self.eval_expr(&args[0])?.as_string_lossy();
                Ok(serde_json::from_str::<serde_json::Value>(&text)
                    .map(json_to_value)
                    .map_err(|err| RillError::new(format!("json parse failed: {err}")))?)
            }
            "stringify" => Ok(serde_json::to_string(&value_to_json(&self.eval_expr(&args[0])?))
                .map(Value::Str)
                .map_err(|err| RillError::new(format!("json stringify failed: {err}")))?),
            "pretty" => {
                Ok(serde_json::to_string_pretty(&value_to_json(&self.eval_expr(&args[0])?))
                    .map(Value::Str)
                    .map_err(|err| {
                        RillError::new(format!("json pretty stringify failed: {err}"))
                    })?)
            }
            _ => Err(RillError::new(format!("json has no function '{name}'")).into()),
        }
    }

    fn eval_stdlib(&mut self, module: &str, name: &str, args: &[Expr]) -> Result<Value, EvalError> {
        match (module, name) {
            ("Stream", "tail") => {
                if args.len() != 2 {
                    return Err(RillError::new("Stream.tail needs 2 args: hosts, path").into());
                }
                let hosts = match self.eval_expr(&args[0])? {
                    Value::Str(s) => vec![s],
                    Value::List(items) => items.into_iter().map(|v| v.as_string_lossy()).collect(),
                    other => {
                        return Err(RillError::new(format!(
                            "Stream.tail hosts must be Str or List, got {}",
                            other.type_name()
                        ))
                        .into())
                    }
                };
                let path = self.eval_expr(&args[1])?.as_string_lossy();
                let handle = crate::runtime::spawn_tail(hosts, path).map_err(EvalError::Runtime)?;
                Ok(Value::Stream(handle))
            }
            ("fs", "write") => {
                if args.len() != 2 {
                    return Err(RillError::new("fs.write needs 2 args").into());
                }
                let path = self.eval_expr(&args[0])?.as_string_lossy();
                let text = self.eval_expr(&args[1])?.as_string_lossy();
                self.check_cancelled()?;
                Ok(fs::write(&path, text)
                    .map(|_| Value::Null)
                    .map_err(|err| RillError::new(format!("failed to write '{path}': {err}")))?)
            }
            ("fs", "exists") => Ok(Value::Bool(
                PathBuf::from(self.eval_one_arg("fs.exists", args)?.as_string_lossy()).exists(),
            )),
            ("path", "join") => {
                if args.len() != 2 {
                    return Err(RillError::new("path.join needs 2 args").into());
                }
                let base = self.eval_expr(&args[0])?.as_string_lossy();
                let child_str = self.eval_expr(&args[1])?.as_string_lossy();
                let child = self.eval_resolve_path(&child_str)?;
                Ok(Value::Path(PathBuf::from(base).join(child)))
            }
            ("csv", "parse") => {
                let text = self.eval_one_arg("csv.parse", args)?.as_string_lossy();
                Ok(Value::List(
                    text.lines()
                        .map(|line| {
                            Value::List(
                                line.split(',')
                                    .map(|item| Value::Str(item.trim().to_string()))
                                    .collect(),
                            )
                        })
                        .collect(),
                ))
            }
            ("toml", "parse") => {
                let text = self.eval_one_arg("toml.parse", args)?.as_string_lossy();
                let value = text
                    .parse::<toml::Value>()
                    .map_err(|err| RillError::new(format!("toml parse failed: {err}")))?;
                let json = serde_json::to_value(value)
                    .map_err(|err| RillError::new(format!("toml conversion failed: {err}")))?;
                Ok(json_to_value(json))
            }
            ("yaml", "parse") => {
                let text = self.eval_one_arg("yaml.parse", args)?.as_string_lossy();
                let value = serde_yaml::from_str::<serde_yaml::Value>(&text)
                    .map_err(|err| RillError::new(format!("yaml parse failed: {err}")))?;
                let json = serde_json::to_value(value)
                    .map_err(|err| RillError::new(format!("yaml conversion failed: {err}")))?;
                Ok(json_to_value(json))
            }
            ("url", "parse") => {
                let text = self.eval_one_arg("url.parse", args)?.as_string_lossy();
                let parsed = parse_url(&text)?;
                Ok(Value::Map(parsed))
            }
            ("hashes", "sha256") => {
                use sha2::{Digest, Sha256};
                let text = self.eval_one_arg("hashes.sha256", args)?.as_string_lossy();
                let digest = Sha256::digest(text.as_bytes());
                Ok(Value::Str(digest.iter().map(|byte| format!("{byte:02x}")).collect()))
            }
            ("toml", "stringify") => {
                Ok(toml::to_string_pretty(&value_to_json(&self.eval_expr(&args[0])?))
                    .map(Value::Str)
                    .map_err(|err| RillError::new(format!("toml stringify failed: {err}")))?)
            }
            ("yaml", "stringify") => {
                Ok(serde_yaml::to_string(&value_to_json(&self.eval_expr(&args[0])?))
                    .map(Value::Str)
                    .map_err(|err| RillError::new(format!("yaml stringify failed: {err}")))?)
            }
            ("archive", "list") => {
                let path = self.eval_one_arg("archive.list", args)?.as_string_lossy();
                let file = fs::File::open(&path).map_err(|err| {
                    RillError::new(format!("failed to open archive '{path}': {err}"))
                })?;
                let mut entries = Vec::new();
                for entry in tar::Archive::new(file)
                    .entries()
                    .map_err(|err| RillError::new(format!("failed to read archive: {err}")))?
                {
                    let entry = entry.map_err(|err| {
                        RillError::new(format!("failed to read archive entry: {err}"))
                    })?;
                    entries.push(Value::Str(
                        entry
                            .path()
                            .map_err(|err| RillError::new(format!("invalid archive path: {err}")))?
                            .display()
                            .to_string(),
                    ));
                }
                Ok(Value::List(entries))
            }
            ("archive", "extract") => {
                if args.len() != 2 {
                    return Err(RillError::new("archive.extract needs 2 args").into());
                }
                let archive = self.eval_expr(&args[0])?.as_string_lossy();
                let dest_str = self.eval_expr(&args[1])?.as_string_lossy();
                let destination = self.eval_resolve_path(&dest_str)?;
                let file = fs::File::open(&archive).map_err(|err| {
                    RillError::new(format!("failed to open archive '{archive}': {err}"))
                })?;
                self.check_cancelled()?;
                Ok(tar::Archive::new(file)
                    .unpack(destination)
                    .map(|_| Value::Null)
                    .map_err(|err| RillError::new(format!("failed to extract archive: {err}")))?)
            }
            ("path", "parent") => {
                let p = PathBuf::from(self.eval_one_arg("path.parent", args)?.as_string_lossy());
                Ok(Value::Path(p.parent().unwrap_or(std::path::Path::new("")).to_path_buf()))
            }
            ("path", "basename") => {
                let p = PathBuf::from(self.eval_one_arg("path.basename", args)?.as_string_lossy());
                Ok(Value::Str(
                    p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
                ))
            }
            ("path", "stem") => {
                let p = PathBuf::from(self.eval_one_arg("path.stem", args)?.as_string_lossy());
                Ok(Value::Str(
                    p.file_stem().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
                ))
            }
            ("path", "extension") => {
                let p = PathBuf::from(self.eval_one_arg("path.extension", args)?.as_string_lossy());
                Ok(Value::Str(
                    p.extension().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
                ))
            }
            ("path", "canonicalize") => {
                let s = self.eval_one_arg("path.canonicalize", args)?.as_string_lossy();
                let p = PathBuf::from(&s);
                Ok(Value::Path(
                    p.canonicalize()
                        .map_err(|err| RillError::new(format!("canonicalize '{s}': {err}")))?,
                ))
            }
            ("fs", "is_file") => {
                let p = PathBuf::from(self.eval_one_arg("fs.is_file", args)?.as_string_lossy());
                Ok(Value::Bool(p.is_file()))
            }
            ("fs", "is_dir") => {
                let p = PathBuf::from(self.eval_one_arg("fs.is_dir", args)?.as_string_lossy());
                Ok(Value::Bool(p.is_dir()))
            }
            ("fs", "read_lines") => {
                let path_str = self.eval_one_arg("fs.read_lines", args)?.as_string_lossy();
                let text = fs::read_to_string(&path_str)
                    .map_err(|err| RillError::new(format!("failed to read '{path_str}': {err}")))?;
                Ok(Value::List(text.lines().map(|l| Value::Str(l.to_string())).collect()))
            }
            ("fs", "append") => {
                if args.len() != 2 {
                    return Err(RillError::new("fs.append needs 2 args").into());
                }
                let path_str = self.eval_expr(&args[0])?.as_string_lossy();
                let text = self.eval_expr(&args[1])?.as_string_lossy();
                self.check_cancelled()?;
                use std::io::Write;
                let mut file =
                    fs::OpenOptions::new().create(true).append(true).open(&path_str).map_err(
                        |err| RillError::new(format!("failed to open '{path_str}': {err}")),
                    )?;
                file.write_all(text.as_bytes()).map_err(|err| {
                    RillError::new(format!("failed to append to '{path_str}': {err}"))
                })?;
                Ok(Value::Null)
            }
            ("fs", "mkdir") => {
                let path_str = self.eval_one_arg("fs.mkdir", args)?.as_string_lossy();
                self.check_cancelled()?;
                fs::create_dir_all(&path_str).map_err(|err| {
                    RillError::new(format!("failed to mkdir '{path_str}': {err}"))
                })?;
                Ok(Value::Null)
            }
            ("fs", "rename") => {
                if args.len() != 2 {
                    return Err(RillError::new("fs.rename needs 2 args").into());
                }
                let from = self.eval_expr(&args[0])?.as_string_lossy();
                let to = self.eval_expr(&args[1])?.as_string_lossy();
                self.check_cancelled()?;
                fs::rename(&from, &to).map_err(|err| {
                    RillError::new(format!("failed to rename '{from}' → '{to}': {err}"))
                })?;
                Ok(Value::Null)
            }
            ("fs", "stat") => {
                let path_str = self.eval_one_arg("fs.stat", args)?.as_string_lossy();
                let meta = fs::metadata(&path_str)
                    .map_err(|err| RillError::new(format!("stat '{path_str}': {err}")))?;
                let modified = meta
                    .modified()
                    .ok()
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|d| Value::Int(d.as_secs() as i64))
                    .unwrap_or(Value::Null);
                let mut map = Map::new();
                map.insert("size".into(), Value::Int(meta.len() as i64));
                map.insert("modified".into(), modified);
                map.insert("is_file".into(), Value::Bool(meta.is_file()));
                map.insert("is_dir".into(), Value::Bool(meta.is_dir()));
                map.insert("is_symlink".into(), Value::Bool(meta.file_type().is_symlink()));
                Ok(Value::Map(map))
            }
            ("fs", "ls") => {
                let path_str = self.eval_one_arg("fs.ls", args)?.as_string_lossy();
                let mut entries = Vec::new();
                for entry in fs::read_dir(&path_str)
                    .map_err(|err| RillError::new(format!("ls '{path_str}': {err}")))?
                {
                    let entry =
                        entry.map_err(|err| RillError::new(format!("ls entry error: {err}")))?;
                    entries.push(Value::Path(entry.path()));
                }
                entries.sort_by_cached_key(Value::as_string_lossy);
                Ok(Value::List(entries))
            }
            ("process", "run") => {
                let raw = argv_from_value(self.eval_one_arg("process.run", args)?, "process.run")?;
                let argv = self.apply_remote_prefix(raw);
                Ok(self
                    .execute_command(&argv, None, &CommandIo::default())
                    .map(Value::ProcessResult)?)
            }
            _ => Err(RillError::new(format!("{module} has no function '{name}'")).into()),
        }
    }

    fn eval_builtin_or_function(&mut self, name: &str, args: &[Expr]) -> Result<Value, EvalError> {
        if let Some(builtin) = crate::methods::lookup_builtin(name) {
            builtin.validate(args.len()).map_err(RillError::new)?;
            return self.eval_global_builtin(builtin.implementation, args);
        }
        match name {
            "get" => {
                if args.len() != 2 {
                    return Err(RillError::new("get needs 2 args").into());
                }
                let map = self.eval_expr(&args[0])?;
                let key = self.eval_expr(&args[1])?.as_string_lossy();
                match map {
                    Value::Map(values) => Ok(values.get(&key).cloned().unwrap_or(Value::Null)),
                    Value::Null => Ok(Value::Null),
                    other => {
                        Err(RillError::new(format!("get needs a Map, got {}", other.type_name()))
                            .into())
                    }
                }
            }
            "spawn" => {
                let value = self.eval_one_arg("spawn", args)?;
                let argv = self.apply_remote_prefix(argv_from_value(value, "spawn")?);
                self.check_cancelled()?;
                let handle = TokioProcessRuntime.spawn(ProcessSpec {
                    argv,
                    input: None,
                    io: self.command_io_with_environment(CommandIo::default())?,
                })?;
                Ok(Value::ProcessHandle(handle))
            }
            "await" => {
                let value = self.eval_one_arg("await", args)?;
                let Value::ProcessHandle(handle) = value else {
                    return Err(RillError::new("await needs ProcessHandle").into());
                };
                Ok(Value::ProcessResult(TokioProcessRuntime.await_process(handle)?))
            }
            "parallel" => {
                let value = self.eval_one_arg("parallel", args)?;
                let Value::List(commands) = value else {
                    return Err(RillError::new("parallel needs List[List[Str]]").into());
                };
                let specs = commands
                    .into_iter()
                    .map(|command| {
                        Ok(ProcessSpec {
                            argv: self.apply_remote_prefix(argv_from_value(command, "parallel")?),
                            input: None,
                            io: self.command_io_with_environment(CommandIo::default())?,
                        })
                    })
                    .collect::<Result<Vec<_>, EvalError>>()?;
                self.check_cancelled()?;
                Ok(Value::List(
                    TokioProcessRuntime
                        .parallel(specs)?
                        .into_iter()
                        .map(Value::ProcessResult)
                        .collect(),
                ))
            }
            "lines" => {
                let value = self.eval_one_arg("lines", args)?;
                Ok(Value::List(
                    value
                        .as_string_lossy()
                        .lines()
                        .map(|line| Value::Str(line.to_string()))
                        .collect(),
                ))
            }
            "trim" => {
                let value = self.eval_one_arg("trim", args)?;
                Ok(Value::Str(value.as_string_lossy().trim().to_string()))
            }
            "take" => {
                let list = self.eval_expr(&args[0])?;
                let count = as_usize(&self.eval_expr(&args[1])?)?;
                let Value::List(items) = list else {
                    return Err(RillError::new("take needs a List").into());
                };
                Ok(Value::List(items.into_iter().take(count).collect()))
            }
            "reverse" => {
                let mut value = self.eval_one_arg("reverse", args)?;
                let Value::List(items) = &mut value else {
                    return Err(RillError::new("reverse needs a List").into());
                };
                items.reverse();
                Ok(value)
            }
            "map" => self.eval_map(args),
            "parallel_map" => self.eval_parallel_map(args),
            "filter" => self.eval_filter(args),
            "parallel_filter" => self.eval_parallel_filter(args),
            "flatten" => self.eval_flatten(args),
            "zip" => self.eval_zip(args),
            "enumerate" => self.eval_enumerate(args),
            "group_by" => self.eval_group_by(args),
            "glob" => {
                let pattern = self.eval_one_arg("glob", args)?.as_string_lossy();
                let paths = simple_glob_with_lookup(&pattern, &|name| {
                    self.lookup_environment_value(name).map(|v| v.as_string_lossy())
                })?;
                Ok(Value::List(paths.into_iter().map(Value::Path).collect()))
            }
            // join: Path join or List string-join depending on receiver
            "join" => {
                let receiver = self.eval_expr(&args[0])?;
                let arg = self.eval_expr(&args[1])?;
                match receiver {
                    Value::List(items) => {
                        let sep = arg.as_string_lossy();
                        let parts: Vec<String> =
                            items.iter().map(|v| v.as_string_lossy()).collect();
                        Ok(Value::Str(parts.join(&sep)))
                    }
                    Value::Path(base) => {
                        let child_str = arg.as_string_lossy();
                        let child = self.eval_resolve_path(&child_str)?;
                        Ok(Value::Path(base.join(child)))
                    }
                    other => Err(RillError::new(format!(
                        "join needs a List or Path, got {}",
                        other.type_name()
                    ))
                    .into()),
                }
            }
            // String methods
            "len" => {
                let val = self.eval_expr(&args[0])?;
                match val {
                    Value::Str(s) => Ok(Value::Int(s.chars().count() as i64)),
                    Value::List(items) => Ok(Value::Int(items.len() as i64)),
                    Value::Map(map) => Ok(Value::Int(map.len() as i64)),
                    other => {
                        Err(RillError::new(format!("len not supported on {}", other.type_name()))
                            .into())
                    }
                }
            }
            "split" => {
                let s = self.eval_expr(&args[0])?.as_string_lossy();
                let sep = self.eval_expr(&args[1])?.as_string_lossy();
                Ok(Value::List(s.split(sep.as_str()).map(|p| Value::Str(p.to_string())).collect()))
            }
            "upper" => Ok(Value::Str(self.eval_expr(&args[0])?.as_string_lossy().to_uppercase())),
            "lower" => Ok(Value::Str(self.eval_expr(&args[0])?.as_string_lossy().to_lowercase())),
            "contains" => {
                let receiver = self.eval_expr(&args[0])?;
                let needle = self.eval_expr(&args[1])?;
                match receiver {
                    Value::Str(s) => Ok(Value::Bool(s.contains(needle.as_string_lossy().as_str()))),
                    Value::List(items) => Ok(Value::Bool(items.contains(&needle))),
                    other => Err(RillError::new(format!(
                        "contains not supported on {}",
                        other.type_name()
                    ))
                    .into()),
                }
            }
            "starts_with" => {
                let s = self.eval_expr(&args[0])?.as_string_lossy();
                let prefix = self.eval_expr(&args[1])?.as_string_lossy();
                Ok(Value::Bool(s.starts_with(prefix.as_str())))
            }
            "ends_with" => {
                let s = self.eval_expr(&args[0])?.as_string_lossy();
                let suffix = self.eval_expr(&args[1])?.as_string_lossy();
                Ok(Value::Bool(s.ends_with(suffix.as_str())))
            }
            "repeat" => {
                let s = self.eval_expr(&args[0])?.as_string_lossy();
                let n = as_usize(&self.eval_expr(&args[1])?)?;
                Ok(Value::Str(s.repeat(n)))
            }
            "index_of" => {
                let s = self.eval_expr(&args[0])?.as_string_lossy();
                let needle = self.eval_expr(&args[1])?.as_string_lossy();
                Ok(Value::Int(match s.find(needle.as_str()) {
                    Some(idx) => s[..idx].chars().count() as i64,
                    None => -1,
                }))
            }
            // List methods
            "sort" => {
                let val = self.eval_expr(&args[0])?;
                let Value::List(mut items) = val else {
                    return Err(RillError::new("sort needs a List").into());
                };
                let all_int = items.iter().all(|v| matches!(v, Value::Int(_)));
                if all_int {
                    items.sort_by_key(|v| if let Value::Int(n) = v { *n } else { 0 });
                } else {
                    items.sort_by_cached_key(Value::as_string_lossy);
                }
                Ok(Value::List(items))
            }
            "flat_map" => self.eval_flat_map(args),
            "find" | "any" => self.eval_search(name, args),
            "first" => {
                let val = self.eval_expr(&args[0])?;
                let Value::List(items) = val else {
                    return Err(RillError::new("first needs a List").into());
                };
                Ok(items.into_iter().next().unwrap_or(Value::Null))
            }
            "last" => {
                let val = self.eval_expr(&args[0])?;
                let Value::List(items) = val else {
                    return Err(RillError::new("last needs a List").into());
                };
                Ok(items.into_iter().next_back().unwrap_or(Value::Null))
            }
            "slice" => {
                let list = self.eval_expr(&args[0])?;
                let start = as_usize(&self.eval_expr(&args[1])?)?;
                let end = as_usize(&self.eval_expr(&args[2])?)?;
                let Value::List(items) = list else {
                    return Err(RillError::new("slice needs a List").into());
                };
                let len = items.len();
                let start = start.min(len);
                let end = end.min(len);
                Ok(Value::List(items[start..end].to_vec()))
            }
            "skip" => {
                let list = self.eval_expr(&args[0])?;
                let n = as_usize(&self.eval_expr(&args[1])?)?;
                let Value::List(items) = list else {
                    return Err(RillError::new("skip needs a List").into());
                };
                Ok(Value::List(items.into_iter().skip(n).collect()))
            }
            "difference" => {
                let Value::List(a) = self.eval_expr(&args[0])? else {
                    return Err(RillError::new("difference needs a List").into());
                };
                let Value::List(b) = self.eval_expr(&args[1])? else {
                    return Err(RillError::new("difference needs a List").into());
                };
                Ok(Value::List(match crate::eval_collections::difference_membership(&a, &b) {
                    Some(membership) => a
                        .into_iter()
                        .zip(membership)
                        .filter_map(|(value, keep)| keep.then_some(value))
                        .collect(),
                    None => a.into_iter().filter(|x| !b.contains(x)).collect(),
                }))
            }
            "symmetric_difference" => {
                let Value::List(a) = self.eval_expr(&args[0])? else {
                    return Err(RillError::new("symmetric_difference needs a List").into());
                };
                let Value::List(b) = self.eval_expr(&args[1])? else {
                    return Err(RillError::new("symmetric_difference needs a List").into());
                };
                let (only_a, only_b): (Vec<Value>, Vec<Value>) =
                    match crate::eval_collections::symmetric_membership(&a, &b) {
                        Some((a_membership, b_membership)) => (
                            a.into_iter()
                                .zip(a_membership)
                                .filter_map(|(value, keep)| keep.then_some(value))
                                .collect(),
                            b.into_iter()
                                .zip(b_membership)
                                .filter_map(|(value, keep)| keep.then_some(value))
                                .collect(),
                        ),
                        None => (
                            a.iter().filter(|x| !b.contains(x)).cloned().collect(),
                            b.iter().filter(|x| !a.contains(x)).cloned().collect(),
                        ),
                    };
                Ok(Value::List(only_a.into_iter().chain(only_b).collect()))
            }
            // Map methods
            "keys" => {
                let val = self.eval_expr(&args[0])?;
                let Value::Map(map) = val else {
                    return Err(RillError::new("keys needs a Map").into());
                };
                Ok(Value::List(map.into_keys().map(Value::Str).collect()))
            }
            "values" => {
                let val = self.eval_expr(&args[0])?;
                let Value::Map(map) = val else {
                    return Err(RillError::new("values needs a Map").into());
                };
                Ok(Value::List(map.into_values().collect()))
            }
            "entries" => {
                let val = self.eval_expr(&args[0])?;
                let Value::Map(map) = val else {
                    return Err(RillError::new("entries needs a Map").into());
                };
                Ok(Value::List(
                    map.into_iter().map(|(k, v)| Value::List(vec![Value::Str(k), v])).collect(),
                ))
            }
            "contains_key" => {
                let val = self.eval_expr(&args[0])?;
                let key = self.eval_expr(&args[1])?.as_string_lossy();
                let Value::Map(map) = val else {
                    return Err(RillError::new("contains_key needs a Map").into());
                };
                Ok(Value::Bool(map.contains_key(&key)))
            }
            "merge" => {
                let base_val = self.eval_expr(&args[0])?;
                let other_val = self.eval_expr(&args[1])?;
                let Value::Map(mut base) = base_val else {
                    return Err(RillError::new("merge needs a Map").into());
                };
                let Value::Map(other) = other_val else {
                    return Err(RillError::new("merge argument must be a Map").into());
                };
                base.extend(other);
                Ok(Value::Map(base))
            }
            _ => {
                if let Some(value) = self.env.lookup(name)? {
                    let values = args
                        .iter()
                        .map(|arg| self.eval_expr(arg))
                        .collect::<Result<Vec<_>, _>>()?;
                    self.call_callable(value, values)
                } else {
                    Err(RillError::new(format!("unknown function '{name}'")).into())
                }
            }
        }
    }

    fn eval_global_builtin(
        &mut self,
        implementation: crate::methods::BuiltinImplementation,
        args: &[Expr],
    ) -> Result<Value, EvalError> {
        use crate::methods::BuiltinImplementation as Builtin;
        match implementation {
            Builtin::Str => Ok(Value::Str(self.eval_expr(&args[0])?.as_string_lossy())),
            Builtin::Bool => Ok(Value::Bool(self.eval_expr(&args[0])?.truthy())),
            Builtin::Int => match self.eval_expr(&args[0])? {
                Value::Int(value) => Ok(Value::Int(value)),
                Value::Float(value) => Ok(Value::Int(value as i64)),
                Value::Bool(value) => Ok(Value::Int(value as i64)),
                Value::Str(value) => {
                    value.trim().parse::<i64>().map(Value::Int).map_err(|_| {
                        RillError::new(format!("cannot coerce {value:?} to Int")).into()
                    })
                }
                other => {
                    Err(RillError::new(format!("cannot coerce {} to Int", other.type_name()))
                        .into())
                }
            },
            Builtin::Float => match self.eval_expr(&args[0])? {
                Value::Float(value) => Ok(Value::Float(value)),
                Value::Int(value) => Ok(Value::Float(value as f64)),
                Value::Bool(value) => Ok(Value::Float(if value { 1.0 } else { 0.0 })),
                Value::Str(value) => value.trim().parse::<f64>().map(Value::Float).map_err(|_| {
                    RillError::new(format!("cannot coerce {value:?} to Float")).into()
                }),
                other => {
                    Err(RillError::new(format!("cannot coerce {} to Float", other.type_name()))
                        .into())
                }
            },
            Builtin::Type => Ok(Value::Str(self.eval_expr(&args[0])?.type_name().to_string())),
            Builtin::PrettyPrint => {
                let value = self.eval_expr(&args[0])?;
                println!("{}{}", pretty_value(&value, 0), ANSI_RESET);
                Ok(Value::Null)
            }
            Builtin::Print | Builtin::Prin | Builtin::Warn => {
                for arg in args {
                    let value = self.eval_expr(arg)?;
                    self.check_cancelled()?;
                    if implementation == Builtin::Print {
                        println!("{value}");
                    } else if implementation == Builtin::Prin {
                        use std::io::Write;
                        let mut stdout = std::io::stdout().lock();
                        write!(stdout, "{value}").map_err(|error| {
                            RillError::new(format!("failed to write stdout: {error}"))
                        })?;
                        stdout.flush().map_err(|error| {
                            RillError::new(format!("failed to flush stdout: {error}"))
                        })?;
                    } else {
                        eprintln!("{value}");
                    }
                }
                Ok(Value::Null)
            }
            Builtin::Die => {
                let message = match args.first() {
                    Some(arg) => self.eval_expr(arg)?.as_string_lossy(),
                    None => "die".to_string(),
                };
                Err(RillError::new(message).into())
            }
            Builtin::SetEnv => {
                let name = self.eval_expr(&args[0])?.as_string_lossy();
                let value = self.eval_expr(&args[1])?.as_string_lossy();
                self.set_environment(&name, value)?;
                Ok(Value::Null)
            }
            Builtin::Path | Builtin::File => {
                let value = self.eval_expr(&args[0])?.as_string_lossy();
                Ok(Value::Path(self.eval_resolve_path(&value)?))
            }
            Builtin::Shell => {
                let script = self.eval_expr(&args[0])?.as_string_lossy();
                let argv = self.apply_remote_prefix(vec!["sh".into(), "-c".into(), script]);
                Ok(self
                    .execute_command(&argv, None, &CommandIo::default())
                    .map(Value::ProcessResult)?)
            }
            Builtin::Docker => {
                let container = self.eval_expr(&args[0])?.as_string_lossy();
                Ok(Value::List(vec![
                    Value::Str("sudo".into()),
                    Value::Str("docker".into()),
                    Value::Str("exec".into()),
                    Value::Str(container),
                ]))
            }
            Builtin::Exit => {
                let code = match args.first() {
                    Some(arg) => match self.eval_expr(arg)? {
                        Value::Int(code) => code,
                        _ => return Err(RillError::new("exit code must be Int").into()),
                    },
                    None => 0,
                };
                let code =
                    i32::try_from(code).map_err(|_| RillError::new("exit code must fit in i32"))?;
                Err(RillError::exit(code).into())
            }
            Builtin::Some => Ok(Value::Option(Some(Box::new(self.eval_expr(&args[0])?)))),
            Builtin::None => Ok(Value::Option(None)),
            Builtin::Ok => Ok(Value::Result(Ok(Box::new(self.eval_expr(&args[0])?)))),
            Builtin::Err => Ok(Value::Result(Err(Box::new(self.eval_expr(&args[0])?)))),
        }
    }

    fn eval_map(&mut self, args: &[Expr]) -> Result<Value, EvalError> {
        if args.len() != 2 {
            return Err(RillError::new("map needs 1 or 2 args").into());
        }
        let list = self.eval_expr(&args[0])?;
        let mapper = &args[1];
        match list {
            Value::List(items) => {
                let mut out = Vec::new();
                for item in items {
                    out.push(self.apply_unary(mapper, item)?);
                }
                Ok(Value::List(out))
            }
            Value::Map(entries) => {
                let mut out = Vec::new();
                for (key, value) in entries {
                    out.push(self.apply_binary(mapper, Value::Str(key), value)?);
                }
                Ok(Value::List(out))
            }
            other => {
                Err(RillError::new(format!("map needs a List or Map, got {}", other.type_name()))
                    .into())
            }
        }
    }

    fn eval_parallel_map(&mut self, args: &[Expr]) -> Result<Value, EvalError> {
        if args.len() != 2 {
            return Err(RillError::new("parallel_map needs 1 arg").into());
        }
        let list = self.eval_expr(&args[0])?;
        let callback = args[1].clone();

        // (item_args, is_binary) per slot — binary for Map entries (key, value)
        let slots: Vec<(Vec<Value>, bool)> = match list {
            Value::List(items) => items.into_iter().map(|v| (vec![v], false)).collect(),
            Value::Map(entries) => {
                entries.into_iter().map(|(k, v)| (vec![Value::Str(k), v], true)).collect()
            }
            other => {
                return Err(RillError::new(format!(
                    "parallel_map needs a List or Map, got {}",
                    other.type_name()
                ))
                .into())
            }
        };

        let cancellation = self.cancellation.child_token();
        let terminal = Arc::new(Mutex::new(None::<ParallelTerminal>));
        let template = self.fork(cancellation.clone());
        let executor = self.executor.clone();
        let out = executor.pool.install(|| {
            slots
                .into_par_iter()
                .map(|(item_args, is_binary)| {
                    if cancellation.is_cancelled() {
                        return None;
                    }
                    let mut child = template.clone();
                    let result = catch_parallel_job(|| {
                        if is_binary {
                            child.apply_binary(
                                &callback,
                                item_args[0].clone(),
                                item_args[1].clone(),
                            )
                        } else {
                            child.apply_unary(
                                &callback,
                                item_args.into_iter().next().expect("list slot has one item"),
                            )
                        }
                    });
                    match result {
                        Ok(value) => Some(value),
                        Err(EvalError::Cancelled) => None,
                        Err(EvalError::Runtime(error)) => {
                            record_parallel_terminal(
                                &terminal,
                                &cancellation,
                                ParallelTerminal::Runtime(error),
                            );
                            None
                        }
                        Err(EvalError::Control(flow)) => {
                            record_parallel_terminal(
                                &terminal,
                                &cancellation,
                                ParallelTerminal::Control(flow),
                            );
                            None
                        }
                    }
                })
                .collect::<Vec<_>>()
        });
        if let Some(outcome) = terminal.lock().unwrap_or_else(|error| error.into_inner()).take() {
            return Err(parallel_terminal_error(outcome));
        }
        Ok(Value::List(out.into_iter().flatten().collect()))
    }

    fn eval_filter(&mut self, args: &[Expr]) -> Result<Value, EvalError> {
        if args.len() != 2 {
            return Err(RillError::new("filter needs 1 or 2 args").into());
        }
        let list = self.eval_expr(&args[0])?;
        let Value::List(items) = list else {
            return Err(RillError::new("filter needs a List").into());
        };
        let predicate = &args[1];
        let mut out = Vec::new();
        for item in items {
            if self.apply_unary(predicate, item.clone())?.truthy() {
                out.push(item);
            }
        }
        Ok(Value::List(out))
    }

    fn eval_parallel_filter(&mut self, args: &[Expr]) -> Result<Value, EvalError> {
        if args.len() != 2 {
            return Err(RillError::new("parallel_filter needs 1 arg").into());
        }
        let list = self.eval_expr(&args[0])?;
        let Value::List(items) = list else {
            return Err(RillError::new("parallel_filter needs a List").into());
        };
        let callback = args[1].clone();
        let cancellation = self.cancellation.child_token();
        let terminal = Arc::new(Mutex::new(None::<ParallelTerminal>));
        let template = self.fork(cancellation.clone());
        let executor = self.executor.clone();
        let decisions = executor.pool.install(|| {
            items
                .into_par_iter()
                .map(|item| {
                    if cancellation.is_cancelled() {
                        return None;
                    }
                    let mut child = template.clone();
                    match catch_parallel_job(|| child.apply_unary(&callback, item.clone())) {
                        Ok(value) => Some((item, value.truthy())),
                        Err(EvalError::Cancelled) => None,
                        Err(EvalError::Runtime(error)) => {
                            record_parallel_terminal(
                                &terminal,
                                &cancellation,
                                ParallelTerminal::Runtime(error),
                            );
                            None
                        }
                        Err(EvalError::Control(flow)) => {
                            record_parallel_terminal(
                                &terminal,
                                &cancellation,
                                ParallelTerminal::Control(flow),
                            );
                            None
                        }
                    }
                })
                .collect::<Vec<_>>()
        });
        if let Some(outcome) = terminal.lock().unwrap_or_else(|error| error.into_inner()).take() {
            return Err(parallel_terminal_error(outcome));
        }
        Ok(Value::List(
            decisions
                .into_iter()
                .flatten()
                .filter_map(|(item, keep)| keep.then_some(item))
                .collect(),
        ))
    }

    fn eval_flatten(&mut self, args: &[Expr]) -> Result<Value, EvalError> {
        let list = self.eval_one_arg("flatten", args)?;
        let Value::List(items) = list else {
            return Err(RillError::new("flatten needs a List").into());
        };
        let mut out = Vec::new();
        for item in items {
            match item {
                Value::List(inner) => out.extend(inner),
                other => out.push(other),
            }
        }
        Ok(Value::List(out))
    }

    fn eval_zip(&mut self, args: &[Expr]) -> Result<Value, EvalError> {
        if args.len() != 2 {
            return Err(RillError::new("zip needs 1 arg").into());
        }
        let a = self.eval_expr(&args[0])?;
        let b = self.eval_expr(&args[1])?;
        let Value::List(a) = a else {
            return Err(RillError::new("zip needs a List receiver").into());
        };
        let Value::List(b) = b else {
            return Err(RillError::new("zip argument must be a List").into());
        };
        let out = a.into_iter().zip(b).map(|(x, y)| Value::List(vec![x, y])).collect();
        Ok(Value::List(out))
    }

    fn eval_enumerate(&mut self, args: &[Expr]) -> Result<Value, EvalError> {
        let list = self.eval_one_arg("enumerate", args)?;
        let Value::List(items) = list else {
            return Err(RillError::new("enumerate needs a List").into());
        };
        let out = items
            .into_iter()
            .enumerate()
            .map(|(i, v)| Value::Tuple(vec![Value::Int(i as i64), v]))
            .collect();
        Ok(Value::List(out))
    }

    fn eval_group_by(&mut self, args: &[Expr]) -> Result<Value, EvalError> {
        if args.len() != 2 {
            return Err(RillError::new("group_by needs 1 arg").into());
        }
        let list = self.eval_expr(&args[0])?;
        let Value::List(items) = list else {
            return Err(RillError::new("group_by needs a List").into());
        };
        let keyer = &args[1];
        let mut map: indexmap::IndexMap<String, Vec<Value>> = indexmap::IndexMap::new();
        for item in items {
            let key = self.apply_unary(keyer, item.clone())?.as_string_lossy();
            map.entry(key).or_default().push(item);
        }
        Ok(Value::Map(map.into_iter().map(|(k, v)| (k, Value::List(v))).collect()))
    }

    fn eval_flat_map(&mut self, args: &[Expr]) -> Result<Value, EvalError> {
        if args.len() != 2 {
            return Err(RillError::new("flat_map needs 1 arg").into());
        }
        let list = self.eval_expr(&args[0])?;
        let Value::List(items) = list else {
            return Err(RillError::new("flat_map needs a List").into());
        };
        let mapper = &args[1];
        let mut out = Vec::new();
        for item in items {
            let mapped = self.apply_unary(mapper, item)?;
            match mapped {
                Value::List(inner) => out.extend(inner),
                other => out.push(other),
            }
        }
        Ok(Value::List(out))
    }

    fn eval_search(&mut self, name: &str, args: &[Expr]) -> Result<Value, EvalError> {
        if args.len() != 2 {
            return Err(RillError::new(format!("{name} needs 1 arg")).into());
        }
        let list = self.eval_expr(&args[0])?;
        let Value::List(items) = list else {
            return Err(RillError::new(format!("{name} needs a List")).into());
        };
        let pred = &args[1];
        for item in items {
            let result = self.apply_unary(pred, item.clone())?;
            if is_truthy(&result)? {
                return Ok(if name == "any" {
                    Value::Bool(true)
                } else {
                    Value::Option(Some(Box::new(item)))
                });
            }
        }
        Ok(if name == "any" { Value::Bool(false) } else { Value::Option(None) })
    }

    fn apply_unary(&mut self, callable: &Expr, item: Value) -> Result<Value, EvalError> {
        match callable.unspanned() {
            Expr::Ident(name) => match name.as_str() {
                "trim" => Ok(Value::Str(item.as_string_lossy().trim().to_string())),
                _ => {
                    let value = self
                        .env
                        .lookup(name)?
                        .ok_or_else(|| RillError::new(format!("unknown function '{name}'")))?;
                    self.call_callable(value, vec![item])
                }
            },
            _ => {
                if contains_implicit(callable) {
                    self.env.push();
                    self.env.set("it".to_string(), item.clone());
                    self.env.set("_".to_string(), item);
                    let value = self.eval_expr(callable);
                    self.env.pop();
                    return value;
                }
                let value = self.eval_expr(callable)?;
                self.call_callable(value, vec![item])
            }
        }
    }

    fn apply_binary(
        &mut self,
        callable: &Expr,
        key: Value,
        value: Value,
    ) -> Result<Value, EvalError> {
        let function = self.eval_expr(callable)?;
        self.call_callable(function, vec![key, value])
    }

    fn call_function_values(
        &mut self,
        function: &Function,
        args: Vec<Value>,
    ) -> Result<Value, EvalError> {
        if function.params.len() != args.len() {
            return Err(RillError::new(format!(
                "function expected {} args, got {}",
                function.params.len(),
                args.len()
            ))
            .into());
        }
        for (param, value) in function.params.iter().zip(&args) {
            if let Some(annotation) = &param.type_name {
                crate::typeck::check_value_annotation(value, annotation)?;
            }
        }
        self.call_function_in_env(function, args)
    }

    fn call_function_in_env(
        &mut self,
        function: &Function,
        args: Vec<Value>,
    ) -> Result<Value, EvalError> {
        self.function_depth += 1;
        self.env.push();
        for (param, value) in function.params.iter().zip(args) {
            self.env.set(param.name.clone(), value);
        }
        let flow_result = self.eval_stmts(&function.body);
        self.env.pop();
        self.function_depth -= 1;
        let flow = flow_result?;
        match flow {
            Flow::Value(value) | Flow::Return(value) => Ok(wrap_declared_result(function, value)),
            Flow::Propagate(value) => Ok(propagated_value(value)),
            Flow::Break => Err(RillError::new("break outside loop").into()),
            Flow::Continue => Err(RillError::new("continue outside loop").into()),
            Flow::Emit(_) => Err(RillError::new("emit outside for expression").into()),
        }
    }

    fn call_callable(&mut self, value: Value, args: Vec<Value>) -> Result<Value, EvalError> {
        match value {
            Value::Function(function) => {
                // Run the body in the scope chain the function closed over; a
                // frame for its parameters is pushed on top.  The parallel
                // write floor carries over so a worker cannot smuggle a write
                // through a function defined outside the parallel body.
                let frozen = self.env.frozen.min(function.captured.len());
                let callee_env = Env { scopes: function.captured.clone(), frozen };
                let current = std::mem::replace(&mut self.env, callee_env);
                let result = self.call_function_values(&function.function, args);
                self.env = current;
                result
            }
            other => Err(RillError::new(format!("{} is not callable", other.type_name())).into()),
        }
    }

    fn eval_one_arg(&mut self, name: &str, args: &[Expr]) -> Result<Value, EvalError> {
        if args.len() != 1 {
            return Err(RillError::new(format!("{name} needs 1 arg")).into());
        }
        self.eval_expr(&args[0])
    }

    fn eval_field(&mut self, base: &Expr, name: &str) -> Result<Value, EvalError> {
        // stdlib module accessed as a value (e.g. `json.parse` passed to .map)
        if let Expr::Ident(module) = base.unspanned() {
            if crate::methods::is_namespace(module) && self.env.get(module).is_none() {
                if crate::methods::namespace_method(module, name).is_some() {
                    let call = Expr::Call {
                        callee: Box::new(Expr::Field {
                            base: Box::new(Expr::Ident(module.to_string())),
                            name: name.to_string(),
                        }),
                        args: vec![Expr::Ident("_arg".to_string())],
                    };
                    return Ok(self.make_function(
                        Function {
                            params: vec![Param { name: "_arg".to_string(), type_name: None }],
                            return_type: None,
                            body: vec![Stmt::Return(Some(call))],
                        },
                        None,
                    ));
                }
                return Err(RillError::new(format!("{module} has no function '{name}'")).into());
            }
        }
        let base = self.eval_expr(base)?;
        Ok(field_value(base, name)?)
    }

    fn eval_binary(&mut self, op: BinaryOp, left: &Expr, right: &Expr) -> Result<Value, EvalError> {
        let left = self.eval_expr(left)?;
        let right = self.eval_expr(right)?;
        let value = match op {
            BinaryOp::Eq => Value::Bool(left == right),
            BinaryOp::Ne => Value::Bool(left != right),
            BinaryOp::Gt | BinaryOp::Ge | BinaryOp::Lt | BinaryOp::Le => {
                check_numeric_operands(&left, &right, op)?;
                let order = match (&left, &right) {
                    (Value::Int(a), Value::Int(b)) => a.partial_cmp(b),
                    _ => as_f64(&left)?.partial_cmp(&as_f64(&right)?),
                };
                use std::cmp::Ordering::{Equal, Greater, Less};
                Value::Bool(match op {
                    BinaryOp::Gt => order == Some(Greater),
                    BinaryOp::Ge => matches!(order, Some(Greater | Equal)),
                    BinaryOp::Lt => order == Some(Less),
                    _ => matches!(order, Some(Less | Equal)),
                })
            }
            BinaryOp::Range => match (&left, &right) {
                (Value::Int(a), Value::Int(b)) => Value::List((*a..*b).map(Value::Int).collect()),
                _ => {
                    return Err(RillError::new(format!(
                        "range requires Int..Int, got {}..{}",
                        left.type_name(),
                        right.type_name()
                    ))
                    .into())
                }
            },
            BinaryOp::And | BinaryOp::Or => {
                unreachable!("And/Or handled before eval_binary in eval_expr")
            }
            _ => numeric_binary(left, right, op)?,
        };
        Ok(value)
    }

    fn eval_pipe(&mut self, kind: PipeKind, left: &Expr, right: &Expr) -> Result<Value, EvalError> {
        match kind {
            PipeKind::Value => {
                let left_value = self.eval_expr(left)?;
                self.apply_value_pipe(left_value, right)
            }
            PipeKind::Byte => {
                let mut commands = Vec::new();
                if collect_byte_pipeline_commands(left, &mut commands)
                    && matches!(right.unspanned(), Expr::Command(_))
                {
                    if let Expr::Command(command) = right.unspanned() {
                        commands.push(command);
                    }
                    return self.eval_pipeline_commands(commands);
                }
                let bytes = self.eval_pipeline_bytes(left)?;
                match right.unspanned() {
                    Expr::Command(command) => {
                        let result = self.eval_command(command, Some(bytes))?;
                        Ok(result)
                    }
                    expr if crate::methods::is_lines_consumer(expr) => Ok(Value::List(
                        String::from_utf8_lossy(&bytes)
                            .lines()
                            .map(|line| Value::Str(line.to_string()))
                            .collect(),
                    )),
                    _ => {
                        Err(RillError::new("right side of | must be a command or byte consumer")
                            .into())
                    }
                }
            }
        }
    }

    fn apply_value_pipe(&mut self, left_value: Value, right: &Expr) -> Result<Value, EvalError> {
        self.env.push();
        let result = if crate::methods::is_method_pipeline(right) {
            self.env.set("it".into(), left_value);
            self.eval_expr(right)
        } else {
            self.env.set("$pipe_receiver".into(), left_value);
            let receiver = Expr::Ident("$pipe_receiver".into());
            match right.unspanned() {
                Expr::Call { callee, args } => {
                    let mut all_args = vec![receiver];
                    all_args.extend(args.iter().cloned());
                    self.eval_call(callee, &all_args)
                }
                Expr::Ident(_) => self.eval_call(right, &[receiver]),
                _ => Err(RillError::new("right side of |> must be callable").into()),
            }
        };
        self.env.pop();
        result
    }

    fn eval_pipeline_bytes(&mut self, expr: &Expr) -> Result<Vec<u8>, EvalError> {
        let mut commands = Vec::new();
        if collect_byte_pipeline_commands(expr, &mut commands) {
            return Ok(self.eval_pipeline_commands(commands)?.as_string_lossy().into_bytes());
        }
        match expr {
            Expr::Pipe { kind: PipeKind::Byte, left, right } => {
                let input = self.eval_pipeline_bytes(left)?;
                match right.unspanned() {
                    Expr::Command(command) => {
                        let value = self.eval_command(command, Some(input))?;
                        Ok(value.as_string_lossy().into_bytes())
                    }
                    _ => Err(RillError::new("middle of byte pipeline must be a command").into()),
                }
            }
            Expr::Command(command) => {
                let value = self.eval_command(command, None)?;
                Ok(value.as_string_lossy().into_bytes())
            }
            _ => Ok(self.eval_expr(expr)?.as_string_lossy().into_bytes()),
        }
    }

    fn eval_pipeline_commands(&mut self, commands: Vec<&CommandExpr>) -> Result<Value, EvalError> {
        let mut argv = Vec::with_capacity(commands.len());
        let mut io_configs = Vec::with_capacity(commands.len());
        for command in &commands {
            argv.push(self.command_argv(command)?);
            io_configs.push(self.command_io(command)?);
        }
        let result = self.execute_pipeline(&argv, &io_configs)?;
        Ok(Value::ProcessResult(result))
    }

    fn eval_command(
        &mut self,
        command: &CommandExpr,
        input: Option<Vec<u8>>,
    ) -> Result<Value, EvalError> {
        let argv = self.command_argv(command)?;
        let io = self.command_io(command)?;
        if command.check_exit {
            // For run!, convert both spawn failures and non-zero exits into propagated Err.
            let result = match self.execute_command(&argv, input, &io) {
                Ok(r) => r,
                Err(e) => {
                    let msg = e.to_string();
                    return self.check_exit_error(msg, None, command.span);
                }
            };
            if result.status != 0 {
                let msg = if result.err.is_empty() {
                    format!("command exited with status {}", result.status)
                } else {
                    result.stderr_text_lossy().trim().to_string()
                };
                return self.check_exit_error(msg, Some(result.status), command.span);
            }
            Ok(Value::Str(result.stdout_text_lossy().into_owned()))
        } else {
            Ok(Value::ProcessResult(self.execute_command(&argv, input, &io)?))
        }
    }

    fn check_exit_error(
        &self,
        msg: String,
        status: Option<i32>,
        span: crate::diagnostic::Span,
    ) -> Result<Value, EvalError> {
        let err_val = Value::Result(Err(Box::new(Value::Error(crate::types::ErrorValue {
            kind: crate::types::ErrorKind::Process,
            message: msg.clone(),
            operation: None,
            status,
            span: None,
        }))));
        if self.function_depth > 0 {
            Err(EvalError::Control(Flow::Propagate(err_val)))
        } else {
            Err(RillError::spanned(msg, span).into())
        }
    }

    fn command_argv(&mut self, command: &CommandExpr) -> Result<Vec<String>, EvalError> {
        let mut out = Vec::new();
        for arg in &command.argv {
            match arg {
                CommandArg::Word(value) => {
                    let interpolated = self.interpolate_string(value)?.as_string_lossy();
                    // Expand a leading tilde prefix from the source text.
                    out.push(expand_tilde_prefix(
                        &interpolated,
                        self.environment.get("HOME").map(String::as_str),
                    ));
                }
                CommandArg::String(value) => {
                    out.push(self.interpolate_string(value)?.as_string_lossy());
                }
                CommandArg::Var(name) => {
                    let value = self.env.get(name).ok_or_else(|| {
                        RillError::new(format!("unknown command variable '${name}'"))
                    })?;
                    match value {
                        Value::List(items) => {
                            out.extend(items.into_iter().map(|item| item.as_string_lossy()));
                        }
                        other => out.push(other.as_string_lossy()),
                    }
                }
                CommandArg::Splat(name) => {
                    let value = self.env.get(name).ok_or_else(|| {
                        RillError::new(format!("unknown command variable '${name}'"))
                    })?;
                    match value {
                        Value::List(items) => {
                            out.extend(items.into_iter().map(|item| item.as_string_lossy()));
                        }
                        other => {
                            out.extend(
                                other.as_string_lossy().split_whitespace().map(|w| w.to_string()),
                            );
                        }
                    }
                }
                CommandArg::Parts(parts) => {
                    let mut arg = String::new();
                    let mut is_first = true;
                    for part in parts {
                        match part {
                            CommandPart::Text(text) => {
                                let s = self.interpolate_string(text)?.as_string_lossy();
                                // Expand leading tilde only when it is the very first
                                // fragment of a Parts arg.
                                if is_first {
                                    arg.push_str(&expand_tilde_prefix(
                                        &s,
                                        self.environment.get("HOME").map(String::as_str),
                                    ));
                                } else {
                                    arg.push_str(&s);
                                }
                            }
                            CommandPart::Var(name) => {
                                let value = self.env.get(name).ok_or_else(|| {
                                    RillError::new(format!("unknown command variable '${name}'"))
                                })?;
                                arg.push_str(&value.as_string_lossy());
                            }
                            CommandPart::VarDefault(name, default) => {
                                let val = self.env.get(name);
                                let s = val.as_ref().map(|v| v.as_string_lossy());
                                if s.as_deref().is_some_and(|v| !v.is_empty()) {
                                    arg.push_str(&s.unwrap());
                                } else {
                                    // Expand the default as a path template so that
                                    // `~` and env vars work there too.
                                    let expanded = self
                                        .eval_resolve_path(default)
                                        .map_err(EvalError::from)?
                                        .to_string_lossy()
                                        .into_owned();
                                    arg.push_str(&expanded);
                                }
                            }
                        }
                        is_first = false;
                    }
                    out.push(arg);
                }
                CommandArg::Expr(expr) => out.push(self.eval_expr(expr)?.as_string_lossy()),
            }
        }
        Ok(self.apply_remote_prefix(out))
    }

    fn command_io(&mut self, command: &CommandExpr) -> Result<CommandIo, EvalError> {
        command.validate_config()?;
        let mut io = CommandIo::default();
        for redirect in &command.redirects {
            let target = self.command_arg_to_string(&redirect.target)?;
            let path = self.eval_resolve_path(&target)?;
            match redirect.kind {
                RedirectKind::Stdout => io.stdout_file = Some(path),
                RedirectKind::Stderr => io.stderr_file = Some(path),
                RedirectKind::All => io.all_file = Some(path),
                RedirectKind::Stdin => io.stdin_file = Some(path),
            }
        }
        for (name, expr) in &command.config {
            match name.as_str() {
                "cwd" => io.cwd = Some(self.config_path(expr, "cwd")?),
                "env" => {
                    let value = self.eval_expr(expr)?;
                    let Value::Map(values) = value else {
                        return Err(RillError::new("command env config needs a Map").into());
                    };
                    for (key, value) in values {
                        io.env.insert(key, value.as_string_lossy());
                    }
                }
                "stdin" => match self.config_stream(expr, "stdin")? {
                    ConfigStream::File(path) => io.stdin_file = Some(path),
                    ConfigStream::Mode(StreamMode::Null) => io.stdin_null = true,
                    ConfigStream::Mode(StreamMode::Inherit | StreamMode::Capture) => {}
                },
                "stdout" => match self.config_stream(expr, "stdout")? {
                    ConfigStream::File(path) => io.stdout_file = Some(path),
                    ConfigStream::Mode(mode) => io.stdout_mode = mode,
                },
                "stderr" => match self.config_stream(expr, "stderr")? {
                    ConfigStream::File(path) => io.stderr_file = Some(path),
                    ConfigStream::Mode(mode) => io.stderr_mode = mode,
                },
                _ => {
                    return Err(
                        RillError::new(format!("unknown command config key '{name}'")).into()
                    )
                }
            }
        }
        Ok(io)
    }

    fn config_path(&mut self, expr: &Expr, name: &str) -> Result<PathBuf, EvalError> {
        match expr.unspanned() {
            Expr::Call { callee, args } if matches!(callee.unspanned(), Expr::Ident(value) if value == "file") =>
            {
                if args.len() != 1 {
                    return Err(RillError::new("file needs 1 arg").into());
                }
                let s = self.eval_expr(&args[0])?.as_string_lossy();
                Ok(self.eval_resolve_path(&s)?)
            }
            _ => {
                let value = self.eval_expr(expr)?;
                match value {
                    Value::Path(path) => Ok(path),
                    Value::Str(path) => Ok(self.eval_resolve_path(&path)?),
                    other => Err(RillError::new(format!(
                        "command {name} config needs Str or Path, got {}",
                        other.type_name()
                    ))
                    .into()),
                }
            }
        }
    }

    fn config_stream(&mut self, expr: &Expr, name: &str) -> Result<ConfigStream, EvalError> {
        if matches!(expr.unspanned(), Expr::Literal(Literal::Null)) {
            return Ok(ConfigStream::Mode(StreamMode::Null));
        }
        if let Expr::Ident(value) = expr.unspanned() {
            return match value.as_str() {
                "capture" => Ok(ConfigStream::Mode(StreamMode::Capture)),
                "inherit" => Ok(ConfigStream::Mode(StreamMode::Inherit)),
                "null" => Ok(ConfigStream::Mode(StreamMode::Null)),
                _ => {
                    Err(RillError::new(format!("unknown command {name} stream mode '{value}'"))
                        .into())
                }
            };
        }
        Ok(ConfigStream::File(self.config_path(expr, name)?))
    }

    fn command_arg_to_string(&mut self, arg: &CommandArg) -> Result<String, EvalError> {
        match arg {
            CommandArg::Word(value) | CommandArg::String(value) => {
                Ok(self.interpolate_string(value)?.as_string_lossy())
            }
            CommandArg::Var(name) => {
                let value = self
                    .env
                    .get(name)
                    .ok_or_else(|| RillError::new(format!("unknown command variable '${name}'")))?;
                Ok(value.as_string_lossy())
            }
            CommandArg::Parts(parts) => {
                let mut arg = String::new();
                for part in parts {
                    match part {
                        CommandPart::Text(text) => {
                            arg.push_str(&self.interpolate_string(text)?.as_string_lossy())
                        }
                        CommandPart::Var(name) => {
                            let value = self.env.get(name).ok_or_else(|| {
                                RillError::new(format!("unknown command variable '${name}'"))
                            })?;
                            arg.push_str(&value.as_string_lossy());
                        }
                        CommandPart::VarDefault(name, default) => {
                            let val = self.env.get(name);
                            let s = val.as_ref().map(|v| v.as_string_lossy());
                            if s.as_deref().is_some_and(|v| !v.is_empty()) {
                                arg.push_str(&s.unwrap());
                            } else {
                                let expanded = self
                                    .eval_resolve_path(default)
                                    .map_err(EvalError::from)?
                                    .to_string_lossy()
                                    .into_owned();
                                arg.push_str(&expanded);
                            }
                        }
                    }
                }
                Ok(arg)
            }
            CommandArg::Expr(expr) => Ok(self.eval_expr(expr)?.as_string_lossy()),
            CommandArg::Splat(name) => {
                let value = self
                    .env
                    .get(name)
                    .ok_or_else(|| RillError::new(format!("unknown command variable '${name}'")))?;
                Ok(value.as_string_lossy())
            }
        }
    }
}

fn record_parallel_terminal(
    terminal: &Mutex<Option<ParallelTerminal>>,
    cancellation: &CancellationToken,
    outcome: ParallelTerminal,
) {
    let mut slot = terminal.lock().unwrap_or_else(|error| error.into_inner());
    if slot.is_none() {
        *slot = Some(outcome);
        cancellation.cancel();
    }
}

fn catch_parallel_job<T>(job: impl FnOnce() -> Result<T, EvalError>) -> Result<T, EvalError> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(job)).unwrap_or_else(|_| {
        Err(EvalError::Runtime(
            RillError::new("parallel iteration panicked").with_kind(ErrorKind::Runtime),
        ))
    })
}

fn parallel_terminal_error(outcome: ParallelTerminal) -> EvalError {
    match outcome {
        ParallelTerminal::Runtime(error) => EvalError::Runtime(error),
        ParallelTerminal::Control(flow) => EvalError::Control(flow),
    }
}

const ANSI_RESET: &str = "\x1b[0m";
const ANSI_BLUE: &str = "\x1b[34m";
const ANSI_CYAN: &str = "\x1b[36m";
const ANSI_GREEN: &str = "\x1b[32m";
const ANSI_MAGENTA: &str = "\x1b[35m";
const ANSI_RED: &str = "\x1b[31m";
const ANSI_YELLOW: &str = "\x1b[33m";

fn pretty_color(color: &str, value: impl AsRef<str>) -> String {
    format!("{color}{}{ANSI_RESET}", value.as_ref())
}

fn pretty_indent(level: usize) -> String {
    "  ".repeat(level)
}

fn pretty_string(value: &str) -> String {
    pretty_color(ANSI_GREEN, serde_json::to_string(value).unwrap_or_else(|_| format!("{value:?}")))
}

fn pretty_value(value: &Value, level: usize) -> String {
    match value {
        Value::Null => pretty_color(ANSI_YELLOW, "null"),
        Value::Bool(value) => pretty_color(ANSI_MAGENTA, value.to_string()),
        Value::Int(value) => pretty_color(ANSI_YELLOW, value.to_string()),
        Value::Float(value) => pretty_color(ANSI_YELLOW, value.to_string()),
        Value::Str(value) => pretty_string(value),
        Value::Bytes(value) => pretty_string(&String::from_utf8_lossy(value)),
        Value::List(values) => pretty_collection("[", "]", values, level),
        Value::Tuple(values) => pretty_collection("(", ")", values, level),
        Value::Map(values) => {
            if values.is_empty() {
                return "{}".to_string();
            }
            let entries = values
                .iter()
                .map(|(key, value)| {
                    format!(
                        "{}{}: {}",
                        pretty_indent(level + 1),
                        pretty_color(ANSI_CYAN, serde_json::to_string(key).unwrap()),
                        pretty_value(value, level + 1)
                    )
                })
                .collect::<Vec<_>>();
            format!("{{\n{}\n{}}}", entries.join(",\n"), pretty_indent(level))
        }
        Value::Option(Some(value)) => {
            format!("{}({})", pretty_color(ANSI_BLUE, "some"), pretty_value(value, level))
        }
        Value::Option(None) => pretty_color(ANSI_BLUE, "none"),
        Value::Result(Ok(value)) => {
            format!("{}({})", pretty_color(ANSI_BLUE, "ok"), pretty_value(value, level))
        }
        Value::Result(Err(value)) => {
            format!("{}({})", pretty_color(ANSI_RED, "err"), pretty_value(value, level))
        }
        Value::Error(error) => pretty_color(ANSI_RED, error.message.clone()),
        other => pretty_color(ANSI_BLUE, other.to_string()),
    }
}

fn pretty_collection(open: &str, close: &str, values: &[Value], level: usize) -> String {
    if values.is_empty() {
        return format!("{open}{close}");
    }
    let items = values
        .iter()
        .map(|value| format!("{}{}", pretty_indent(level + 1), pretty_value(value, level + 1)))
        .collect::<Vec<_>>();
    format!("{open}\n{}\n{}{close}", items.join(",\n"), pretty_indent(level))
}

enum ConfigStream {
    Mode(StreamMode),
    File(PathBuf),
}

fn method_kind(value: &Value) -> crate::methods::Kind {
    use crate::methods::Kind;
    match value {
        Value::Null => Kind::Null,
        Value::Str(_) => Kind::Str,
        Value::Float(_) => Kind::Float,
        Value::List(_) => Kind::List,
        Value::Tuple(_) => Kind::Tuple,
        Value::Map(_) => Kind::Map,
        Value::Path(_) => Kind::Path,
        Value::Regex(_) => Kind::Regex,
        Value::Duration(_) => Kind::Duration,
        Value::ProcessHandle(_) => Kind::ProcessHandle,
        Value::Int(_) => Kind::Int,
        Value::Bool(_) => Kind::Bool,
        Value::Function(_) => Kind::Function,
        Value::Option(_) => Kind::Option,
        Value::Result(_) => Kind::Result,
        _ => Kind::Other,
    }
}

fn field_value(base: Value, name: &str) -> Result<Value, RillError> {
    match base {
        Value::Map(map) => {
            map.get(name).cloned().ok_or_else(|| RillError::new(format!("unknown field '{name}'")))
        }
        Value::ProcessResult(result) => match name {
            "status" => Ok(Value::Int(result.status as i64)),
            "out" => Ok(Value::Str(result.stdout_text_lossy().into_owned())),
            "err" => Ok(Value::Str(result.stderr_text_lossy().into_owned())),
            "ok" => Ok(Value::Bool(result.ok())),
            other => Err(RillError::new(format!("unknown ProcessResult field '{other}'"))),
        },
        ref value
            if crate::methods::lookup(name)
                .is_some_and(|method| method.accepts(method_kind(value))) =>
        {
            Err(RillError::new(format!(".{name} is a method; call .{name}(...)")))
        }
        other => Err(RillError::new(format!("{} has no field '{name}'", other.type_name()))),
    }
}

fn numeric_binary(left: Value, right: Value, op: BinaryOp) -> Result<Value, RillError> {
    if op == BinaryOp::Add && matches!((&left, &right), (Value::List(_), Value::List(_))) {
        let (Value::List(mut left), Value::List(right)) = (left, right) else { unreachable!() };
        left.extend(right);
        return Ok(Value::List(left));
    }
    if let (Value::Str(a), Value::Str(b), BinaryOp::Add) = (&left, &right, op) {
        return Ok(Value::Str(format!("{a}{b}")));
    }
    check_numeric_operands(&left, &right, op)?;
    if let (Value::Int(a), Value::Int(b)) = (&left, &right) {
        let value = match op {
            BinaryOp::Add => a.checked_add(*b),
            BinaryOp::Sub => a.checked_sub(*b),
            BinaryOp::Mul => a.checked_mul(*b),
            BinaryOp::Div => a.checked_div(*b),
            _ => unreachable!(),
        };
        return value
            .map(Value::Int)
            .ok_or_else(|| RillError::new("integer overflow or division by zero"));
    }
    let a = as_f64(&left)?;
    let b = as_f64(&right)?;
    if op == BinaryOp::Div && b == 0.0 {
        return Err(RillError::new("division by zero"));
    }
    let value = match op {
        BinaryOp::Add => a + b,
        BinaryOp::Sub => a - b,
        BinaryOp::Mul => a * b,
        BinaryOp::Div => a / b,
        _ => unreachable!(),
    };
    if !value.is_finite() {
        return Err(RillError::new("floating-point overflow"));
    }
    Ok(Value::Float(value))
}

/// Apply `current <op>= rhs` in place.  List and string appends extend the
/// existing value instead of rebuilding it, so repeated appends stay linear.
fn apply_compound(current: &mut Value, rhs: Value, op: AssignOp) -> Result<(), RillError> {
    let bin_op = match op {
        AssignOp::Assign => {
            *current = rhs;
            return Ok(());
        }
        AssignOp::Add => BinaryOp::Add,
        AssignOp::Sub => BinaryOp::Sub,
        AssignOp::Mul => BinaryOp::Mul,
        AssignOp::Div => BinaryOp::Div,
    };
    if bin_op == BinaryOp::Add {
        match current {
            Value::List(items) if matches!(rhs, Value::List(_)) => {
                let Value::List(more) = rhs else { unreachable!() };
                items.extend(more);
                return Ok(());
            }
            Value::Str(text) if matches!(rhs, Value::Str(_)) => {
                let Value::Str(more) = rhs else { unreachable!() };
                text.push_str(&more);
                return Ok(());
            }
            _ => {}
        }
    }
    *current = numeric_binary(current.clone(), rhs, bin_op)?;
    Ok(())
}

fn index_assign_in_place(
    collection: &mut Value,
    idx_val: Value,
    literal_index: Option<i64>,
    op: AssignOp,
    rhs: Value,
) -> Result<(), RillError> {
    match collection {
        Value::Map(map) => {
            let key = match &idx_val {
                Value::Str(s) => s.clone(),
                Value::Int(i) => i.to_string(),
                other => {
                    return Err(RillError::new(format!(
                        "map key must be Str or Int, got {}",
                        other.type_name()
                    )))
                }
            };
            if matches!(op, AssignOp::Assign) {
                map.insert(key, rhs);
                return Ok(());
            }
            let slot = map.entry(key).or_insert(Value::Int(0));
            apply_compound(slot, rhs, op)
        }
        Value::List(items) => {
            let Value::Int(idx) = idx_val else {
                return Err(RillError::new(format!(
                    "list index must be Int, got {}",
                    idx_val.type_name()
                )));
            };
            let len = items.len() as i64;
            let position = if idx < 0 { len + idx } else { idx };
            if position < 0 || position >= len {
                return Err(RillError::new(format!("index {idx} out of bounds (len {len})")));
            }
            apply_compound(&mut items[position as usize], rhs, op)
        }
        Value::Tuple(items) => {
            let Some(literal_index) = literal_index else {
                return Err(RillError::new("tuple assignment index must be an integer literal"));
            };
            let Value::Int(index_value) = idx_val else {
                return Err(RillError::new("tuple index must be Int"));
            };
            debug_assert_eq!(literal_index, index_value);
            let position = normalize_runtime_index(index_value, items.len(), "tuple")?;
            apply_compound(&mut items[position], rhs, op)
        }
        other => Err(RillError::new(format!("cannot index-assign into {}", other.type_name()))),
    }
}

fn check_numeric_operands(left: &Value, right: &Value, op: BinaryOp) -> Result<(), RillError> {
    if matches!(left, Value::Int(_) | Value::Float(_))
        && matches!(right, Value::Int(_) | Value::Float(_))
    {
        return Ok(());
    }
    let operator = match op {
        BinaryOp::Add => "+",
        BinaryOp::Sub => "-",
        BinaryOp::Mul => "*",
        BinaryOp::Div => "/",
        BinaryOp::Gt => ">",
        BinaryOp::Ge => ">=",
        BinaryOp::Lt => "<",
        BinaryOp::Le => "<=",
        _ => unreachable!(),
    };
    Err(RillError::new(format!(
        "operator '{operator}' needs numeric operands, got {} and {}",
        left.type_name(),
        right.type_name()
    )))
}

fn as_f64(value: &Value) -> Result<f64, RillError> {
    match value {
        Value::Int(v) => Ok(*v as f64),
        Value::Float(v) => Ok(*v),
        _ => Err(RillError::new(format!("expected numeric value, got {}", value.type_name()))),
    }
}

fn as_usize(value: &Value) -> Result<usize, RillError> {
    match value {
        Value::Int(v) if *v >= 0 => Ok(*v as usize),
        _ => Err(RillError::new("expected non-negative Int")),
    }
}

fn constant_int_expr(expr: &Expr) -> Option<i64> {
    match expr.unspanned() {
        Expr::Literal(Literal::Int(value)) => Some(*value),
        Expr::Binary { op: BinaryOp::Sub, left, right }
            if matches!(left.unspanned(), Expr::Literal(Literal::Int(0))) =>
        {
            match right.unspanned() {
                Expr::Literal(Literal::Int(value)) => value.checked_neg(),
                _ => None,
            }
        }
        _ => None,
    }
}

fn normalize_runtime_index(index: i64, len: usize, kind: &str) -> Result<usize, RillError> {
    let len_i64 = len as i64;
    let normalized = if index < 0 { len_i64 + index } else { index };
    if normalized < 0 || normalized >= len_i64 {
        Err(RillError::new(format!("{kind} index {index} out of bounds (len {len})")))
    } else {
        Ok(normalized as usize)
    }
}

fn parse_arg_value(
    value: &str,
    type_name: &TypeName,
    lookup: &dyn Fn(&str) -> Option<String>,
) -> Result<Value, RillError> {
    match type_name {
        TypeName::Str => Ok(Value::Str(value.to_string())),
        TypeName::Bool => value
            .parse::<bool>()
            .map(Value::Bool)
            .map_err(|_| RillError::new(format!("expected Bool argument, got '{value}'"))),
        TypeName::Int => value
            .parse::<i64>()
            .map(Value::Int)
            .map_err(|_| RillError::new(format!("expected Int argument, got '{value}'"))),
        TypeName::Float => value
            .parse::<f64>()
            .map(Value::Float)
            .map_err(|_| RillError::new(format!("expected Float argument, got '{value}'"))),
        TypeName::Path => Ok(Value::Path(crate::pathresolver::resolve_path(value, lookup)?)),
        TypeName::Option(inner) => {
            parse_arg_value(value, inner, lookup).map(|value| Value::Option(Some(Box::new(value))))
        }
        other => Err(RillError::new(format!("unsupported CLI argument type {other:?}"))),
    }
}

fn parse_url(value: &str) -> Result<Map, RillError> {
    let (scheme, rest) =
        value.split_once("://").ok_or_else(|| RillError::new("url must include a scheme"))?;
    let (authority, path_query) = rest.split_once('/').unwrap_or((rest, ""));
    let (path, query) = path_query.split_once('?').unwrap_or((path_query, ""));
    let (query, fragment) = query.split_once('#').unwrap_or((query, ""));
    Ok(Map::from_iter([
        ("scheme".to_string(), Value::Str(scheme.to_string())),
        ("host".to_string(), Value::Str(authority.to_string())),
        ("path".to_string(), Value::Str(format!("/{path}"))),
        ("query".to_string(), Value::Str(query.to_string())),
        ("fragment".to_string(), Value::Str(fragment.to_string())),
    ]))
}

fn regex_is_match(value: &str, pattern: &str) -> Result<bool, RillError> {
    let regex = regex::Regex::new(pattern)
        .map_err(|err| RillError::new(format!("invalid regex /{pattern}/: {err}")))?;
    Ok(regex.is_match(value))
}

fn regex_captures(
    value: &str,
    pattern: &str,
) -> Result<Option<BTreeMap<String, String>>, RillError> {
    let regex = regex::Regex::new(pattern)
        .map_err(|err| RillError::new(format!("invalid regex /{pattern}/: {err}")))?;
    let Some(captures) = regex.captures(value) else {
        return Ok(None);
    };
    let mut named = BTreeMap::new();
    for name in regex.capture_names().flatten() {
        if let Some(capture) = captures.name(name) {
            named.insert(name.to_string(), capture.as_str().to_string());
        }
    }
    Ok(Some(named))
}

fn regex_replace(
    value: &str,
    pattern: &str,
    replacement: &str,
    all: bool,
) -> Result<String, RillError> {
    let regex = regex::Regex::new(pattern)
        .map_err(|err| RillError::new(format!("invalid regex /{pattern}/: {err}")))?;
    if all {
        Ok(regex.replace_all(value, replacement).into_owned())
    } else {
        Ok(regex.replace(value, replacement).into_owned())
    }
}

fn simple_glob_with_lookup(
    pattern: &str,
    lookup: &dyn Fn(&str) -> Option<String>,
) -> Result<Vec<PathBuf>, RillError> {
    let resolved = crate::pathresolver::resolve_path(pattern, lookup)?;
    if !resolved.to_string_lossy().chars().any(|c| matches!(c, '*' | '?' | '[')) {
        return Ok(vec![resolved]);
    }
    let pattern_text = resolved.to_string_lossy().into_owned();
    let entries = glob::glob(&pattern_text)
        .map_err(|err| RillError::new(format!("invalid glob '{pattern_text}': {err}")))?;
    entries
        .map(|entry| {
            entry.map_err(|err| {
                RillError::new(format!("failed to read glob entry: {err}")).with_kind(ErrorKind::Io)
            })
        })
        .collect()
}

fn expand_tilde_prefix(s: &str, home: Option<&str>) -> String {
    if s == "~" || s.starts_with("~/") {
        if let Some(home) = home.filter(|home| !home.is_empty()) {
            let suffix = &s[1..];
            return format!("{home}{suffix}");
        }
    } else if let Some(rest) = s.strip_prefix('~') {
        let (user, suffix) = rest.split_once('/').unwrap_or((rest, ""));
        if !user.is_empty()
            && user.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
        {
            if let Some(home) = {
                use crate::pathresolver::UserLookup as _;
                crate::pathresolver::OsUserLookup.home_for(user)
            } {
                return if suffix.is_empty() {
                    home.to_string_lossy().into_owned()
                } else {
                    format!("{}/{suffix}", home.display())
                };
            }
        }
    }
    s.to_string()
}

fn collect_byte_pipeline_commands<'a>(expr: &'a Expr, commands: &mut Vec<&'a CommandExpr>) -> bool {
    match expr.unspanned() {
        Expr::Command(command) => {
            commands.push(command);
            true
        }
        Expr::Pipe { kind: PipeKind::Byte, left, right } => {
            collect_byte_pipeline_commands(left, commands)
                && match right.unspanned() {
                    Expr::Command(command) => {
                        commands.push(command);
                        true
                    }
                    _ => false,
                }
        }
        _ => false,
    }
}

fn contains_implicit(expr: &Expr) -> bool {
    match expr.unspanned() {
        Expr::Implicit | Expr::ImplicitField(_) => true,
        Expr::List(items) => items.iter().any(contains_implicit),
        Expr::Tuple(items) => items.iter().any(contains_implicit),
        Expr::Map(pairs) => pairs.iter().any(|(_, value)| contains_implicit(value)),
        Expr::Call { callee, args } => {
            contains_implicit(callee) || args.iter().any(contains_implicit)
        }
        Expr::Field { base, .. } => contains_implicit(base),
        Expr::Index { base, index } => contains_implicit(base) || contains_implicit(index),
        Expr::Binary { left, right, .. } => contains_implicit(left) || contains_implicit(right),
        Expr::Pipe { left, right, .. } => contains_implicit(left) || contains_implicit(right),
        Expr::RegexMatch { value, .. }
        | Expr::RegexSubstitution { value, .. }
        | Expr::Match { value, .. } => contains_implicit(value),
        Expr::NamedArg { value, .. } => contains_implicit(value),
        Expr::Try(value) => contains_implicit(value),
        Expr::For { iter, .. } => contains_implicit(iter),
        Expr::Block(stmts) => stmts.iter().any(|stmt| match stmt {
            Stmt::Let { expr, .. } | Stmt::Expr(expr) | Stmt::Emit(expr) => contains_implicit(expr),
            Stmt::Assign { expr, .. } => contains_implicit(expr),
            Stmt::IndexAssign { index, expr, .. } => {
                contains_implicit(index) || contains_implicit(expr)
            }
            Stmt::While { cond, .. } => contains_implicit(cond),
            Stmt::If { cond, .. } => contains_implicit(cond),
            Stmt::On { host, .. } => contains_implicit(host),
            Stmt::Return(Some(expr)) => contains_implicit(expr),
            _ => false,
        }),
        Expr::Unary { operand, .. } => contains_implicit(operand),
        Expr::Closure { .. } | Expr::Literal(_) | Expr::Ident(_) | Expr::Command(_) => false,
        Expr::Located { expr, .. } => contains_implicit(expr),
    }
}

fn wrap_declared_result(function: &Function, value: Value) -> Value {
    match function.return_type.as_ref() {
        Some(TypeName::Result(..)) if !matches!(value, Value::Result(_)) => {
            Value::Result(Ok(Box::new(value)))
        }
        Some(TypeName::Result(_, Some(error_type))) if **error_type == TypeName::Error => {
            match value {
                Value::Result(Err(error)) if !matches!(*error, Value::Error(_)) => {
                    Value::Result(Err(Box::new(Value::Error(ErrorValue {
                        kind: ErrorKind::Runtime,
                        message: error.as_string_lossy(),
                        operation: None,
                        status: None,
                        span: None,
                    }))))
                }
                other => other,
            }
        }
        Some(TypeName::Option(_)) if !matches!(value, Value::Option(_)) => {
            Value::Option(Some(Box::new(value)))
        }
        _ => value,
    }
}

fn propagated_value(value: Value) -> Value {
    match value {
        Value::Result(_) | Value::Option(_) => value,
        other => Value::Result(Err(Box::new(other))),
    }
}

fn argv_from_value(value: Value, operation: &str) -> Result<Vec<String>, RillError> {
    let Value::List(items) = value else {
        return Err(RillError::new(format!("{operation} needs List[Str]")));
    };
    if items.is_empty() {
        return Err(RillError::new(format!("{operation} needs a non-empty argv")));
    }
    items
        .into_iter()
        .map(|item| match item {
            Value::Str(value) => Ok(value),
            other => Err(RillError::new(format!(
                "{operation} argv entries must be Str, got {}",
                other.type_name()
            ))),
        })
        .collect()
}
