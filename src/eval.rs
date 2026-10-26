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
use std::sync::{Arc, Mutex, OnceLock, RwLock};
pub use tokio_util::sync::CancellationToken;

/// Stack reserved for the evaluator thread (see `lib::run_compiled`).  Debug
/// builds use far larger frames, so they get twice the reservation.
pub const EVAL_STACK_SIZE: usize = if cfg!(debug_assertions) { 2 << 30 } else { 1 << 30 };
/// Stack reserved for each parallel worker thread.
const WORKER_STACK_SIZE: usize = 256 << 20;
/// Default limit on nested Rill function calls.
const DEFAULT_MAX_CALL_DEPTH: usize = 10_000;
/// Largest list a range expression may produce.
const MAX_RANGE_LEN: i64 = 32 << 20;
/// Largest string `repeat` may produce, in bytes.
const MAX_REPEAT_BYTES: usize = 1 << 30;

fn max_call_depth() -> usize {
    static LIMIT: OnceLock<usize> = OnceLock::new();
    *LIMIT.get_or_init(|| {
        std::env::var("RILL_MAX_CALL_DEPTH")
            .ok()
            .and_then(|value| value.trim().parse::<usize>().ok())
            .filter(|limit| *limit > 0)
            .unwrap_or(DEFAULT_MAX_CALL_DEPTH)
    })
}

#[derive(Clone)]
pub struct ParallelExecutor {
    pool: Arc<rayon::ThreadPool>,
}

impl ParallelExecutor {
    pub fn new(parallelism: NonZeroUsize) -> Result<Self, RillError> {
        rayon::ThreadPoolBuilder::new()
            .num_threads(parallelism.get())
            .stack_size(WORKER_STACK_SIZE)
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
            Binding::cell(Value::Map(
                Map::from_iter([("lines".to_string(), Value::List(lines))]).into(),
            )),
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

    /// Declare an immutable binding (loop variables, destructured patterns).
    fn bind_immutable(&mut self, name: String, value: Value) {
        self.current().insert(name, Binding::value(value));
    }

    fn bind(&mut self, name: String, value: Value, mutable: bool) {
        let binding = if mutable { Binding::cell(value) } else { Binding::value(value) };
        self.current().insert(name, binding);
    }

    /// Add an existing binding (an imported module symbol) to the current scope.
    fn insert_binding(&mut self, name: String, binding: Binding) {
        self.current().insert(name, binding);
    }

    /// The binding for `name` in the innermost scope only.
    fn binding_in_current(&self, name: &str) -> Option<Binding> {
        self.scopes.last().and_then(|scope| scope.get(name).cloned())
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
                return Err(RillError::new(format!("cannot assign to immutable variable '{name}'"))
                    .with_help(format!(
                        "declare it with `var {name} = ...` to allow reassignment"
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
    Break(Value),
    Continue,
    Emit(Value),
}

#[derive(Debug)]
enum EvalError {
    /// Boxed so that `Result<Value, EvalError>` stays small on the hot path.
    Runtime(Box<RillError>),
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
        Self::Runtime(Box::new(error))
    }
}

impl EvalError {
    fn diagnostic(self) -> RillError {
        match self {
            Self::Runtime(error) => *error,
            Self::Control(_) => RillError::new("control flow escaped its evaluation boundary"),
            Self::Cancelled => RillError::new("evaluation cancelled").with_kind(ErrorKind::Runtime),
        }
    }
}

/// Which standard stream a piece of script output was written to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum OutStream {
    Stdout,
    Stderr,
}

/// Receives everything `print`, `prin`, `warn` and `pretty_print` write, and
/// the output of child processes whose inherited streams were captured (see
/// [`IoPolicy`]).  Installed with [`Evaluator::set_output`]; without one, the
/// evaluator writes to the real stdout and stderr.
pub type OutputSink = Arc<dyn Fn(OutStream, &str) + Send + Sync>;

/// Receives what `show` is given (an optional title and the value), so a
/// host with a display can draw records as a live table and anything else
/// as a tree.  Without one, `show` writes a text table to stdout.
pub type ShowSink = Arc<dyn Fn(Option<&str>, &Value) + Send + Sync>;

/// How child processes may touch the host's terminal.  The default changes
/// nothing; an embedding host that owns the terminal (an IDE in raw mode)
/// turns both on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct IoPolicy {
    /// Give children `/dev/null` as stdin unless the script pipes input or
    /// asks for `stdin: inherit` explicitly, instead of the host's stdin.
    pub null_stdin: bool,
    /// Capture `stdout: inherit` / `stderr: inherit` streams and forward them
    /// through the output sink instead of letting the child write to the
    /// terminal.  The script still sees empty `out`/`err` for those streams.
    pub capture_inherited: bool,
}

#[derive(Clone)]
pub struct Evaluator {
    env: Env,
    output: Option<OutputSink>,
    show: Option<ShowSink>,
    io_policy: IoPolicy,
    /// The process environment seen by `env.NAME`, `set_env`, `with env`, and
    /// child processes.  Evaluator-local; the host process is never mutated.
    environment: BTreeMap<String, String>,
    function_depth: usize,
    /// Statements and expressions evaluated since the last cancellation poll;
    /// polling the token locks a mutex, so it is done every `POLL_INTERVAL` steps.
    steps: u32,
    remote_prefix: Vec<RemoteLayer>,
    executor: ParallelExecutor,
    cancellation: CancellationToken,
    cancellation_active: bool,
    /// Record types declared by the program, for checking a Map passed to
    /// a parameter annotated with one.
    records: Arc<BTreeMap<String, crate::records::RecordDecl>>,
}

/// The value of a block used as an expression: `return`, `break` and friends
/// keep unwinding through the enclosing expression.
fn block_value(flow: Flow) -> Result<Value, EvalError> {
    match flow {
        Flow::Value(value) => Ok(value),
        flow => Err(EvalError::Control(flow)),
    }
}

impl Evaluator {
    fn bind_pattern(&mut self, pattern: &LetPattern, value: Value) -> Result<(), EvalError> {
        match pattern {
            LetPattern::Ident(name) => self.env.bind_immutable(name.clone(), value),
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
                        self.env.bind_immutable(
                            name.clone(),
                            items.get(i).cloned().unwrap_or(Value::Null),
                        );
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
                for (name, item) in names.iter().zip(Arc::unwrap_or_clone(items)) {
                    if name != "_" {
                        self.env.bind_immutable(name.clone(), item);
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
                    self.env.bind_immutable(
                        binding.to_string(),
                        map.get(key).cloned().unwrap_or(Value::Null),
                    );
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
                for (name, item) in names.iter().zip(Arc::unwrap_or_clone(items)) {
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
            output: None,
            show: None,
            io_policy: IoPolicy::default(),
            environment,
            function_depth: 0,
            steps: 0,
            remote_prefix: Vec::new(),
            executor,
            cancellation: CancellationToken::new(),
            cancellation_active: false,
            records: Arc::default(),
        }
    }

    /// A child evaluator for a parallel worker.  It shares every binding
    /// visible at the fork point but may not assign into them.
    fn fork(&self, cancellation: CancellationToken) -> Self {
        let mut env = self.env.clone();
        env.frozen = env.scopes.len();
        Self {
            env,
            output: self.output.clone(),
            show: self.show.clone(),
            io_policy: self.io_policy,
            environment: self.environment.clone(),
            function_depth: self.function_depth,
            steps: 0,
            remote_prefix: self.remote_prefix.clone(),
            executor: self.executor.clone(),
            cancellation,
            cancellation_active: true,
            records: self.records.clone(),
        }
    }

    /// Route script output through `sink` instead of the process's stdio.
    pub fn set_output(&mut self, sink: OutputSink) {
        self.output = Some(sink);
    }

    /// Hand `show`'s values to `sink` instead of printing a text table.
    pub fn set_show(&mut self, sink: ShowSink) {
        self.show = Some(sink);
    }

    /// Make this evaluator stop at `token`: statements poll it, `sleep` wakes
    /// up, and child processes are killed.  Tokens are one-shot, so a host
    /// installs a fresh one before every run.
    pub fn set_cancellation(&mut self, token: CancellationToken) {
        self.cancellation = token;
        self.cancellation_active = true;
    }

    pub fn set_io_policy(&mut self, policy: IoPolicy) {
        self.io_policy = policy;
    }

    /// An independent copy of this evaluator's state.  `clone` shares `var`
    /// cells with the original, so a later assignment would show through;
    /// a snapshot copies them, which is what a session that can go back to an
    /// earlier point needs.  Closures created before the snapshot keep
    /// referring to the cells they captured.
    pub fn snapshot(&self) -> Self {
        let mut copy = self.clone();
        for scope in &mut copy.env.scopes {
            let scope = Arc::make_mut(scope);
            for binding in scope.values_mut() {
                if let Slot::Cell(cell) = &binding.slot {
                    let value = cell.read().unwrap_or_else(|error| error.into_inner()).clone();
                    binding.slot = Slot::Cell(Arc::new(RwLock::new(value)));
                }
            }
        }
        copy
    }

    /// Bind `name` in the current (global, for a host) scope to a value the
    /// host made, such as a database handle: `ev.define("db", Value::Db(db))`.
    pub fn define(&mut self, name: impl Into<String>, value: Value) {
        self.env.bind_immutable(name.into(), value);
    }

    /// The current value of a name in scope.
    pub fn binding(&self, name: &str) -> Option<Value> {
        self.env.get(name)
    }

    /// Every global binding with its current value, sorted by name.
    pub fn bindings(&self) -> Vec<(String, Value)> {
        self.binding_names()
            .into_iter()
            .filter_map(|name| self.env.get(&name).map(|value| (name, value)))
            .collect()
    }

    /// Write script output to the sink, or to the real stream without one.
    fn emit(&self, stream: OutStream, text: &str) -> Result<(), RillError> {
        use std::io::Write;
        if let Some(sink) = &self.output {
            sink(stream, text);
            return Ok(());
        }
        let (name, result) = match stream {
            OutStream::Stdout => {
                let mut stdout = std::io::stdout().lock();
                ("stdout", stdout.write_all(text.as_bytes()).and_then(|()| stdout.flush()))
            }
            OutStream::Stderr => {
                let mut stderr = std::io::stderr().lock();
                ("stderr", stderr.write_all(text.as_bytes()).and_then(|()| stderr.flush()))
            }
        };
        result.map_err(|error| RillError::new(format!("failed to write {name}: {error}")))
    }

    /// Which of a command's inherited streams the policy turns into captures.
    fn forced_capture(&self, io: &CommandIo) -> (bool, bool) {
        if !self.io_policy.capture_inherited {
            return (false, false);
        }
        (io.stdout_mode == StreamMode::Inherit, io.stderr_mode == StreamMode::Inherit)
    }

    /// Forward captured-instead-of-inherited output to the sink and hide it
    /// from the script, which expected an inherited stream to capture nothing.
    fn forward_forced(&self, result: &mut crate::types::ProcessResult, forced: (bool, bool)) {
        if forced.0 {
            let _ = self.emit(OutStream::Stdout, &String::from_utf8_lossy(&result.out));
            result.out.clear();
        }
        if forced.1 {
            let _ = self.emit(OutStream::Stderr, &String::from_utf8_lossy(&result.err));
            result.err.clear();
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
        Value::map(
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

    /// Cheap per-step cancellation check: only every `POLL_INTERVAL` steps
    /// touches the token (which takes a lock).
    #[inline]
    fn poll_cancelled(&mut self) -> Result<(), EvalError> {
        const POLL_INTERVAL: u32 = 64;
        self.steps = self.steps.wrapping_add(1);
        if self.steps % POLL_INTERVAL == 0 {
            self.check_cancelled()
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
        ParallelExecutor::new(count).map_err(EvalError::from)
    }

    fn execute_command(
        &self,
        argv: &[String],
        input: Option<Vec<u8>>,
        io: &CommandIo,
    ) -> Result<crate::types::ProcessResult, RillError> {
        let forced = self.forced_capture(io);
        let io = self.command_io_with_environment(io.clone(), input.is_some())?;
        let mut result = if self.cancellation_active {
            run_command_cancellable(argv, input, &io, &self.cancellation)
        } else {
            run_command(argv, input, &io)
        }?;
        self.forward_forced(&mut result, forced);
        Ok(result)
    }

    fn execute_pipeline(
        &self,
        argv: &[Vec<String>],
        io: &[CommandIo],
    ) -> Result<crate::types::ProcessResult, RillError> {
        let forced = io.last().map(|last| self.forced_capture(last)).unwrap_or((false, false));
        let io = io
            .iter()
            .cloned()
            .enumerate()
            .map(|(index, config)| self.command_io_with_environment(config, index > 0))
            .collect::<Result<Vec<_>, _>>()?;
        let mut result = if self.cancellation_active {
            run_pipeline_cancellable(argv, &io, &self.cancellation)
        } else {
            run_pipeline(argv, &io)
        }?;
        self.forward_forced(&mut result, forced);
        Ok(result)
    }

    fn lookup_environment_value(&self, name: &str) -> Option<Value> {
        self.env
            .get(name)
            .or_else(|| self.environment.get(name).map(|value| Value::Str(value.clone())))
    }

    /// Fill in the process environment and apply the [`IoPolicy`].  `piped`
    /// says the child's stdin is already connected to script input or to the
    /// previous pipeline stage.
    fn command_io_with_environment(
        &self,
        mut io: CommandIo,
        piped: bool,
    ) -> Result<CommandIo, RillError> {
        let explicit = std::mem::take(&mut io.env);
        io.env = self.environment.clone();
        io.env.extend(explicit);
        if self.io_policy.null_stdin && !piped && io.stdin_file.is_none() && !io.stdin_inherit {
            io.stdin_null = true;
        }
        if self.io_policy.capture_inherited {
            if io.stdout_mode == StreamMode::Inherit {
                io.stdout_mode = StreamMode::Capture;
            }
            if io.stderr_mode == StreamMode::Inherit {
                io.stderr_mode = StreamMode::Capture;
            }
        }
        Ok(io)
    }

    /// Wrap a local argv in the active `on host { }` layers.
    fn wrap_remote(&self, argv: Vec<String>, stdin_piped: bool) -> Result<Vec<String>, RillError> {
        if self.remote_prefix.is_empty() {
            return Ok(argv);
        }
        let needs_ssh = self.remote_prefix.iter().any(|layer| matches!(layer, RemoteLayer::Ssh(_)));
        let control = if needs_ssh { ssh_control_options()? } else { Vec::new() };
        Ok(wrap_remote_layers(&self.remote_prefix, argv, stdin_piped, &control))
    }

    /// Resolve a path template using Rill bindings first, then process env.
    fn eval_resolve_path(&self, template: &str) -> Result<PathBuf, RillError> {
        crate::pathresolver::resolve_path(template, &|name| {
            self.lookup_environment_value(name).map(|v| v.as_string_lossy())
        })
    }

    /// Names bound in the global scope, sorted (for the REPL's `:env`).
    pub fn binding_names(&self) -> Vec<String> {
        let mut names: Vec<String> = self
            .env
            .scopes
            .first()
            .map(|scope| scope.keys().cloned().collect())
            .unwrap_or_default();
        names.sort();
        names
    }

    pub fn eval_program(&mut self, program: &Program) -> Result<Value, RillError> {
        self.eval_program_with_imports(program, Vec::new())
    }

    /// Run the root program with the bindings exported by its imports.
    pub(crate) fn eval_program_with_imports(
        &mut self,
        program: &Program,
        imports: Vec<(String, Binding)>,
    ) -> Result<Value, RillError> {
        for (name, binding) in imports {
            self.env.insert_binding(name, binding);
        }
        if let Some(group) = program.stmts.iter().find_map(|stmt| match stmt.unspanned() {
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
            Flow::Break(_) => Err(RillError::new("break outside loop")),
            Flow::Continue => Err(RillError::new("continue outside loop")),
            Flow::Emit(_) => Err(RillError::new("emit outside for expression")),
        }
    }

    /// Run a module in its own scope (over the global one) with the bindings
    /// it imports, and return the bindings it exports: its top-level `let`
    /// values and `fn` definitions, never its `var`s or private helpers of
    /// other modules.
    pub(crate) fn eval_module(
        &mut self,
        program: &Program,
        imports: Vec<(String, Binding)>,
    ) -> Result<Vec<(String, Binding)>, RillError> {
        self.env.push();
        for (name, binding) in imports {
            self.env.insert_binding(name, binding);
        }
        let flow = self.eval_stmts(&program.stmts).map_err(EvalError::diagnostic);
        let exports = crate::typeck::top_level_names(&program.stmts)
            .into_iter()
            .filter_map(|name| {
                self.env.binding_in_current(&name).and_then(|binding| match binding.slot {
                    Slot::Value(_) | Slot::Hoisted(_) => Some((name, binding)),
                    Slot::Cell(_) => None,
                })
            })
            .collect();
        self.env.pop();
        match flow? {
            Flow::Value(_) | Flow::Return(_) => Ok(exports),
            Flow::Propagate(value) => Err(RillError::new(format!(
                "cannot propagate {} outside a function",
                value.type_name()
            ))),
            Flow::Break(_) => Err(RillError::new("break outside loop")),
            Flow::Continue => Err(RillError::new("continue outside loop")),
            Flow::Emit(_) => Err(RillError::new("emit outside for expression")),
        }
    }

    /// Reserve slots for every `fn` in `stmts` so that functions can call
    /// each other regardless of declaration order.
    fn hoist_functions(&mut self, stmts: &[Stmt]) {
        for stmt in stmts {
            match stmt.unspanned() {
                Stmt::Fn { name, .. } => self.env.hoist(name),
                Stmt::TypeDecl { name, fields } => {
                    let decl =
                        crate::records::RecordDecl { name: name.clone(), fields: fields.clone() };
                    Arc::make_mut(&mut self.records).insert(name.clone(), decl);
                }
                _ => {}
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
                                EvalError::Runtime(error) => *error,
                                other => RillError::new(format!("{other:?}")),
                            }),
                        );
                        return None;
                    }
                    match catch_parallel_job(|| child.eval_stmts_collecting_emits(body)) {
                        Ok((Flow::Value(_), emits))
                        | Ok((Flow::Break(_), emits))
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
                                ParallelTerminal::Runtime(*error),
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
                        EvalError::Runtime(error) => *error,
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
                Ok(Flow::Break(_)) | Ok(Flow::Continue) => return,
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
                    state.finish(RaceOutcome::Terminal(ParallelTerminal::Runtime(*error)));
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
        // Kept small on purpose: it strips the span wrapper and delegates, so a
        // nested Rill call costs one large frame here instead of two.
        match stmt {
            Stmt::Located { stmt, span } => {
                self.eval_stmt_inner(stmt).map_err(|error| match error {
                    EvalError::Runtime(error) => {
                        EvalError::Runtime(Box::new(error.with_span(*span)))
                    }
                    other => other,
                })
            }
            other => self.eval_stmt_inner(other),
        }
    }

    fn eval_stmt_inner(&mut self, stmt: &Stmt) -> Result<Flow, EvalError> {
        self.poll_cancelled()?;
        match stmt {
            Stmt::Located { .. } => self.eval_stmt(stmt),
            Stmt::Use(module) => {
                let _ = module;
                Ok(Flow::Value(Value::Null))
            }
            Stmt::TypeDecl { .. } => Ok(Flow::Value(Value::Null)),
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
                        for (name, item) in names.iter().zip(Arc::unwrap_or_clone(items)) {
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
            Stmt::Assign { target, op, expr } => {
                let rhs = self.eval_expr(expr)?;
                let op = *op;
                let (root, steps) = self.resolve_place(target)?;
                let updated =
                    self.env.update(root, |value| assign_place(value, &steps, op, rhs))?;
                if !updated {
                    return Err(EvalError::Runtime(Box::new(RillError::new(format!(
                        "assignment to undeclared variable '{root}'"
                    )))));
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
            Stmt::Return(expr) => {
                let value = match expr {
                    Some(expr) => self.eval_expr(expr)?,
                    None => Value::Null,
                };
                Ok(Flow::Return(value))
            }
            Stmt::Continue => Ok(Flow::Continue),
            Stmt::Break(value) => {
                let value = match value {
                    Some(expr) => self.eval_expr(expr)?,
                    None => Value::Null,
                };
                Ok(Flow::Break(value))
            }
            Stmt::While { cond, body } => {
                loop {
                    if !self.eval_expr(cond)?.truthy() {
                        break;
                    }
                    match self.eval_stmts(body)? {
                        Flow::Break(_) => break,
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
        // Small wrapper for the same reason as `eval_stmt`.
        match expr {
            Expr::Located { expr, span } => {
                self.eval_expr_inner(expr).map_err(|error| match error {
                    EvalError::Runtime(error) => {
                        EvalError::Runtime(Box::new(error.with_span(*span)))
                    }
                    flow => flow,
                })
            }
            other => self.eval_expr_inner(other),
        }
    }

    fn eval_expr_inner(&mut self, expr: &Expr) -> Result<Value, EvalError> {
        self.poll_cancelled()?;
        match expr {
            Expr::Located { .. } => self.eval_expr(expr),
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
                .map(Value::list),
            Expr::Tuple(items) => items
                .iter()
                .map(|item| self.eval_expr(item))
                .collect::<Result<Vec<_>, _>>()
                .map(Value::tuple),
            Expr::Map(pairs) => {
                let mut map = Map::new();
                for (key, expr) in pairs {
                    map.insert(key.clone(), self.eval_expr(expr)?);
                }
                Ok(Value::Map(map.into()))
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
                        items.get(i as usize).cloned().ok_or_else(|| {
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
            Expr::Slice { base, start, end, inclusive } => {
                let base_val = self.eval_expr(base)?;
                let mut bounds = [None, None];
                for (slot, bound) in bounds.iter_mut().zip([start, end]) {
                    if let Some(bound) = bound {
                        match self.eval_expr(bound)? {
                            Value::Int(value) => *slot = Some(value),
                            other => {
                                return Err(RillError::new(format!(
                                    "slice bounds must be Int, got {}",
                                    other.type_name()
                                ))
                                .into())
                            }
                        }
                    }
                }
                match base_val {
                    Value::List(items) => {
                        let (from, to) =
                            slice_bounds(items.len(), bounds[0], bounds[1], *inclusive);
                        Ok(Value::List(items[from..to.max(from)].to_vec().into()))
                    }
                    Value::Str(text) => {
                        let chars: Vec<char> = text.chars().collect();
                        let (from, to) =
                            slice_bounds(chars.len(), bounds[0], bounds[1], *inclusive);
                        Ok(Value::Str(chars[from..to.max(from)].iter().collect()))
                    }
                    other => {
                        Err(RillError::new(format!("cannot slice {}", other.type_name())).into())
                    }
                }
            }
            Expr::Field { base, name } => {
                if let Expr::Ident(module) = base.unspanned() {
                    if crate::methods::is_namespace(module) && self.env.get(module).is_none() {
                        if crate::methods::namespace_function(module, name).is_some() {
                            return Err(RillError::new(format!(
                                "{module}.{name} must be called: {module}.{name}()"
                            ))
                            .into());
                        }
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
                                        default: None,
                                    }],
                                    return_type: None,
                                    body: vec![Stmt::Return(Some(call))],
                                    propagation: PropagationCell::new(Propagation::None),
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
            Expr::Binary { op: BinaryOp::Coalesce, left, right } => match self.eval_expr(left)? {
                Value::Null | Value::Option(None) => self.eval_expr(right),
                Value::Option(Some(value)) => Ok(*value),
                other => Ok(other),
            },
            Expr::Binary { op, left, right } => self.eval_binary(*op, left, right),
            Expr::Unary { op: UnaryOp::Not, operand } => {
                let v = self.eval_expr(operand)?;
                Ok(Value::Bool(!is_truthy(&v)?))
            }
            Expr::Unary { op: UnaryOp::Neg, operand } => match self.eval_expr(operand)? {
                Value::Int(value) => value
                    .checked_neg()
                    .map(Value::Int)
                    .ok_or_else(|| RillError::new("integer overflow").into()),
                Value::Float(value) => Ok(Value::Float(-value)),
                other => Err(RillError::new(format!(
                    "unary minus needs a number, got {}",
                    other.type_name()
                ))
                .into()),
            },
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
                                    Flow::Break(_) => break,
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
                    return Ok(Value::List(collected.into()));
                }
                let list = match iter_val {
                    Value::List(items) => items,
                    Value::Map(map) => Arc::unwrap_or_clone(map)
                        .into_iter()
                        .map(|(key, value)| Value::Tuple(vec![Value::Str(key), value].into()))
                        .collect(),
                    other => {
                        return Err(RillError::new(format!(
                            "for loop needs List, Map, or Stream, got {}",
                            other.type_name()
                        ))
                        .into())
                    }
                };
                match mode {
                    ForMode::Sequential => {
                        let mut collected: Vec<Value> = Vec::new();
                        // One scope serves every iteration: the loop variable is
                        // rebound in place, and scopes are copy-on-write, so a
                        // closure created in an earlier iteration keeps its own
                        // snapshot.  This avoids allocating a scope per iteration.
                        self.scoped(|evaluator| {
                            for item in Arc::unwrap_or_clone(list) {
                                evaluator.bind_pattern(pattern, item)?;
                                let (flow, emits) = evaluator.eval_stmts_collecting_emits(body)?;
                                collected.extend(emits);
                                match flow {
                                    Flow::Value(_) | Flow::Continue => {}
                                    Flow::Break(_) => break,
                                    Flow::Return(v) => {
                                        return Err(EvalError::Control(Flow::Return(v)))
                                    }
                                    Flow::Propagate(v) => {
                                        return Err(EvalError::Control(Flow::Propagate(v)))
                                    }
                                    Flow::Emit(_) => unreachable!(),
                                }
                            }
                            Ok(())
                        })?;
                        Ok(Value::List(collected.into()))
                    }
                    ForMode::Parallel => {
                        self.eval_for_parallel(workers.as_deref(), pattern, (*list).clone(), body)
                    }
                    ForMode::Race => {
                        self.eval_for_race(workers.as_deref(), pattern, (*list).clone(), body)
                    }
                }
            }
            Expr::If { cond, pattern, then_body, else_body } => {
                let flow = self.eval_if(cond, pattern.as_ref(), then_body, else_body)?;
                match flow {
                    Flow::Value(value) => Ok(value),
                    flow => Err(EvalError::Control(flow)),
                }
            }
            Expr::Loop { body } => loop {
                self.check_cancelled()?;
                let flow = self.scoped(|evaluator| evaluator.eval_stmts(body))?;
                match flow {
                    Flow::Break(value) => return Ok(value),
                    Flow::Continue | Flow::Value(_) => {}
                    flow => return Err(EvalError::Control(flow)),
                }
            },
            Expr::Block(stmts) => {
                self.env.push();
                let flow = self.eval_stmts(stmts);
                self.env.pop();
                match flow? {
                    Flow::Value(value) => Ok(value),
                    flow => Err(EvalError::Control(flow)),
                }
            }
            Expr::WithEnv { bindings, body } => {
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
                block_value(result?)
            }
            Expr::On { host, body } => {
                let host_val = self.eval_expr(host)?;
                let prefix = match host_val {
                    Value::Str(host) => {
                        validate_remote_name("ssh host", &host)?;
                        RemoteLayer::Ssh(host)
                    }
                    Value::List(items) => {
                        if items.is_empty() {
                            return Err(RillError::new("on needs a nonempty command prefix").into());
                        }
                        RemoteLayer::Exec(
                            Arc::unwrap_or_clone(items)
                                .into_iter()
                                .map(|v| v.as_string_lossy())
                                .collect(),
                        )
                    }
                    other => {
                        return Err(RillError::new(format!(
                            "on needs Str or List[Str], got {}",
                            other.type_name()
                        ))
                        .into())
                    }
                };
                self.remote_prefix.push(prefix);
                let result = self.scoped(|evaluator| evaluator.eval_stmts(body));
                self.remote_prefix.pop();
                block_value(result?)
            }
            Expr::Closure { params, body, propagation } => Ok(self.make_function(
                Function {
                    params: params
                        .iter()
                        .map(|name| Param { name: name.clone(), type_name: None, default: None })
                        .collect(),
                    return_type: None,
                    body: body.clone(),
                    propagation: propagation.clone(),
                },
                None,
            )),
            Expr::Interpolated(parts) => Ok(Value::Str(self.eval_parts(parts)?)),
            Expr::Try(inner) => match self.eval_expr(inner)? {
                Value::Option(Some(value)) => Ok(*value),
                Value::Option(None) => self.propagate(Value::Option(None)),
                Value::Result(Ok(value)) => Ok(*value),
                Value::Result(Err(error)) => self.propagate(Value::Result(Err(error))),
                other => Err(RillError::new(format!(
                    "? needs Option or Result, got {}",
                    other.type_name()
                ))
                .with_help(format!(
                    "the value is {}\n? unwraps an Option or Result; it does not test whether a field exists (use .get(\"name\") for that)",
                    preview_value(&other)
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

    fn eval_if(
        &mut self,
        cond: &Expr,
        pattern: Option<&LetPattern>,
        then_body: &[Stmt],
        else_body: &[Stmt],
    ) -> Result<Flow, EvalError> {
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
            let flow =
                if cond.truthy() { self.eval_stmts(then_body) } else { self.eval_stmts(else_body) };
            self.env.pop();
            flow
        }
    }

    fn eval_literal(&mut self, value: &Literal) -> Result<Value, EvalError> {
        match value {
            Literal::Str(text) => Ok(Value::Str(text.clone())),
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
            Some(Value::List(values)) => Arc::unwrap_or_clone(values)
                .into_iter()
                .map(|value| value.as_string_lossy())
                .collect::<Vec<_>>(),
            _ => Vec::new(),
        };
        let values = self.parse_arg_group(group, &raw)?;
        self.env.set("args".to_string(), Value::Map(values.into()));
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
                    .ok_or_else(|| usage_error(format!("unknown option '--{name}'")))?;
                if matches!(field.type_name, TypeName::Bool) {
                    values.insert(field.name.clone(), Value::Bool(true));
                } else {
                    index += 1;
                    let value = raw
                        .get(index)
                        .ok_or_else(|| usage_error(format!("option '--{name}' needs a value")))?;
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
                    .ok_or_else(|| usage_error(format!("unknown option '-{short}'")))?;
                if matches!(field.type_name, TypeName::Bool) {
                    values.insert(field.name.clone(), Value::Bool(true));
                } else {
                    index += 1;
                    let value = raw
                        .get(index)
                        .ok_or_else(|| usage_error(format!("option '-{short}' needs a value")))?;
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
                    return Err(
                        usage_error(format!("missing required argument '{}'", field.name)).into()
                    );
                }
            }
        }
        if let Some((command, next_index)) = child {
            let command_values = self.parse_arg_group(&command, &raw[next_index..])?;
            values.insert(
                "command".to_string(),
                Value::map(Map::from_iter([(command.name.clone(), Value::map(command_values))])),
            );
        }
        if position < positional.len() {
            return Err(usage_error("too many positional arguments").into());
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

    /// Render the parts of an interpolated string.
    fn eval_parts(&mut self, parts: &[StrPart]) -> Result<String, EvalError> {
        let mut out = String::new();
        for part in parts {
            match part {
                StrPart::Text(text) => out.push_str(text),
                StrPart::Expr(expr) => {
                    let value = self.eval_expr(expr)?;
                    out.push_str(&interpolated_text(&value));
                }
                StrPart::Var(name) => {
                    let value = self.lookup_environment_value(name).ok_or_else(|| {
                        RillError::new(format!("unknown variable '${name}'"))
                            .with_help("`$name` looks up a Rill binding, then the environment")
                    })?;
                    out.push_str(&interpolated_text(&value));
                }
                StrPart::VarDefault(name, default) => {
                    let value = self.lookup_environment_value(name).map(|v| interpolated_text(&v));
                    match value {
                        Some(text) if !text.is_empty() => out.push_str(&text),
                        _ => {
                            let text = self.eval_parts(default)?;
                            out.push_str(&text);
                        }
                    }
                }
            }
        }
        Ok(out)
    }

    /// Evaluate an assignment target into its root variable and the field/index
    /// steps leading to the assigned slot.  Index expressions are evaluated
    /// left to right before any mutation happens.
    fn resolve_place<'a>(
        &mut self,
        place: &'a Expr,
    ) -> Result<(&'a str, Vec<PlaceStep>), EvalError> {
        match place.unspanned() {
            Expr::Ident(name) => Ok((name.as_str(), Vec::new())),
            Expr::Field { base, name } => {
                let (root, mut steps) = self.resolve_place(base)?;
                steps.push(PlaceStep::Field(name.clone()));
                Ok((root, steps))
            }
            Expr::Index { base, index } => {
                let (root, mut steps) = self.resolve_place(base)?;
                let key = self.eval_expr(index)?;
                steps.push(PlaceStep::Index { key, literal: constant_int_expr(index) });
                Ok((root, steps))
            }
            _ => Err(RillError::new("invalid assignment target").into()),
        }
    }

    /// Resolve an expression to a path.  A leading `~` is expanded only in
    /// source text (string literals and interpolations, possibly wrapped in
    /// `path(...)` or `file(...)`); runtime values are taken literally.
    fn path_from_expr(&mut self, expr: &Expr) -> Result<PathBuf, EvalError> {
        if let Expr::Call { callee, args } = expr.unspanned() {
            if args.len() == 1
                && matches!(callee.unspanned(), Expr::Ident(name) if name == "file" || name == "path")
            {
                return self.path_from_expr(&args[0]);
            }
        }
        let source_text = is_source_text(expr);
        match self.eval_expr(expr)? {
            Value::Path(path) => Ok(path),
            Value::Str(text) => Ok(self.path_from_text(&text, source_text)?),
            other => {
                Err(RillError::new(format!("expected a Str or Path, got {}", other.type_name()))
                    .into())
            }
        }
    }

    fn path_from_text(&self, text: &str, expand_tilde: bool) -> Result<PathBuf, RillError> {
        if expand_tilde {
            self.eval_resolve_path(text)
        } else {
            Ok(PathBuf::from(text))
        }
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
            let mut bindings = Vec::new();
            if !match_pattern(&arm.pattern, &val, &mut bindings)? {
                continue;
            }
            let result = self.scoped(|evaluator| {
                for (name, bound) in bindings {
                    evaluator.env.set(name, bound);
                }
                if let Some(guard) = &arm.guard {
                    if !evaluator.eval_expr(guard)?.truthy() {
                        return Ok(None);
                    }
                }
                evaluator.eval_expr(&arm.expr).map(Some)
            })?;
            if let Some(result) = result {
                return Ok(result);
            }
        }
        Ok(Value::Null)
    }

    fn eval_call(&mut self, callee: &Expr, args: &[Expr]) -> Result<Value, EvalError> {
        if let Expr::Ident(name) = callee.unspanned() {
            if let Some(method) = crate::methods::moved_function(name) {
                if let Some(function) = self.env.get(name) {
                    let (positional, named) = self.eval_call_args(args)?;
                    return self.call_callable_named(function, positional, named);
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
                    if let Some(function) = crate::methods::namespace_function(module, name) {
                        return self.eval_namespace_function(function, args);
                    }
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
        let (positional, named) = self.eval_call_args(args)?;
        self.call_callable_named(value, positional, named)
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
                    Value::Str(path) => Value::Path(
                        self.path_from_text(path, is_source_text(&args[0]))
                            .map_err(EvalError::from)?,
                    ),
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
        #[cfg(feature = "postgres")]
        if let Value::Db(db) = &receiver {
            return self.eval_sql_method(db.clone(), name, args);
        }
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
                    let expected = crate::methods::callback_arity(name, kind);
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
                        result.map(Box::new).map_err(|err| Box::new(Value::Error(Box::new(err)))),
                    ))
                }
                Target::Structural(_) => unreachable!("structural methods dispatch before binding"),
                Target::Sql => Err(RillError::new(format!(
                    ".{name}() needs a Db from sql.connect(..), got {}",
                    receiver.type_name()
                ))
                .into()),
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
            (Structural::AndThen, Value::Option(Some(value)))
            | (Structural::AndThen, Value::Result(Ok(value))) => {
                let result = self.apply_unary(&args[0], *value)?;
                if !matches!(result, Value::Option(_) | Value::Result(_)) {
                    return Err(RillError::new(format!(
                        "and_then callback must return an Option or Result, got {}",
                        result.type_name()
                    ))
                    .into());
                }
                Ok(result)
            }
            (Structural::AndThen, other) => Ok(other),
            (Structural::MapErr, Value::Result(Err(error))) => {
                Ok(Value::Result(Err(Box::new(self.apply_unary(&args[0], *error)?))))
            }
            (Structural::MapErr, other) => Ok(other),
            (Structural::UnwrapOrElse, Value::Option(Some(value)))
            | (Structural::UnwrapOrElse, Value::Result(Ok(value))) => Ok(*value),
            (Structural::UnwrapOrElse, Value::Option(None)) => {
                let callback = self.eval_expr(&args[0])?;
                self.call_callable(callback, Vec::new())
            }
            (Structural::UnwrapOrElse, Value::Result(Err(error))) => {
                self.apply_unary(&args[0], *error)
            }
            (Structural::OkOr, Value::Option(Some(value))) => Ok(Value::Result(Ok(value))),
            (Structural::OkOr, Value::Option(None)) => {
                Ok(Value::Result(Err(Box::new(self.eval_expr(&args[0])?))))
            }
            (Structural::Expect, Value::Option(Some(value)))
            | (Structural::Expect, Value::Result(Ok(value))) => Ok(*value),
            (Structural::Expect, Value::Option(None)) => {
                Err(RillError::new(self.eval_expr(&args[0])?.as_string_lossy()).into())
            }
            (Structural::Expect, Value::Result(Err(error))) => {
                let message = self.eval_expr(&args[0])?.as_string_lossy();
                Err(RillError::new(format!("{message}: {error}")).into())
            }
            (Structural::DurationSecs, Value::Duration(d)) => Ok(Value::Int(d.as_secs() as i64)),
            (Structural::DurationMillis, Value::Duration(d)) => {
                Ok(Value::Int(d.as_millis().min(i64::MAX as u128) as i64))
            }
            (Structural::DurationMicros, Value::Duration(d)) => {
                Ok(Value::Int(d.as_micros().min(i64::MAX as u128) as i64))
            }
            (Structural::DurationNanos, Value::Duration(d)) => {
                Ok(Value::Int(d.as_nanos().min(i64::MAX as u128) as i64))
            }
            (Structural::DurationSecsFloat, Value::Duration(d)) => {
                Ok(Value::Float(d.as_secs_f64()))
            }
            (Structural::HumanSize, Value::Int(bytes)) => {
                Ok(Value::Str(crate::types::format_size(bytes)))
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
                    Value::List(items) => Arc::unwrap_or_clone(items)
                        .into_iter()
                        .map(|v| v.as_string_lossy())
                        .collect(),
                    other => {
                        return Err(RillError::new(format!(
                            "Stream.tail hosts must be Str or List, got {}",
                            other.type_name()
                        ))
                        .into())
                    }
                };
                let path = self.eval_expr(&args[1])?.as_string_lossy();
                let handle = crate::runtime::spawn_tail(hosts, path).map_err(EvalError::from)?;
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
                let child = self.eval_expr(&args[1])?.as_string_lossy();
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
                Ok(Value::Map(parsed.into()))
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
                Ok(Value::List(entries.into()))
            }
            ("archive", "extract") => {
                if args.len() != 2 {
                    return Err(RillError::new("archive.extract needs 2 args").into());
                }
                let archive = self.eval_expr(&args[0])?.as_string_lossy();
                let destination = self.path_from_expr(&args[1])?;
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
                Ok(Value::Map(map.into()))
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
                Ok(Value::List(entries.into()))
            }
            ("process", "run") => {
                let raw = argv_from_value(self.eval_one_arg("process.run", args)?, "process.run")?;
                let argv = self.wrap_remote(raw, false)?;
                Ok(self.execute_command(&argv, None, &CommandIo::default()).map(Value::process)?)
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
                let receiver = self.eval_expr(&args[0])?;
                let key = self.eval_expr(&args[1])?;
                match receiver {
                    Value::Map(values) => {
                        Ok(values.get(&key.as_string_lossy()).cloned().unwrap_or(Value::Null))
                    }
                    Value::Null => Ok(Value::Null),
                    Value::List(items) => {
                        let Value::Int(index) = key else {
                            return Err(RillError::new(format!(
                                "list get needs an Int index, got {}",
                                key.type_name()
                            ))
                            .into());
                        };
                        let len = items.len() as i64;
                        let position = if index < 0 { len + index } else { index };
                        let found = usize::try_from(position).ok().and_then(|p| items.get(p));
                        Ok(Value::Option(found.cloned().map(Box::new)))
                    }
                    other => Err(RillError::new(format!(
                        "get needs a Map or List, got {}",
                        other.type_name()
                    ))
                    .into()),
                }
            }
            "spawn" => {
                let value = self.eval_one_arg("spawn", args)?;
                let argv = self.wrap_remote(argv_from_value(value, "spawn")?, false)?;
                self.check_cancelled()?;
                let handle = TokioProcessRuntime.spawn(ProcessSpec {
                    argv,
                    input: None,
                    io: self.command_io_with_environment(CommandIo::default(), false)?,
                    cancellation: None,
                })?;
                Ok(Value::ProcessHandle(handle))
            }
            "await" => {
                let value = self.eval_one_arg("await", args)?;
                let Value::ProcessHandle(handle) = value else {
                    return Err(RillError::new("await needs ProcessHandle").into());
                };
                // Poll so a cancelled race or parallel worker stops waiting; the
                // process itself is only killed when its last handle is dropped.
                while self.cancellation_active && !TokioProcessRuntime.is_finished(&handle) {
                    self.check_cancelled()?;
                    std::thread::sleep(std::time::Duration::from_millis(5));
                }
                Ok(Value::ProcessResult((TokioProcessRuntime.await_process(handle)?).into()))
            }
            "parallel" => {
                let value = self.eval_one_arg("parallel", args)?;
                let Value::List(commands) = value else {
                    return Err(RillError::new("parallel needs List[List[Str]]").into());
                };
                let specs = Arc::unwrap_or_clone(commands)
                    .into_iter()
                    .map(|command| {
                        Ok(ProcessSpec {
                            argv: self.wrap_remote(argv_from_value(command, "parallel")?, false)?,
                            input: None,
                            io: self.command_io_with_environment(CommandIo::default(), false)?,
                            cancellation: self
                                .cancellation_active
                                .then(|| self.cancellation.child_token()),
                        })
                    })
                    .collect::<Result<Vec<_>, EvalError>>()?;
                self.check_cancelled()?;
                let limit = self.executor.parallelism().max(8);
                Ok(Value::List(
                    TokioProcessRuntime
                        .parallel(specs, limit)?
                        .into_iter()
                        .map(Value::process)
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
                Ok(Value::List(Arc::unwrap_or_clone(items).into_iter().take(count).collect()))
            }
            "reverse" => {
                let mut value = self.eval_one_arg("reverse", args)?;
                let Value::List(items) = &mut value else {
                    return Err(RillError::new("reverse needs a List").into());
                };
                Arc::make_mut(items).reverse();
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
            "parse_xml" => {
                let text = self.eval_expr(&args[0])?.as_string_lossy();
                Ok(crate::xml::parse(&text)?)
            }
            "select" | "select_one" => {
                let receiver = self.eval_expr(&args[0])?;
                let path = match args.get(1) {
                    Some(path) => self.eval_expr(path)?.as_string_lossy(),
                    None => return Err(RillError::new(format!("{name} needs a path")).into()),
                };
                let matches = crate::xml::select(&receiver, &path, name)?;
                Ok(if name == "select" {
                    Value::list(matches)
                } else {
                    Value::Option(matches.into_iter().next().map(Box::new))
                })
            }
            "to_xml" => {
                let receiver = self.eval_expr(&args[0])?;
                Ok(Value::Str(crate::xml::to_xml(&receiver)?))
            }
            "parse_time" => {
                let text = self.eval_expr(&args[0])?.as_string_lossy();
                let format = match args.get(1) {
                    Some(format) => Some(self.eval_expr(format)?.as_string_lossy()),
                    None => None,
                };
                Ok(Value::Time(parse_time(&text, format.as_deref())?))
            }
            "year" | "month" | "day" | "hour" | "minute" | "second" | "weekday" | "unix"
            | "date" | "iso" | "format" | "age" => self.eval_time_method(name, args),
            "index_by" => self.eval_index_by(args),
            "sort_by" => self.eval_sort_by(args),
            "unique" => self.eval_unique(args),
            "all" | "count" => self.eval_predicate_fold(name, args),
            "sum" => self.eval_sum(args),
            "servers"
            | "volumes"
            | "ports"
            | "networks"
            | "subnets"
            | "images"
            | "flavors"
            | "hypervisors"
            | "projects"
            | "users"
            | "loadbalancers"
            | "amphorae"
            | "list"
            | "inventory"
            | "floating_ips"
            | "security_groups"
            | "routers"
            | "attached_volumes"
            | "attached_servers"
            | "project"
            | "server"
            | "network"
            | "port"
            | "hypervisor"
            | "created_servers"
            | "project_memberships"
            | "keypairs"
            | "server_groups"
            | "aggregates"
            | "availability_zones"
            | "compute_services"
            | "network_agents"
            | "snapshots"
            | "backups"
            | "pools"
            | "role_assignments"
            | "flavor"
            | "image"
            | "actions"
            | "compute_service"
            | "quota"
            | "volume"
            | "members" => self.eval_ostack_map_method(name, args),
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
                    Value::Path(base) => Ok(Value::Path(base.join(arg.as_string_lossy()))),
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
                    Value::List(items) => {
                        Ok(Value::Bool(items.iter().any(|item| values_equal(item, &needle))))
                    }
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
                if s.len().saturating_mul(n) > MAX_REPEAT_BYTES {
                    return Err(RillError::new(format!(
                        "repeat would produce more than {} bytes",
                        MAX_REPEAT_BYTES
                    ))
                    .into());
                }
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
                let Value::List(items) = val else {
                    return Err(RillError::new("sort needs a List").into());
                };
                let mut items = Arc::unwrap_or_clone(items);
                let all_int = items.iter().all(|v| matches!(v, Value::Int(_)));
                if all_int {
                    items.sort_by_key(|v| if let Value::Int(n) = v { *n } else { 0 });
                } else {
                    items.sort_by_cached_key(Value::as_string_lossy);
                }
                Ok(Value::list(items))
            }
            "flat_map" => self.eval_flat_map(args),
            "find" | "any" => self.eval_search(name, args),
            "first" => {
                let val = self.eval_expr(&args[0])?;
                let Value::List(items) = val else {
                    return Err(RillError::new("first needs a List").into());
                };
                Ok(Value::Option(Arc::unwrap_or_clone(items).into_iter().next().map(Box::new)))
            }
            "last" => {
                let val = self.eval_expr(&args[0])?;
                let Value::List(items) = val else {
                    return Err(RillError::new("last needs a List").into());
                };
                Ok(Value::Option(Arc::unwrap_or_clone(items).into_iter().next_back().map(Box::new)))
            }
            "slice" => {
                let list = self.eval_expr(&args[0])?;
                let Value::Int(start) = self.eval_expr(&args[1])? else {
                    return Err(RillError::new("slice start must be Int").into());
                };
                let Value::Int(end) = self.eval_expr(&args[2])? else {
                    return Err(RillError::new("slice end must be Int").into());
                };
                let Value::List(items) = list else {
                    return Err(RillError::new("slice needs a List").into());
                };
                let (from, to) = slice_bounds(items.len(), Some(start), Some(end), false);
                if from > to {
                    return Err(
                        RillError::new(format!("slice start {start} is after end {end}")).into()
                    );
                }
                Ok(Value::List(items[from..to].to_vec().into()))
            }
            "skip" => {
                let list = self.eval_expr(&args[0])?;
                let n = as_usize(&self.eval_expr(&args[1])?)?;
                let Value::List(items) = list else {
                    return Err(RillError::new("skip needs a List").into());
                };
                Ok(Value::List(Arc::unwrap_or_clone(items).into_iter().skip(n).collect()))
            }
            "step" => {
                let list = self.eval_expr(&args[0])?;
                let n = as_usize(&self.eval_expr(&args[1])?)?;
                if n == 0 {
                    return Err(RillError::new("step needs a positive step").into());
                }
                let Value::List(items) = list else {
                    return Err(RillError::new("step needs a List").into());
                };
                Ok(Value::List(Arc::unwrap_or_clone(items).into_iter().step_by(n).collect()))
            }
            "difference" => {
                let Value::List(a) = self.eval_expr(&args[0])? else {
                    return Err(RillError::new("difference needs a List").into());
                };
                let Value::List(b) = self.eval_expr(&args[1])? else {
                    return Err(RillError::new("difference needs a List").into());
                };
                Ok(Value::List(match crate::eval_collections::difference_membership(&a, &b) {
                    Some(membership) => Arc::unwrap_or_clone(a)
                        .into_iter()
                        .zip(membership)
                        .filter_map(|(value, keep)| keep.then_some(value))
                        .collect(),
                    None => {
                        Arc::unwrap_or_clone(a).into_iter().filter(|x| !b.contains(x)).collect()
                    }
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
                            Arc::unwrap_or_clone(a)
                                .into_iter()
                                .zip(a_membership)
                                .filter_map(|(value, keep)| keep.then_some(value))
                                .collect(),
                            Arc::unwrap_or_clone(b)
                                .into_iter()
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
                Ok(Value::List(Arc::unwrap_or_clone(map).into_keys().map(Value::Str).collect()))
            }
            "values" => {
                let val = self.eval_expr(&args[0])?;
                let Value::Map(map) = val else {
                    return Err(RillError::new("values needs a Map").into());
                };
                Ok(Value::List(Arc::unwrap_or_clone(map).into_values().collect()))
            }
            "entries" => {
                let val = self.eval_expr(&args[0])?;
                let Value::Map(map) = val else {
                    return Err(RillError::new("entries needs a Map").into());
                };
                Ok(Value::List(
                    Arc::unwrap_or_clone(map)
                        .into_iter()
                        .map(|(k, v)| Value::Tuple(vec![Value::Str(k), v].into()))
                        .collect(),
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
                Arc::make_mut(&mut base).extend(Arc::unwrap_or_clone(other));
                Ok(Value::Map(base))
            }
            _ => {
                if let Some(value) = self.env.lookup(name)? {
                    let (positional, named) = self.eval_call_args(args)?;
                    self.call_callable_named(value, positional, named)
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
                let text = if crate::term::color_enabled() {
                    format!("{}{}\n", pretty_value(&value, 0), ANSI_RESET)
                } else {
                    format!("{}\n", pretty_value(&value, 0))
                };
                self.emit(OutStream::Stdout, &text)?;
                Ok(Value::Null)
            }
            Builtin::Show => {
                let value = self.eval_expr(&args[0])?;
                let title = match args.get(1) {
                    Some(arg) => Some(self.eval_expr(arg)?.as_string_lossy()),
                    None => None,
                };
                if let Some(sink) = &self.show {
                    sink(title.as_deref(), &value);
                } else {
                    let mut text = show_text(&value, title.as_deref());
                    if crate::term::color_enabled() {
                        text.push_str(ANSI_RESET);
                        text.push('\n');
                    }
                    self.emit(OutStream::Stdout, &text)?;
                }
                Ok(Value::Null)
            }
            Builtin::Print | Builtin::Prin | Builtin::Warn => {
                for arg in args {
                    let value = self.eval_expr(arg)?;
                    self.check_cancelled()?;
                    match implementation {
                        Builtin::Print => self.emit(OutStream::Stdout, &format!("{value}\n"))?,
                        Builtin::Prin => self.emit(OutStream::Stdout, &value.to_string())?,
                        _ => self.emit(OutStream::Stderr, &format!("{value}\n"))?,
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
            Builtin::Path | Builtin::File => Ok(Value::Path(self.path_from_expr(&args[0])?)),
            Builtin::Shell => {
                let script = self.eval_expr(&args[0])?.as_string_lossy();
                let argv = self.wrap_remote(vec!["sh".into(), "-c".into(), script], false)?;
                Ok(self.execute_command(&argv, None, &CommandIo::default()).map(Value::process)?)
            }
            Builtin::Docker => {
                let container = self.eval_expr(&args[0])?.as_string_lossy();
                validate_remote_name("container name", &container)?;
                Ok(Value::List(
                    vec![
                        Value::Str("sudo".into()),
                        Value::Str("docker".into()),
                        Value::Str("exec".into()),
                        Value::Str(container),
                    ]
                    .into(),
                ))
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
                for item in Arc::unwrap_or_clone(items) {
                    out.push(self.apply_unary(mapper, item)?);
                }
                Ok(Value::List(out.into()))
            }
            Value::Map(entries) => {
                let mut out = Vec::new();
                for (key, value) in Arc::unwrap_or_clone(entries) {
                    out.push(self.apply_binary(mapper, Value::Str(key), value)?);
                }
                Ok(Value::List(out.into()))
            }
            Value::Option(Some(value)) => {
                Ok(Value::Option(Some(Box::new(self.apply_unary(mapper, *value)?))))
            }
            Value::Option(None) => Ok(Value::Option(None)),
            Value::Result(Ok(value)) => {
                Ok(Value::Result(Ok(Box::new(self.apply_unary(mapper, *value)?))))
            }
            Value::Result(Err(error)) => Ok(Value::Result(Err(error))),
            other => Err(RillError::new(format!(
                "map needs a List, Map, Option, or Result, got {}",
                other.type_name()
            ))
            .into()),
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
            Value::List(items) => {
                Arc::unwrap_or_clone(items).into_iter().map(|v| (vec![v], false)).collect()
            }
            Value::Map(entries) => Arc::unwrap_or_clone(entries)
                .into_iter()
                .map(|(k, v)| (vec![Value::Str(k), v], true))
                .collect(),
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
                                ParallelTerminal::Runtime(*error),
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
        for item in Arc::unwrap_or_clone(items) {
            if self.apply_unary(predicate, item.clone())?.truthy() {
                out.push(item);
            }
        }
        Ok(Value::List(out.into()))
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
            Arc::unwrap_or_clone(items)
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
                                ParallelTerminal::Runtime(*error),
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
        for item in Arc::unwrap_or_clone(items) {
            match item {
                Value::List(inner) => out.extend(Arc::unwrap_or_clone(inner)),
                other => out.push(other),
            }
        }
        Ok(Value::List(out.into()))
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
        let out = Arc::unwrap_or_clone(a)
            .into_iter()
            .zip(Arc::unwrap_or_clone(b))
            .map(|(x, y)| Value::Tuple(vec![x, y].into()))
            .collect();
        Ok(Value::List(out))
    }

    fn eval_enumerate(&mut self, args: &[Expr]) -> Result<Value, EvalError> {
        let list = self.eval_one_arg("enumerate", args)?;
        let Value::List(items) = list else {
            return Err(RillError::new("enumerate needs a List").into());
        };
        let out = Arc::unwrap_or_clone(items)
            .into_iter()
            .enumerate()
            .map(|(i, v)| Value::Tuple(vec![Value::Int(i as i64), v].into()))
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
        for item in Arc::unwrap_or_clone(items) {
            let key = self.apply_unary(keyer, item.clone())?.as_string_lossy();
            map.entry(key).or_default().push(item);
        }
        Ok(Value::map(map.into_iter().map(|(k, v)| (k, Value::List(v.into()))).collect()))
    }

    fn eval_index_by(&mut self, args: &[Expr]) -> Result<Value, EvalError> {
        if args.len() != 2 {
            return Err(RillError::new("index_by needs 1 arg").into());
        }
        let list = self.eval_expr(&args[0])?;
        let Value::List(items) = list else {
            return Err(RillError::new("index_by needs a List").into());
        };
        let keyer = &args[1];
        let mut map: indexmap::IndexMap<String, Value> = indexmap::IndexMap::new();
        for item in Arc::unwrap_or_clone(items) {
            self.poll_cancelled()?;
            let key = self.apply_unary(keyer, item.clone())?.as_string_lossy();
            map.insert(key, item);
        }
        Ok(Value::map(map))
    }

    fn eval_sort_by(&mut self, args: &[Expr]) -> Result<Value, EvalError> {
        if args.len() != 2 {
            return Err(RillError::new("sort_by needs 1 arg").into());
        }
        let list = self.eval_expr(&args[0])?;
        let Value::List(items) = list else {
            return Err(RillError::new("sort_by needs a List").into());
        };
        let keyer = &args[1];
        let mut keyed = Vec::with_capacity(items.len());
        for item in Arc::unwrap_or_clone(items) {
            self.poll_cancelled()?;
            let key = self.apply_unary(keyer, item.clone())?;
            keyed.push((key, item));
        }
        keyed.sort_by(|(a, _), (b, _)| order_values(a, b));
        Ok(Value::List(keyed.into_iter().map(|(_, item)| item).collect()))
    }

    fn eval_unique(&mut self, args: &[Expr]) -> Result<Value, EvalError> {
        let list = self.eval_one_arg("unique", args)?;
        let Value::List(items) = list else {
            return Err(RillError::new("unique needs a List").into());
        };
        let mut seen = std::collections::HashSet::new();
        let mut out = Vec::new();
        for item in Arc::unwrap_or_clone(items) {
            if seen.insert(crate::eval_data::value_to_json(&item).to_string()) {
                out.push(item);
            }
        }
        Ok(Value::List(out.into()))
    }

    /// `all(pred)` and `count(pred)`.
    fn eval_predicate_fold(&mut self, name: &str, args: &[Expr]) -> Result<Value, EvalError> {
        if args.len() != 2 {
            return Err(RillError::new(format!("{name} needs 1 arg")).into());
        }
        let list = self.eval_expr(&args[0])?;
        let Value::List(items) = list else {
            return Err(RillError::new(format!("{name} needs a List")).into());
        };
        let pred = &args[1];
        let mut count = 0i64;
        for item in Arc::unwrap_or_clone(items) {
            self.poll_cancelled()?;
            let result = self.apply_unary(pred, item)?;
            if is_truthy(&result)? {
                count += 1;
            } else if name == "all" {
                return Ok(Value::Bool(false));
            }
        }
        Ok(if name == "all" { Value::Bool(true) } else { Value::Int(count) })
    }

    fn eval_sum(&mut self, args: &[Expr]) -> Result<Value, EvalError> {
        let list = self.eval_one_arg("sum", args)?;
        let Value::List(items) = list else {
            return Err(RillError::new("sum needs a List").into());
        };
        if items.iter().all(|item| matches!(item, Value::Int(_))) {
            let mut total: i64 = 0;
            for item in items.iter() {
                if let Value::Int(n) = item {
                    total = total
                        .checked_add(*n)
                        .ok_or_else(|| EvalError::from(RillError::new("sum overflowed an Int")))?;
                }
            }
            return Ok(Value::Int(total));
        }
        let mut total = 0.0;
        for item in items.iter() {
            total += match item {
                Value::Int(n) => *n as f64,
                Value::Float(f) => *f,
                other => {
                    return Err(RillError::new(format!(
                        "sum needs numbers, got {}",
                        other.type_name()
                    ))
                    .into())
                }
            };
        }
        Ok(Value::Float(total))
    }

    /// `ostack.servers(all_projects=true)`: a zero-receiver namespace function.
    fn eval_namespace_function(
        &mut self,
        function: &'static crate::methods::NamespaceFunction,
        args: &[Expr],
    ) -> Result<Value, EvalError> {
        if function.module == "sql" && function.name == "statement" {
            // Before evaluation: the interpolations become parameters.
            return Ok(self.sql_statement("sql.statement", args)?.to_value());
        }
        let (positional, named) = self.eval_call_args(args)?;
        let kinds: Vec<crate::methods::Kind> = positional.iter().map(method_kind).collect();
        let named_kinds: Vec<(&str, crate::methods::Kind)> =
            named.iter().map(|(name, value)| (name.as_str(), method_kind(value))).collect();
        function.validate(&kinds, &named_kinds)?;
        self.check_cancelled()?;
        match function.module {
            "ostack" => self.eval_ostack(function.name, positional, named),
            "sql" => match function.name {
                "ident" => Ok(crate::sql::fragment(crate::sql::quote_ident(
                    &positional[0].as_string_lossy(),
                ))),
                "raw" => Ok(crate::sql::fragment(positional[0].as_string_lossy())),
                "connect" => self.eval_sql_connect(positional, named),
                other => Err(RillError::new(format!("sql has no function '{other}'")).into()),
            },
            "time" => match function.name {
                "now" => Ok(Value::Time(chrono::Utc::now())),
                "from_unix" => match positional.first() {
                    Some(Value::Int(seconds)) => chrono::DateTime::from_timestamp(*seconds, 0)
                        .map(Value::Time)
                        .ok_or_else(|| RillError::new("timestamp out of range").into()),
                    _ => Err(RillError::new("time.from_unix needs Int seconds").into()),
                },
                other => Err(RillError::new(format!("time has no function '{other}'")).into()),
            },
            other => {
                Err(RillError::new(format!("{other} has no function '{}'", function.name)).into())
            }
        }
    }

    /// `handle.servers()` where `handle` came from `ostack.project(..)` or
    /// `ostack.all_projects()`: the namespace function with the scope added
    /// as a named argument.
    /// `.servers()`, `.ports()`, `.project()` … on a map: a scoped listing on
    /// an `ostack.project(..)` / `ostack.all_projects()` handle or a project
    /// record, else the relation of the record's kind.
    fn eval_ostack_map_method(&mut self, name: &str, args: &[Expr]) -> Result<Value, EvalError> {
        let receiver = self.eval_expr(&args[0])?;
        let Value::Map(map) = &receiver else {
            return Err(RillError::new(format!(
                ".{name}() needs an OpenStack record or a handle from ostack.project(name), got {}",
                receiver.type_name()
            ))
            .into());
        };
        let marker = match map.get(crate::methods::OSTACK_MARKER) {
            Some(Value::Str(marker)) => Some(marker.clone()),
            _ => None,
        };
        let kind = crate::methods::ostack_record_kind(map);
        let scoped = crate::methods::OSTACK_SCOPED_METHODS.contains(&name);
        let scope = match (marker.as_deref(), kind) {
            (Some("all"), _) => Some(("all_projects".to_string(), Value::Bool(true))),
            (Some("project"), _) | (None, Some("project")) => match map.get("id") {
                Some(Value::Str(id)) => Some(("project".to_string(), Value::Str(id.clone()))),
                _ => return Err(RillError::new("ostack project handle has no id").into()),
            },
            _ => None,
        };
        if let (Some(scope), true) = (scope.clone(), scoped) {
            let Some(function) = crate::methods::namespace_function("ostack", name) else {
                return Err(RillError::new(format!("ostack has no function '{name}'")).into());
            };
            let (positional, extra) = self.eval_call_args(&args[1..])?;
            let mut named = vec![scope];
            named.extend(extra);
            let kinds: Vec<crate::methods::Kind> = positional.iter().map(method_kind).collect();
            let named_kinds: Vec<(&str, crate::methods::Kind)> =
                named.iter().map(|(name, value)| (name.as_str(), method_kind(value))).collect();
            function.validate(&kinds, &named_kinds)?;
            self.check_cancelled()?;
            return self.eval_ostack(function.name, positional, named);
        }
        let relation_kind = match (marker.as_deref(), kind) {
            (Some("project"), _) => Some("project"),
            (Some(_), _) => None,
            (None, kind) => kind,
        };
        match relation_kind {
            Some(kind) if crate::methods::ostack_relations(kind).contains(&name) => {
                if args.len() > 1 {
                    return Err(
                        RillError::new(format!("{kind}.{name}() takes no arguments")).into()
                    );
                }
                self.check_cancelled()?;
                self.eval_ostack_relation(kind, name, (**map).clone())
            }
            Some(kind) => Err(RillError::new(format!(
                "a {kind} has no .{name}(); it has {}",
                crate::methods::ostack_relations(kind)
                    .iter()
                    .map(|m| format!(".{m}()"))
                    .collect::<Vec<_>>()
                    .join(" ")
            ))
            .into()),
            None if marker.is_some() => Err(RillError::new(format!(
                ".{name}() is not a listing; an ostack handle has {}",
                crate::methods::OSTACK_SCOPED_METHODS
                    .iter()
                    .map(|m| format!(".{m}()"))
                    .collect::<Vec<_>>()
                    .join(" ")
            ))
            .into()),
            None => Err(RillError::new(format!(
                ".{name}() needs an OpenStack record or a handle from ostack.project(name)"
            ))
            .with_help(format!("or call ostack.{name}() for the current project"))
            .into()),
        }
    }

    #[cfg(feature = "openstack")]
    fn eval_ostack_relation(
        &mut self,
        kind: &str,
        method: &str,
        record: crate::types::Map,
    ) -> Result<Value, EvalError> {
        let cancellation = self.cancellation.clone();
        let lookup = |key: &str| self.lookup_environment_value(key).map(|v| v.as_string_lossy());
        match crate::openstack::relation_call(kind, method, record, &lookup, cancellation) {
            Ok(value) => Ok(value),
            Err(crate::openstack::Error::Cancelled) => Err(EvalError::Cancelled),
            Err(crate::openstack::Error::Failed(error)) => Err(error.into()),
        }
    }

    #[cfg(not(feature = "openstack"))]
    fn eval_ostack_relation(
        &mut self,
        kind: &str,
        method: &str,
        _record: crate::types::Map,
    ) -> Result<Value, EvalError> {
        Err(RillError::new(format!(
            "{kind}.{method}() needs OpenStack support, which this rill was built without"
        ))
        .with_help("rebuild with `cargo build --features openstack`")
        .into())
    }

    fn eval_time_method(&mut self, name: &str, args: &[Expr]) -> Result<Value, EvalError> {
        use chrono::{Datelike, Timelike};
        let Value::Time(time) = self.eval_expr(&args[0])? else {
            return Err(RillError::new(format!("{name} needs a Time")).into());
        };
        Ok(match name {
            "year" => Value::Int(time.year() as i64),
            "month" => Value::Int(time.month() as i64),
            "day" => Value::Int(time.day() as i64),
            "hour" => Value::Int(time.hour() as i64),
            "minute" => Value::Int(time.minute() as i64),
            "second" => Value::Int(time.second() as i64),
            "weekday" => Value::Str(time.format("%A").to_string()),
            "unix" => Value::Int(time.timestamp()),
            "date" => Value::Str(time.format("%Y-%m-%d").to_string()),
            "iso" => Value::Str(crate::types::format_time(&time)),
            "format" => {
                let format = self.eval_expr(&args[1])?.as_string_lossy();
                let mut out = String::new();
                use std::fmt::Write;
                write!(out, "{}", time.format(&format))
                    .map_err(|_| RillError::new(format!("invalid time format '{format}'")))?;
                Value::Str(out)
            }
            "age" => Value::Duration(
                chrono::Utc::now().signed_duration_since(time).to_std().unwrap_or_default(),
            ),
            other => return Err(RillError::new(format!("Time has no method '{other}'")).into()),
        })
    }

    #[cfg(feature = "openstack")]
    fn eval_ostack(
        &mut self,
        name: &'static str,
        positional: Vec<Value>,
        named: Vec<(String, Value)>,
    ) -> Result<Value, EvalError> {
        let cancellation = self.cancellation.clone();
        let lookup = |key: &str| self.lookup_environment_value(key).map(|v| v.as_string_lossy());
        let call =
            crate::openstack::Call { name, positional, named, lookup_env: &lookup, cancellation };
        match crate::openstack::call(call) {
            Ok(value) => Ok(value),
            Err(crate::openstack::Error::Cancelled) => Err(EvalError::Cancelled),
            Err(crate::openstack::Error::Failed(error)) => Err(error.into()),
        }
    }

    #[cfg(not(feature = "openstack"))]
    fn eval_ostack(
        &mut self,
        name: &'static str,
        _positional: Vec<Value>,
        _named: Vec<(String, Value)>,
    ) -> Result<Value, EvalError> {
        Err(RillError::new(format!(
            "ostack.{name} needs OpenStack support, which this rill was built without"
        ))
        .with_help("rebuild with `cargo build --features openstack`")
        .into())
    }

    /// The SQL text and parameters of `db.query(q, params?)`.  A literal
    /// `q` with interpolations becomes `$n` placeholders bound to the
    /// interpolated values (fragments from `sql.ident` / `sql.raw` are
    /// spliced); any other `q` is run as written with `params`.
    fn sql_statement(
        &mut self,
        what: &str,
        args: &[Expr],
    ) -> Result<crate::sql::Statement, EvalError> {
        let (query, params) = match args {
            [query] => (query, None),
            [query, params] => (query, Some(params)),
            _ => {
                return Err(RillError::new(format!(
                    "{what} needs a query and optional params, got {} args",
                    args.len()
                ))
                .into())
            }
        };
        let params = match params {
            None => None,
            Some(params) => match self.eval_expr(params)? {
                Value::List(items) => Some(items.to_vec()),
                other => {
                    return Err(RillError::new(format!(
                        "{what} params must be a List, got {}",
                        other.type_name()
                    ))
                    .into())
                }
            },
        };
        if let Expr::Interpolated(parts) = query.unspanned() {
            let mut builder = crate::sql::StatementBuilder::new();
            self.sql_parts(&mut builder, parts)?;
            if params.is_some() && builder.has_params() {
                return Err(RillError::new(format!(
                    "{what}: the query interpolates values and also has a params list"
                ))
                .with_help("use one or the other")
                .into());
            }
            let mut statement = builder.finish()?;
            if let Some(params) = params {
                statement.params = params;
            }
            return Ok(statement);
        }
        match self.eval_expr(query)? {
            Value::Str(text) => {
                Ok(crate::sql::Statement { text, params: params.unwrap_or_default() })
            }
            other => Err(RillError::new(format!(
                "{what} needs the query as a Str, got {}",
                other.type_name()
            ))
            .into()),
        }
    }

    fn sql_parts(
        &mut self,
        builder: &mut crate::sql::StatementBuilder,
        parts: &[StrPart],
    ) -> Result<(), EvalError> {
        for part in parts {
            match part {
                StrPart::Text(text) => builder.text(text),
                StrPart::Expr(expr) => {
                    let value = self.eval_expr(expr)?;
                    builder.value(value)?;
                }
                StrPart::Var(name) => {
                    let value = self.lookup_environment_value(name).ok_or_else(|| {
                        RillError::new(format!("unknown variable '${name}'"))
                            .with_help("`$name` looks up a Rill binding, then the environment")
                    })?;
                    builder.value(value)?;
                }
                StrPart::VarDefault(name, default) => {
                    let value = self
                        .lookup_environment_value(name)
                        .filter(|value| !interpolated_text(value).is_empty());
                    let value = match value {
                        Some(value) => value,
                        None => Value::Str(self.eval_parts(default)?),
                    };
                    builder.value(value)?;
                }
            }
        }
        Ok(())
    }

    #[cfg(feature = "postgres")]
    fn eval_sql_connect(
        &mut self,
        positional: Vec<Value>,
        named: Vec<(String, Value)>,
    ) -> Result<Value, EvalError> {
        let url = positional.first().map(Value::as_string_lossy);
        if let Some((scheme, _)) = url.as_deref().and_then(|url| url.split_once("://")) {
            if !matches!(scheme, "postgres" | "postgresql") {
                return Err(RillError::new(format!("sql: no driver for {scheme}:// URLs"))
                    .with_help("sql.connect supports postgres:// (and postgresql://)")
                    .into());
            }
        }
        let mut options = crate::sql::ConnectOptions::default();
        for (name, value) in named {
            match (name.as_str(), value) {
                ("password", value) => options.password = Some(value.as_string_lossy()),
                ("tls", value) => options.tls = Some(value.as_string_lossy().parse()?),
                ("app", value) => options.app = Some(value.as_string_lossy()),
                ("timeout", Value::Duration(duration)) => options.timeout = Some(duration),
                ("timeout", Value::Int(secs)) if secs > 0 => {
                    options.timeout = Some(std::time::Duration::from_secs(secs as u64))
                }
                (name, value) => {
                    return Err(RillError::new(format!(
                        "sql.connect: invalid {name}= {}",
                        value.type_name()
                    ))
                    .with_help("timeout= takes a Duration such as 30s")
                    .into())
                }
            }
        }
        let cancellation = self.cancellation.clone();
        let lookup = |key: &str| self.lookup_environment_value(key).map(|v| v.as_string_lossy());
        crate::sql::Db::connect_with_env(url.as_deref(), &options, &lookup, &cancellation)
            .map(Value::Db)
            .map_err(sql_failure)
    }

    #[cfg(not(feature = "postgres"))]
    fn eval_sql_connect(
        &mut self,
        _positional: Vec<Value>,
        _named: Vec<(String, Value)>,
    ) -> Result<Value, EvalError> {
        Err(RillError::new(
            "sql.connect needs a database driver, which this rill was built without",
        )
        .with_help("rebuild with `cargo build --features postgres`")
        .into())
    }

    /// `.query()`, `.one()`, `.scalar()`, `.exec()`, `.batch()`,
    /// `.transaction()` and `.close()` on a `Db`.
    #[cfg(feature = "postgres")]
    fn eval_sql_method(
        &mut self,
        db: crate::sql::Db,
        name: &str,
        args: &[Expr],
    ) -> Result<Value, EvalError> {
        let cancellation = self.cancellation.clone();
        match name {
            "query" | "one" | "scalar" | "exec" => {
                let statement = self.sql_statement(&format!(".{name}()"), args)?;
                self.check_cancelled()?;
                let (text, params) = (&statement.text, &statement.params);
                if name == "exec" {
                    let count = db.exec(text, params, &cancellation).map_err(sql_failure)?;
                    return Ok(Value::Int(i64::try_from(count).unwrap_or(i64::MAX)));
                }
                let rows = db.query(text, params, &cancellation).map_err(sql_failure)?;
                let mut rows = rows.into_iter();
                Ok(match name {
                    "query" => Value::list(rows.collect()),
                    "one" => Value::Option(rows.next().map(Box::new)),
                    _ => match rows.next() {
                        Some(Value::Map(row)) => {
                            row.values().next().cloned().unwrap_or(Value::Null)
                        }
                        _ => Value::Null,
                    },
                })
            }
            "batch" => match self.eval_expr(&args[0])? {
                Value::Str(sql) => {
                    db.batch(&sql, &cancellation).map_err(sql_failure)?;
                    Ok(Value::Null)
                }
                other => Err(RillError::new(format!(
                    ".batch() needs the SQL as a Str, got {}",
                    other.type_name()
                ))
                .into()),
            },
            "transaction" => {
                let callback = self.eval_expr(&args[0])?;
                if !matches!(callback, Value::Function(_)) {
                    return Err(RillError::new(format!(
                        ".transaction() needs a function |tx| ..., got {}",
                        callback.type_name()
                    ))
                    .into());
                }
                db.begin(&cancellation).map_err(sql_failure)?;
                let result = self.call_callable(callback, vec![Value::Db(db.clone())]);
                let commit =
                    matches!(&result, Ok(value) if !matches!(value, Value::Result(Err(_))));
                let finished = db.finish(commit);
                let value = result?;
                finished?;
                Ok(value)
            }
            "close" => {
                db.close();
                Ok(Value::Null)
            }
            other => Err(RillError::new(format!("Db has no method '{other}'")).into()),
        }
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
        for item in Arc::unwrap_or_clone(items) {
            let mapped = self.apply_unary(mapper, item)?;
            match mapped {
                Value::List(inner) => out.extend(Arc::unwrap_or_clone(inner)),
                other => out.push(other),
            }
        }
        Ok(Value::List(out.into()))
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
        for item in Arc::unwrap_or_clone(items) {
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

    /// Bind positional and named arguments to parameters, filling in defaults.
    fn call_function_named(
        &mut self,
        function: &Function,
        positional: Vec<Value>,
        named: Vec<(String, Value)>,
    ) -> Result<Value, EvalError> {
        let params = &function.params;
        let required = params.iter().filter(|param| param.default.is_none()).count();
        if positional.len() > params.len() {
            let expected = if required == params.len() {
                params.len().to_string()
            } else {
                format!("{required} to {}", params.len())
            };
            return Err(RillError::new(format!(
                "function expected {expected} args, got {}",
                positional.len()
            ))
            .into());
        }
        let mut bound: Vec<Option<Value>> = positional.into_iter().map(Some).collect();
        bound.resize(params.len(), None);
        for (name, value) in named {
            let Some(index) = params.iter().position(|param| param.name == name) else {
                return Err(RillError::new(format!("function has no parameter '{name}'")).into());
            };
            if bound[index].is_some() {
                return Err(RillError::new(format!("parameter '{name}' given twice")).into());
            }
            bound[index] = Some(value);
        }
        for (param, value) in params.iter().zip(&bound) {
            if let (Some(annotation), Some(value)) = (&param.type_name, value) {
                crate::typeck::check_value_annotation(value, annotation, &self.records)?;
            }
        }
        self.call_function_in_env(function, bound)
    }

    fn call_function_in_env(
        &mut self,
        function: &Function,
        args: Vec<Option<Value>>,
    ) -> Result<Value, EvalError> {
        if self.function_depth >= max_call_depth() {
            return Err(RillError::new(format!(
                "call depth limit of {} exceeded; set RILL_MAX_CALL_DEPTH to raise it",
                max_call_depth()
            ))
            .into());
        }
        self.function_depth += 1;
        self.env.push();
        let mut bind_result = Ok(());
        for (param, value) in function.params.iter().zip(args) {
            // Defaults are evaluated in the callee's scope, so they can refer
            // to earlier parameters.
            let value = match (value, &param.default) {
                (Some(value), _) => value,
                (None, Some(default)) => match self.eval_expr(default) {
                    Ok(value) => value,
                    Err(error) => {
                        bind_result = Err(error);
                        break;
                    }
                },
                (None, None) => {
                    bind_result = Err(RillError::new(format!(
                        "missing argument for parameter '{}'",
                        param.name
                    ))
                    .into());
                    break;
                }
            };
            // Parameters are immutable, like `let`: no shared cell is needed.
            self.env.bind(param.name.clone(), value, false);
        }
        if let Err(error) = bind_result {
            self.env.pop();
            self.function_depth -= 1;
            return Err(error);
        }
        let flow_result = self.eval_stmts(&function.body);
        self.env.pop();
        self.function_depth -= 1;
        let flow = flow_result?;
        match flow {
            Flow::Value(value) | Flow::Return(value) => Ok(wrap_declared_result(function, value)),
            Flow::Propagate(value) => Ok(propagated_value(value)),
            Flow::Break(_) => Err(RillError::new("break outside loop").into()),
            Flow::Continue => Err(RillError::new("continue outside loop").into()),
            Flow::Emit(_) => Err(RillError::new("emit outside for expression").into()),
        }
    }

    fn call_callable(&mut self, value: Value, args: Vec<Value>) -> Result<Value, EvalError> {
        self.call_callable_named(value, args, Vec::new())
    }

    /// Evaluate call arguments, separating `name = value` arguments out.
    fn eval_call_args(&mut self, args: &[Expr]) -> Result<CallArgs, EvalError> {
        let mut positional = Vec::new();
        let mut named = Vec::new();
        for arg in args {
            match arg.unspanned() {
                Expr::NamedArg { name, value } => {
                    named.push((name.clone(), self.eval_expr(value)?));
                }
                _ => {
                    if !named.is_empty() {
                        return Err(RillError::new(
                            "positional arguments must come before named arguments",
                        )
                        .into());
                    }
                    positional.push(self.eval_expr(arg)?);
                }
            }
        }
        Ok((positional, named))
    }

    fn call_callable_named(
        &mut self,
        value: Value,
        positional: Vec<Value>,
        named: Vec<(String, Value)>,
    ) -> Result<Value, EvalError> {
        match value {
            Value::Function(function) => {
                // Run the body in the scope chain the function closed over; a
                // frame for its parameters is pushed on top.  The parallel
                // write floor carries over so a worker cannot smuggle a write
                // through a function defined outside the parallel body.
                let frozen = self.env.frozen.min(function.captured.len());
                let callee_env = Env { scopes: function.captured.clone(), frozen };
                let current = std::mem::replace(&mut self.env, callee_env);
                let result = self.call_function_named(&function.function, positional, named);
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
                if crate::methods::namespace_function(module, name).is_some() {
                    return Err(RillError::new(format!(
                        "{module}.{name} must be called: {module}.{name}()"
                    ))
                    .into());
                }
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
                            params: vec![Param {
                                name: "_arg".to_string(),
                                type_name: None,
                                default: None,
                            }],
                            return_type: None,
                            body: vec![Stmt::Return(Some(call))],
                            propagation: PropagationCell::new(Propagation::None),
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
            BinaryOp::Eq => Value::Bool(values_equal(&left, &right)),
            BinaryOp::Ne => Value::Bool(!values_equal(&left, &right)),
            BinaryOp::Gt | BinaryOp::Ge | BinaryOp::Lt | BinaryOp::Le => {
                use std::cmp::Ordering::{Equal, Greater, Less};
                let order = compare_values(&left, &right, op)?;
                Value::Bool(match op {
                    BinaryOp::Gt => order == Greater,
                    BinaryOp::Ge => matches!(order, Greater | Equal),
                    BinaryOp::Lt => order == Less,
                    _ => matches!(order, Less | Equal),
                })
            }
            BinaryOp::In => Value::Bool(contains_value(&right, &left)?),
            BinaryOp::NotIn => Value::Bool(!contains_value(&right, &left)?),
            BinaryOp::Range | BinaryOp::RangeInclusive => match (&left, &right) {
                (Value::Int(a), Value::Int(b)) => {
                    let end = if op == BinaryOp::RangeInclusive { b.saturating_add(1) } else { *b };
                    if end.saturating_sub(*a) > MAX_RANGE_LEN {
                        return Err(RillError::new(format!(
                            "range {a}..{b} would produce more than {MAX_RANGE_LEN} elements"
                        ))
                        .into());
                    }
                    Value::List((*a..end).map(Value::Int).collect())
                }
                _ => {
                    return Err(RillError::new(format!(
                        "range requires Int..Int, got {}..{}",
                        left.type_name(),
                        right.type_name()
                    ))
                    .into())
                }
            },
            BinaryOp::And | BinaryOp::Or | BinaryOp::Coalesce => {
                unreachable!("short-circuit operators are handled before eval_binary")
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
        for (index, command) in commands.iter().enumerate() {
            let local = self.command_argv(command)?;
            argv.push(self.wrap_remote(local, index > 0)?);
            io_configs.push(self.command_io(command)?);
        }
        let result = self.execute_pipeline(&argv, &io_configs)?;
        Ok(Value::ProcessResult(result.into()))
    }

    fn eval_command(
        &mut self,
        command: &CommandExpr,
        input: Option<Vec<u8>>,
    ) -> Result<Value, EvalError> {
        let argv = self.command_argv(command)?;
        let argv = self.wrap_remote(argv, input.is_some())?;
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
                let msg = if !result.err.is_empty() {
                    result.stderr_text_lossy().trim().to_string()
                } else if let Some(signal) = result.signal {
                    format!("command killed by signal {signal}")
                } else {
                    format!("command exited with status {}", result.status)
                };
                return self.check_exit_error(msg, Some(result.status), command.span);
            }
            Ok(Value::Str(result.stdout_text_lossy().into_owned()))
        } else {
            Ok(Value::ProcessResult((self.execute_command(&argv, input, &io)?).into()))
        }
    }

    fn check_exit_error(
        &self,
        msg: String,
        status: Option<i32>,
        span: crate::diagnostic::Span,
    ) -> Result<Value, EvalError> {
        let err_val =
            Value::Result(Err(Box::new(Value::Error(Box::new(crate::types::ErrorValue {
                kind: crate::types::ErrorKind::Process,
                message: msg.clone(),
                operation: None,
                status,
                span: None,
            })))));
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
                CommandArg::Word(value) => out.push(self.expand_word_tilde(value)),
                CommandArg::String(value) => out.push(value.clone()),
                CommandArg::Var(name) => match self.command_var(name)? {
                    Value::List(items) => {
                        out.extend(
                            Arc::unwrap_or_clone(items)
                                .into_iter()
                                .map(|item| item.as_string_lossy()),
                        );
                    }
                    other => out.push(other.as_string_lossy()),
                },
                CommandArg::Splat(name) => match self.command_var(name)? {
                    Value::List(items) => {
                        out.extend(
                            Arc::unwrap_or_clone(items)
                                .into_iter()
                                .map(|item| item.as_string_lossy()),
                        );
                    }
                    other => out
                        .extend(other.as_string_lossy().split_whitespace().map(|w| w.to_string())),
                },
                CommandArg::Parts(parts) => out.push(self.eval_command_parts(parts, true)?),
                CommandArg::Expr(expr) => out.push(self.eval_expr(expr)?.as_string_lossy()),
            }
        }
        Ok(out)
    }

    fn command_var(&self, name: &str) -> Result<Value, EvalError> {
        self.env
            .get(name)
            .ok_or_else(|| RillError::new(format!("unknown command variable '${name}'")).into())
    }

    /// Join the fragments of one command argument.  With `tilde`, a leading
    /// `~` in source text at the start of the argument is expanded.
    fn eval_command_parts(
        &mut self,
        parts: &[CommandPart],
        tilde: bool,
    ) -> Result<String, EvalError> {
        let mut arg = String::new();
        for (index, part) in parts.iter().enumerate() {
            let fragment = match part {
                CommandPart::Text(text) => text.clone(),
                CommandPart::Var(name) => self.command_var(name)?.as_string_lossy(),
                CommandPart::VarDefault(name, default) => match self.env.get(name) {
                    Some(value) if !value.as_string_lossy().is_empty() => value.as_string_lossy(),
                    _ => self.eval_parts(default)?,
                },
                CommandPart::Expr(expr) => self.eval_expr(expr)?.as_string_lossy(),
            };
            let source_text = matches!(part, CommandPart::Text(_) | CommandPart::VarDefault(..));
            if index == 0 && tilde && source_text {
                arg.push_str(&self.expand_word_tilde(&fragment));
            } else {
                arg.push_str(&fragment);
            }
        }
        Ok(arg)
    }

    fn expand_word_tilde(&self, text: &str) -> String {
        expand_tilde_prefix(text, self.environment.get("HOME").map(String::as_str))
    }

    fn command_io(&mut self, command: &CommandExpr) -> Result<CommandIo, EvalError> {
        command.validate_config()?;
        let mut io = CommandIo::default();
        for redirect in &command.redirects {
            let target = self.command_arg_to_string(&redirect.target)?;
            let source_text = match &redirect.target {
                CommandArg::Word(_) | CommandArg::String(_) => true,
                CommandArg::Parts(parts) => {
                    matches!(
                        parts.first(),
                        Some(CommandPart::Text(_) | CommandPart::VarDefault(..))
                    )
                }
                _ => false,
            };
            let path = self.path_from_text(&target, source_text)?;
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
                    for (key, value) in Arc::unwrap_or_clone(values) {
                        io.env.insert(key, value.as_string_lossy());
                    }
                }
                "stdin" => match self.config_stream(expr, "stdin")? {
                    ConfigStream::File(path) => io.stdin_file = Some(path),
                    ConfigStream::Mode(StreamMode::Null) => io.stdin_null = true,
                    ConfigStream::Mode(StreamMode::Inherit) => io.stdin_inherit = true,
                    ConfigStream::Mode(StreamMode::Capture) => {
                        return Err(RillError::new(
                            "stdin: capture is not supported; a command reads stdin from a pipe, a file, or the terminal",
                        )
                        .into())
                    }
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
        self.path_from_expr(expr).map_err(|error| match error {
            EvalError::Runtime(error) => EvalError::Runtime(Box::new(RillError::new(format!(
                "command {name} config: {}",
                error.message
            )))),
            other => other,
        })
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
            CommandArg::Word(value) | CommandArg::String(value) => Ok(value.clone()),
            CommandArg::Var(name) | CommandArg::Splat(name) => {
                Ok(self.command_var(name)?.as_string_lossy())
            }
            CommandArg::Parts(parts) => self.eval_command_parts(parts, false),
            CommandArg::Expr(expr) => Ok(self.eval_expr(expr)?.as_string_lossy()),
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
        Err(EvalError::Runtime(Box::new(
            RillError::new("parallel iteration panicked").with_kind(ErrorKind::Runtime),
        )))
    })
}

fn parallel_terminal_error(outcome: ParallelTerminal) -> EvalError {
    match outcome {
        ParallelTerminal::Runtime(error) => EvalError::Runtime(Box::new(error)),
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
    if !crate::term::color_enabled() {
        return value.as_ref().to_string();
    }
    format!("{color}{}{ANSI_RESET}", value.as_ref())
}

fn pretty_indent(level: usize) -> String {
    "  ".repeat(level)
}

fn pretty_string(value: &str) -> String {
    pretty_color(ANSI_GREEN, serde_json::to_string(value).unwrap_or_else(|_| format!("{value:?}")))
}

/// What `show` writes without a host: a text table for a list of records
/// (one column per key, nested values summarised), the pretty value
/// otherwise, under `title` when given.
pub fn show_text(value: &Value, title: Option<&str>) -> String {
    let mut out = String::new();
    if let Some(title) = title {
        out.push_str(title);
        out.push('\n');
    }
    if let Some(groups) = grouped_tables(value) {
        for (index, (key, table)) in groups.into_iter().enumerate() {
            if index > 0 {
                out.push('\n');
            }
            out.push_str(&key);
            out.push('\n');
            out.push_str(&table);
        }
        return out;
    }
    match text_table(value) {
        Some(table) => out.push_str(&table),
        None => {
            out.push_str(&pretty_value(value, 0));
            out.push('\n');
        }
    }
    out
}

/// `group_by` output: a map whose values are all lists of records, as one
/// table per key.
fn grouped_tables(value: &Value) -> Option<Vec<(String, String)>> {
    let Value::Map(map) = value else { return None };
    if map.is_empty() {
        return None;
    }
    map.iter().map(|(key, list)| text_table(list).map(|table| (key.clone(), table))).collect()
}

fn text_table(value: &Value) -> Option<String> {
    const MAX_WIDTH: usize = 40;
    let Value::List(items) = value else { return None };
    if items.is_empty() || !items.iter().all(|item| matches!(item, Value::Map(_))) {
        return None;
    }
    let mut columns: Vec<&str> = Vec::new();
    for item in items.iter() {
        if let Value::Map(map) = item {
            for key in map.keys() {
                if !columns.contains(&key.as_str()) {
                    columns.push(key);
                }
            }
        }
    }
    let cell = |value: Option<&Value>| -> String {
        let text = match value {
            None | Some(Value::Null) => String::new(),
            Some(Value::Str(text)) => text.lines().next().unwrap_or("").to_string(),
            Some(Value::Map(map)) => format!("{{{} keys}}", map.len()),
            Some(Value::List(items)) => format!("[{} items]", items.len()),
            Some(other) => other.to_string(),
        };
        if text.chars().count() > MAX_WIDTH {
            format!("{}…", text.chars().take(MAX_WIDTH - 1).collect::<String>())
        } else {
            text
        }
    };
    let rows: Vec<Vec<String>> = items
        .iter()
        .map(|item| match item {
            Value::Map(map) => columns.iter().map(|column| cell(map.get(*column))).collect(),
            _ => Vec::new(),
        })
        .collect();
    let widths: Vec<usize> = columns
        .iter()
        .enumerate()
        .map(|(index, column)| {
            rows.iter()
                .map(|row| row[index].chars().count())
                .max()
                .unwrap_or(0)
                .max(column.chars().count())
        })
        .collect();
    let line = |cells: Vec<String>| -> String {
        let padded: Vec<String> = cells
            .iter()
            .enumerate()
            .map(|(index, text)| format!("{text:<width$}", width = widths[index]))
            .collect();
        format!("{}\n", padded.join("  ").trim_end())
    };
    let mut out = line(columns.iter().map(|c| c.to_string()).collect());
    for row in rows {
        out.push_str(&line(row));
    }
    Some(out)
}

/// Indented, coloured (when enabled) rendering used by `pretty_print` and the REPL.
pub fn pretty_value(value: &Value, level: usize) -> String {
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

/// Bounded, uncoloured rendering of a value for diagnostics: one level of
/// entries, nested collections summarised, long strings and long collections
/// cut short.
pub fn preview_value(value: &Value) -> String {
    const MAX_ENTRIES: usize = 12;
    let entries: Vec<String> = match value {
        Value::Map(map) => map
            .iter()
            .map(|(key, value)| format!("{}: {}", preview_string(key), preview_scalar(value)))
            .collect(),
        Value::List(values) | Value::Tuple(values) => values.iter().map(preview_scalar).collect(),
        other => return preview_scalar(other),
    };
    let (open, close, unit) = match value {
        Value::Map(_) => ("{", "}", "fields"),
        Value::Tuple(_) => ("(", ")", "items"),
        _ => ("[", "]", "items"),
    };
    if entries.is_empty() {
        return format!("{open}{close}");
    }
    let total = entries.len();
    let mut lines: Vec<String> =
        entries.into_iter().take(MAX_ENTRIES).map(|entry| format!("  {entry}")).collect();
    if total > MAX_ENTRIES {
        lines.push(format!("  … {} more {unit}", total - MAX_ENTRIES));
    }
    format!("{open}\n{}\n{close}", lines.join(",\n"))
}

fn preview_string(text: &str) -> String {
    const MAX_CHARS: usize = 60;
    if text.chars().count() > MAX_CHARS {
        let cut: String = text.chars().take(MAX_CHARS).collect();
        let quoted = serde_json::to_string(&cut).unwrap_or_else(|_| format!("{cut:?}"));
        // Re-open the closing quote so the ellipsis sits inside the string.
        format!("{}…\"", &quoted[..quoted.len() - 1])
    } else {
        serde_json::to_string(text).unwrap_or_else(|_| format!("{text:?}"))
    }
}

fn preview_scalar(value: &Value) -> String {
    let count =
        |n: usize, one: &str, many: &str| format!("{n} {}", if n == 1 { one } else { many });
    match value {
        Value::Str(text) => preview_string(text),
        Value::Map(map) if map.is_empty() => "{}".to_string(),
        Value::Map(map) => format!("{{{}}}", count(map.len(), "field", "fields")),
        Value::List(values) if values.is_empty() => "[]".to_string(),
        Value::List(values) => format!("[{}]", count(values.len(), "item", "items")),
        Value::Tuple(values) => format!("({})", count(values.len(), "item", "items")),
        Value::Option(Some(inner)) => format!("some({})", preview_scalar(inner)),
        Value::Option(None) => "none".to_string(),
        Value::Result(Ok(inner)) => format!("ok({})", preview_scalar(inner)),
        Value::Result(Err(inner)) => format!("err({})", preview_scalar(inner)),
        other => preview_other(other),
    }
}

fn preview_other(value: &Value) -> String {
    let text = value.to_string();
    if text.chars().count() > 60 {
        format!("{}…", text.chars().take(60).collect::<String>())
    } else {
        text
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

/// The method-dispatch kind of a runtime value (what `value.method()` looks
/// up), for tools that complete against live values.
pub fn value_kind(value: &Value) -> crate::methods::Kind {
    method_kind(value)
}

#[cfg(feature = "postgres")]
fn sql_failure(error: crate::blocking::Error) -> EvalError {
    match error {
        crate::blocking::Error::Cancelled => EvalError::Cancelled,
        crate::blocking::Error::Failed(error) => error.into(),
    }
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
        Value::Time(_) => Kind::Time,
        Value::ProcessHandle(_) => Kind::ProcessHandle,
        Value::Db(_) => Kind::Db,
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
        Value::Map(ref map) => match map.get(name) {
            Some(value) => Ok(value.clone()),
            None => {
                let mut help = String::new();
                if let Some(close) = crate::suggest::closest(name, map.keys().map(String::as_str)) {
                    help.push_str(&format!("did you mean '{close}'?\n"));
                }
                help.push_str(&format!(
                    "this record has no '{name}'; it is:\n{}\nuse .get(\"{name}\") when the field is optional",
                    preview_value(&base)
                ));
                Err(RillError::new(format!("unknown field '{name}'")).with_help(help))
            }
        },
        Value::ProcessResult(result) => match name {
            "status" => Ok(Value::Int(result.status as i64)),
            "signal" => {
                Ok(Value::Option(result.signal.map(|signal| Box::new(Value::Int(signal as i64)))))
            }
            "out" => Ok(Value::Str(result.stdout_text_lossy().into_owned())),
            "err" => Ok(Value::Str(result.stderr_text_lossy().into_owned())),
            "ok" => Ok(Value::Bool(result.ok())),
            other => Err(RillError::new(format!("unknown ProcessResult field '{other}'"))
                .with_help("a ProcessResult has the fields status, signal, out, err and ok")),
        },
        ref value
            if crate::methods::lookup(name)
                .is_some_and(|method| method.accepts(method_kind(value))) =>
        {
            Err(RillError::new(format!(".{name} is a method; call .{name}(...)")))
        }
        other => Err(RillError::new(format!("{} has no field '{name}'", other.type_name()))
            .with_help(format!("the value is {}", preview_value(&other)))),
    }
}

fn numeric_binary(left: Value, right: Value, op: BinaryOp) -> Result<Value, RillError> {
    if op == BinaryOp::Add && matches!((&left, &right), (Value::List(_), Value::List(_))) {
        let (Value::List(mut left), Value::List(right)) = (left, right) else { unreachable!() };
        Arc::make_mut(&mut left).extend(Arc::unwrap_or_clone(right));
        return Ok(Value::List(left));
    }
    if let (Value::Str(a), Value::Str(b), BinaryOp::Add) = (&left, &right, op) {
        return Ok(Value::Str(format!("{a}{b}")));
    }
    if matches!(left, Value::Time(_)) || matches!(right, Value::Time(_)) {
        return time_binary(left, right, op);
    }
    if matches!(left, Value::Duration(_)) || matches!(right, Value::Duration(_)) {
        return duration_binary(left, right, op);
    }
    check_numeric_operands(&left, &right, op)?;
    if let (Value::Int(a), Value::Int(b)) = (&left, &right) {
        let value = match op {
            BinaryOp::Add => a.checked_add(*b),
            BinaryOp::Sub => a.checked_sub(*b),
            BinaryOp::Mul => a.checked_mul(*b),
            BinaryOp::Div => a.checked_div(*b),
            BinaryOp::Rem => {
                if *b == 0 {
                    return Err(RillError::new("remainder by zero"));
                }
                a.checked_rem(*b)
            }
            BinaryOp::Pow => {
                let exponent = u32::try_from(*b).map_err(|_| {
                    RillError::new(if *b < 0 {
                        "negative exponent on an Int; use a Float base such as 2.0 ** -1"
                    } else {
                        "exponent too large"
                    })
                })?;
                a.checked_pow(exponent)
            }
            _ => unreachable!(),
        };
        return value
            .map(Value::Int)
            .ok_or_else(|| RillError::new("integer overflow or division by zero"));
    }
    let a = as_f64(&left)?;
    let b = as_f64(&right)?;
    if matches!(op, BinaryOp::Div | BinaryOp::Rem) && b == 0.0 {
        return Err(RillError::new("division by zero"));
    }
    let value = match op {
        BinaryOp::Add => a + b,
        BinaryOp::Sub => a - b,
        BinaryOp::Mul => a * b,
        BinaryOp::Div => a / b,
        BinaryOp::Rem => a % b,
        BinaryOp::Pow => a.powf(b),
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
        AssignOp::Rem => BinaryOp::Rem,
    };
    if bin_op == BinaryOp::Add {
        match current {
            Value::List(items) if matches!(rhs, Value::List(_)) => {
                let Value::List(more) = rhs else { unreachable!() };
                Arc::make_mut(items).extend(Arc::unwrap_or_clone(more));
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

/// One step from a variable to the slot being assigned.
enum PlaceStep {
    Field(String),
    Index { key: Value, literal: Option<i64> },
}

/// Walk `steps` into `value` and apply `op`/`rhs` at the end, in place.
fn assign_place(
    value: &mut Value,
    steps: &[PlaceStep],
    op: AssignOp,
    rhs: Value,
) -> Result<(), RillError> {
    let Some((step, rest)) = steps.split_first() else {
        return apply_compound(value, rhs, op);
    };
    let last = rest.is_empty();
    match (value, step) {
        (Value::Map(map), step) => {
            let map = Arc::make_mut(map);
            let key = match step {
                PlaceStep::Field(name) => name.clone(),
                PlaceStep::Index { key: Value::Str(key), .. } => key.clone(),
                PlaceStep::Index { key: Value::Int(key), .. } => key.to_string(),
                PlaceStep::Index { key, .. } => {
                    return Err(RillError::new(format!(
                        "map key must be Str or Int, got {}",
                        key.type_name()
                    )))
                }
            };
            if last && op == AssignOp::Assign {
                map.insert(key, rhs);
                return Ok(());
            }
            if last {
                let slot = map.entry(key).or_insert(Value::Int(0));
                return apply_compound(slot, rhs, op);
            }
            let Some(inner) = map.get_mut(&key) else {
                return Err(RillError::new(format!("key '{key}' not in map")));
            };
            assign_place(inner, rest, op, rhs)
        }
        (Value::List(items), PlaceStep::Index { key: Value::Int(idx), .. }) => {
            let items = Arc::make_mut(items);
            let len = items.len() as i64;
            let position = if *idx < 0 { len + idx } else { *idx };
            if position < 0 || position >= len {
                return Err(RillError::new(format!("index {idx} out of bounds (len {len})")));
            }
            assign_place(&mut items[position as usize], rest, op, rhs)
        }
        (Value::List(_), PlaceStep::Index { key, .. }) => {
            Err(RillError::new(format!("list index must be Int, got {}", key.type_name())))
        }
        (Value::Tuple(items), PlaceStep::Index { key: Value::Int(idx), literal }) => {
            if literal.is_none() {
                return Err(RillError::new("tuple assignment index must be an integer literal"));
            }
            let position = normalize_runtime_index(*idx, items.len(), "tuple")?;
            assign_place(&mut Arc::make_mut(items)[position], rest, op, rhs)
        }
        (Value::Tuple(_), PlaceStep::Index { .. }) => {
            Err(RillError::new("tuple index must be Int"))
        }
        (other, PlaceStep::Field(name)) => {
            Err(RillError::new(format!("cannot assign to field '{name}' of {}", other.type_name())))
        }
        (other, PlaceStep::Index { .. }) => {
            Err(RillError::new(format!("cannot index-assign into {}", other.type_name())))
        }
    }
}

fn check_numeric_operands(left: &Value, right: &Value, op: BinaryOp) -> Result<(), RillError> {
    if matches!(left, Value::Int(_) | Value::Float(_))
        && matches!(right, Value::Int(_) | Value::Float(_))
    {
        return Ok(());
    }
    Err(RillError::new(format!(
        "operator '{}' needs numeric operands, got {} and {}",
        operator_symbol(op),
        left.type_name(),
        right.type_name()
    )))
}

/// `Time ± Duration` and `Time - Time`.
fn time_binary(left: Value, right: Value, op: BinaryOp) -> Result<Value, RillError> {
    let span = |d: &std::time::Duration| {
        chrono::Duration::from_std(*d).map_err(|_| RillError::new("duration too large for a time"))
    };
    let out_of_range = || RillError::new("time out of range");
    match (&left, &right, op) {
        (Value::Time(t), Value::Duration(d), BinaryOp::Add)
        | (Value::Duration(d), Value::Time(t), BinaryOp::Add) => {
            t.checked_add_signed(span(d)?).map(Value::Time).ok_or_else(out_of_range)
        }
        (Value::Time(t), Value::Duration(d), BinaryOp::Sub) => {
            t.checked_sub_signed(span(d)?).map(Value::Time).ok_or_else(out_of_range)
        }
        (Value::Time(a), Value::Time(b), BinaryOp::Sub) => {
            let difference = a.signed_duration_since(*b);
            difference.to_std().map(Value::Duration).map_err(|_| {
                RillError::new(
                    "the second time is later than the first; a Duration cannot be negative",
                )
                .with_help("swap the operands, or compare the times with < and >")
            })
        }
        _ => Err(RillError::new(format!(
            "operator '{}' is not defined for {} and {}",
            operator_symbol(op),
            left.type_name(),
            right.type_name()
        ))),
    }
}

/// Parse a time: RFC 3339 / ISO 8601 (with or without a zone, which then
/// means UTC), `YYYY-MM-DD HH:MM:SS`, a bare date, or a strftime `format`.
pub fn parse_time(
    text: &str,
    format: Option<&str>,
) -> Result<chrono::DateTime<chrono::Utc>, RillError> {
    use chrono::{DateTime, NaiveDate, NaiveDateTime, Utc};
    let text = text.trim();
    let fail = || {
        RillError::new(format!("cannot parse '{text}' as a time")).with_help(match format {
            Some(_) => "check the format against the text (strftime syntax, e.g. %Y-%m-%d %H:%M)",
            None => "expected RFC 3339 (2026-10-15T14:29:21Z), YYYY-MM-DD HH:MM:SS or YYYY-MM-DD; pass a strftime format otherwise",
        })
    };
    if let Some(format) = format {
        if let Ok(time) = DateTime::parse_from_str(text, format) {
            return Ok(time.with_timezone(&Utc));
        }
        if let Ok(naive) = NaiveDateTime::parse_from_str(text, format) {
            return Ok(naive.and_utc());
        }
        if let Ok(date) = NaiveDate::parse_from_str(text, format) {
            return Ok(date.and_hms_opt(0, 0, 0).ok_or_else(fail)?.and_utc());
        }
        return Err(fail());
    }
    if let Ok(time) = DateTime::parse_from_rfc3339(text) {
        return Ok(time.with_timezone(&Utc));
    }
    for pattern in
        ["%Y-%m-%dT%H:%M:%S%.f", "%Y-%m-%d %H:%M:%S%.f", "%Y-%m-%dT%H:%M", "%Y-%m-%d %H:%M"]
    {
        if let Ok(naive) = NaiveDateTime::parse_from_str(text, pattern) {
            return Ok(naive.and_utc());
        }
    }
    for pattern in ["%Y-%m-%d %H:%M:%S%z", "%Y-%m-%dT%H:%M:%S%z", "%Y-%m-%d %H:%M:%S%.f%z"] {
        if let Ok(time) = DateTime::parse_from_str(text, pattern) {
            return Ok(time.with_timezone(&Utc));
        }
    }
    if let Ok(date) = NaiveDate::parse_from_str(text, "%Y-%m-%d") {
        return Ok(date.and_hms_opt(0, 0, 0).ok_or_else(fail)?.and_utc());
    }
    Err(fail())
}

/// Arithmetic involving a Duration: `+`/`-` between durations, scaling by a
/// number, dividing by a number, and the ratio or remainder of two durations.
fn duration_binary(left: Value, right: Value, op: BinaryOp) -> Result<Value, RillError> {
    use std::time::Duration;
    let overflow = || RillError::new("duration overflow");
    let scale = |d: Duration, factor: f64| -> Result<Value, RillError> {
        if !factor.is_finite() || factor < 0.0 {
            return Err(RillError::new("a duration cannot be scaled by a negative number"));
        }
        Duration::try_from_secs_f64(d.as_secs_f64() * factor)
            .map(Value::Duration)
            .map_err(|_| overflow())
    };
    match (&left, &right, op) {
        (Value::Duration(a), Value::Duration(b), BinaryOp::Add) => {
            a.checked_add(*b).map(Value::Duration).ok_or_else(overflow)
        }
        (Value::Duration(a), Value::Duration(b), BinaryOp::Sub) => a
            .checked_sub(*b)
            .map(Value::Duration)
            .ok_or_else(|| RillError::new("duration underflow: the result would be negative")),
        (Value::Duration(d), Value::Int(n), BinaryOp::Mul)
        | (Value::Int(n), Value::Duration(d), BinaryOp::Mul) => {
            if *n < 0 {
                return Err(RillError::new("a duration cannot be scaled by a negative number"));
            }
            match u32::try_from(*n) {
                Ok(n) => d.checked_mul(n).map(Value::Duration).ok_or_else(overflow),
                Err(_) => scale(*d, *n as f64),
            }
        }
        (Value::Duration(d), Value::Float(f), BinaryOp::Mul)
        | (Value::Float(f), Value::Duration(d), BinaryOp::Mul) => scale(*d, *f),
        (Value::Duration(d), Value::Int(n), BinaryOp::Div) => {
            if *n <= 0 {
                return Err(RillError::new("a duration can only be divided by a positive number"));
            }
            match u32::try_from(*n) {
                Ok(n) => d.checked_div(n).map(Value::Duration).ok_or_else(overflow),
                Err(_) => scale(*d, 1.0 / *n as f64),
            }
        }
        (Value::Duration(d), Value::Float(f), BinaryOp::Div) => {
            if f.is_nan() || *f <= 0.0 {
                return Err(RillError::new("a duration can only be divided by a positive number"));
            }
            scale(*d, 1.0 / *f)
        }
        (Value::Duration(a), Value::Duration(b), BinaryOp::Div) => {
            if b.is_zero() {
                return Err(RillError::new("division by a zero duration"));
            }
            Ok(Value::Float(a.as_secs_f64() / b.as_secs_f64()))
        }
        (Value::Duration(a), Value::Duration(b), BinaryOp::Rem) => {
            if b.is_zero() {
                return Err(RillError::new("remainder by a zero duration"));
            }
            let nanos = a.as_nanos() % b.as_nanos();
            Ok(Value::Duration(Duration::from_nanos(nanos as u64)))
        }
        _ => Err(RillError::new(format!(
            "operator '{}' is not defined for {} and {}",
            operator_symbol(op),
            left.type_name(),
            right.type_name()
        ))),
    }
}

fn operator_symbol(op: BinaryOp) -> &'static str {
    match op {
        BinaryOp::Add => "+",
        BinaryOp::Sub => "-",
        BinaryOp::Mul => "*",
        BinaryOp::Div => "/",
        BinaryOp::Rem => "%",
        BinaryOp::Pow => "**",
        BinaryOp::Gt => ">",
        BinaryOp::Ge => ">=",
        BinaryOp::Lt => "<",
        BinaryOp::Le => "<=",
        BinaryOp::Eq => "==",
        BinaryOp::Ne => "!=",
        BinaryOp::Range => "..",
        BinaryOp::RangeInclusive => "..=",
        BinaryOp::And => "&&",
        BinaryOp::Or => "||",
        BinaryOp::In => "in",
        BinaryOp::NotIn => "not in",
        BinaryOp::Coalesce => "??",
    }
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
        Expr::Unary { op: UnaryOp::Neg, operand } => match operand.unspanned() {
            Expr::Literal(Literal::Int(value)) => value.checked_neg(),
            _ => None,
        },
        _ => None,
    }
}

/// Equality with numeric cross-type comparison: `1 == 1.0`, recursively
/// inside collections, options, and results.
fn values_equal(left: &Value, right: &Value) -> bool {
    match (left, right) {
        (Value::Int(a), Value::Float(b)) | (Value::Float(b), Value::Int(a)) => (*a as f64) == *b,
        (Value::List(a), Value::List(b)) | (Value::Tuple(a), Value::Tuple(b)) => {
            a.len() == b.len() && a.iter().zip(b.iter()).all(|(x, y)| values_equal(x, y))
        }
        (Value::Map(a), Value::Map(b)) => {
            a.len() == b.len()
                && a.iter()
                    .all(|(key, value)| b.get(key).is_some_and(|other| values_equal(value, other)))
        }
        (Value::Option(Some(a)), Value::Option(Some(b))) => values_equal(a, b),
        (Value::Result(Ok(a)), Value::Result(Ok(b)))
        | (Value::Result(Err(a)), Value::Result(Err(b))) => values_equal(a, b),
        _ => left == right,
    }
}

/// Ordering for `<`, `<=`, `>`, `>=`: numbers, strings, and durations.
fn compare_values(
    left: &Value,
    right: &Value,
    op: BinaryOp,
) -> Result<std::cmp::Ordering, RillError> {
    use std::cmp::Ordering;
    match (left, right) {
        (Value::Int(a), Value::Int(b)) => Ok(a.cmp(b)),
        (Value::Int(_) | Value::Float(_), Value::Int(_) | Value::Float(_)) => {
            let (a, b) = (as_f64(left)?, as_f64(right)?);
            Ok(a.partial_cmp(&b).unwrap_or(Ordering::Equal))
        }
        (Value::Str(a), Value::Str(b)) => Ok(a.cmp(b)),
        (Value::Duration(a), Value::Duration(b)) => Ok(a.cmp(b)),
        (Value::Time(a), Value::Time(b)) => Ok(a.cmp(b)),
        _ => Err(RillError::new(format!(
            "operator '{}' needs two numbers, two strings, or two durations, got {} and {}",
            operator_symbol(op),
            left.type_name(),
            right.type_name()
        ))),
    }
}

/// A total order for sort keys: nulls first, then booleans, numbers, strings
/// and durations among themselves; anything else by its text.
fn order_values(a: &Value, b: &Value) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    fn rank(value: &Value) -> u8 {
        match value {
            Value::Null => 0,
            Value::Bool(_) => 1,
            Value::Int(_) | Value::Float(_) => 2,
            Value::Str(_) => 3,
            Value::Duration(_) => 4,
            Value::Time(_) => 5,
            _ => 6,
        }
    }
    match (a, b) {
        (Value::Bool(x), Value::Bool(y)) => x.cmp(y),
        (Value::Int(x), Value::Int(y)) => x.cmp(y),
        (Value::Int(_) | Value::Float(_), Value::Int(_) | Value::Float(_)) => {
            let (x, y) = (as_f64(a).unwrap_or(0.0), as_f64(b).unwrap_or(0.0));
            x.partial_cmp(&y).unwrap_or(Ordering::Equal)
        }
        (Value::Str(x), Value::Str(y)) => x.cmp(y),
        (Value::Duration(x), Value::Duration(y)) => x.cmp(y),
        (Value::Time(x), Value::Time(y)) => x.cmp(y),
        _ => rank(a).cmp(&rank(b)).then_with(|| a.as_string_lossy().cmp(&b.as_string_lossy())),
    }
}

/// `needle in container` for lists (by value), maps (by key), and strings.
fn contains_value(container: &Value, needle: &Value) -> Result<bool, RillError> {
    match container {
        Value::List(items) => Ok(items.iter().any(|item| values_equal(item, needle))),
        Value::Map(map) => match needle {
            Value::Str(key) => Ok(map.contains_key(key)),
            other => Err(RillError::new(format!(
                "`in` on a Map needs a Str key, got {}",
                other.type_name()
            ))),
        },
        Value::Str(text) => match needle {
            Value::Str(part) => Ok(text.contains(part.as_str())),
            other => {
                Err(RillError::new(format!("`in` on a Str needs a Str, got {}", other.type_name())))
            }
        },
        other => Err(RillError::new(format!(
            "`in` needs a List, Map, or Str on the right, got {}",
            other.type_name()
        ))),
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

/// A script-argument error: reported with `rill::usage` and exit status 2.
fn usage_error(message: impl Into<String>) -> RillError {
    RillError::new(message)
        .with_code("rill::usage")
        .with_help("run the script with --help for usage")
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
        Expr::Slice { base, start, end, .. } => {
            contains_implicit(base)
                || start.as_deref().is_some_and(contains_implicit)
                || end.as_deref().is_some_and(contains_implicit)
        }
        Expr::Binary { left, right, .. } => contains_implicit(left) || contains_implicit(right),
        Expr::Pipe { left, right, .. } => contains_implicit(left) || contains_implicit(right),
        Expr::RegexMatch { value, .. }
        | Expr::RegexSubstitution { value, .. }
        | Expr::Match { value, .. } => contains_implicit(value),
        Expr::NamedArg { value, .. } => contains_implicit(value),
        Expr::Try(value) => contains_implicit(value),
        Expr::For { iter, .. } => contains_implicit(iter),
        Expr::Block(stmts) => stmts.iter().any(|stmt| match stmt.unspanned() {
            Stmt::Let { expr, .. } | Stmt::Expr(expr) | Stmt::Emit(expr) => contains_implicit(expr),
            Stmt::Assign { target, expr, .. } => {
                contains_implicit(target) || contains_implicit(expr)
            }
            Stmt::While { cond, .. } => contains_implicit(cond),
            Stmt::Return(Some(expr)) => contains_implicit(expr),
            _ => false,
        }),
        Expr::On { host, .. } => contains_implicit(host),
        Expr::WithEnv { bindings, .. } => bindings.iter().any(|(_, expr)| contains_implicit(expr)),
        Expr::Unary { operand, .. } => contains_implicit(operand),
        Expr::Closure { .. } | Expr::Literal(_) | Expr::Ident(_) | Expr::Command(_) => false,
        Expr::Located { expr, .. } => contains_implicit(expr),
        Expr::Interpolated(parts) => parts.iter().any(|part| match part {
            StrPart::Expr(expr) => contains_implicit(expr),
            _ => false,
        }),
        Expr::If { cond, .. } => contains_implicit(cond),
        Expr::Loop { .. } => false,
    }
}

/// Match `value` against `pattern`, collecting bound names.  Regex patterns
/// match the value's text form, so non-strings still work as before.
fn match_pattern(
    pattern: &Pattern,
    value: &Value,
    bindings: &mut Vec<(String, Value)>,
) -> Result<bool, RillError> {
    Ok(match (pattern, value) {
        (Pattern::Wildcard, _) => true,
        (Pattern::Bind(name), _) => {
            bindings.push((name.clone(), value.clone()));
            true
        }
        (Pattern::Literal(literal), _) => values_equal(&literal_value(literal), value),
        (Pattern::Regex(regex), _) => match regex_captures(&value.as_string_lossy(), regex)? {
            Some(captures) => {
                bindings.extend(captures.into_iter().map(|(name, text)| (name, Value::Str(text))));
                true
            }
            None => false,
        },
        (Pattern::Tuple(items), Value::Tuple(values)) => {
            items.len() == values.len() && match_all(items, values, bindings)?
        }
        (Pattern::List { items, rest }, Value::List(values)) => {
            let length_ok =
                if *rest { values.len() >= items.len() } else { values.len() == items.len() };
            length_ok && match_all(items, &values[..items.len()], bindings)?
        }
        (Pattern::Some(inner), Value::Option(Some(value)))
        | (Pattern::Ok(inner), Value::Result(Ok(value)))
        | (Pattern::Err(inner), Value::Result(Err(value))) => {
            match_pattern(inner, value, bindings)?
        }
        (Pattern::None, Value::Option(None)) => true,
        (Pattern::Or(alternatives), _) => {
            for alternative in alternatives {
                let mut scratch = Vec::new();
                if match_pattern(alternative, value, &mut scratch)? {
                    bindings.extend(scratch);
                    return Ok(true);
                }
            }
            false
        }
        _ => false,
    })
}

fn match_all(
    patterns: &[Pattern],
    values: &[Value],
    bindings: &mut Vec<(String, Value)>,
) -> Result<bool, RillError> {
    for (pattern, value) in patterns.iter().zip(values) {
        if !match_pattern(pattern, value, bindings)? {
            return Ok(false);
        }
    }
    Ok(true)
}

fn literal_value(literal: &Literal) -> Value {
    match literal {
        Literal::Null => Value::Null,
        Literal::Bool(value) => Value::Bool(*value),
        Literal::Int(value) => Value::Int(*value),
        Literal::Float(value) => Value::Float(*value),
        Literal::Duration(value) => Value::Duration(*value),
        Literal::Str(value) => Value::Str(value.clone()),
        Literal::Regex(value) => Value::Regex(value.clone()),
    }
}

/// One `on ... { }` layer wrapping commands run inside it.
#[derive(Debug, Clone, PartialEq)]
enum RemoteLayer {
    /// `on "host"`: run through ssh with a persistent control connection.
    Ssh(String),
    /// `on ["cmd", ...]` (including `docker(name)`): prefix the argv.
    Exec(Vec<String>),
}

/// Build the full local argv for `argv` inside the given layers, innermost
/// first.  Remote words are shell-quoted so each arrives as one argument,
/// only the outermost ssh carries the local control-socket options, and
/// `docker exec` gets `-i` when stdin is piped.
fn wrap_remote_layers(
    layers: &[RemoteLayer],
    mut argv: Vec<String>,
    stdin_piped: bool,
    control: &[String],
) -> Vec<String> {
    let outermost_ssh = layers.iter().position(|layer| matches!(layer, RemoteLayer::Ssh(_)));
    for (index, layer) in layers.iter().enumerate().rev() {
        argv = match layer {
            RemoteLayer::Exec(prefix) => {
                let mut wrapped = prefix.clone();
                let docker_exec = wrapped.windows(2).any(|w| w[0] == "docker" && w[1] == "exec");
                if stdin_piped && docker_exec && !wrapped.iter().any(|word| word == "-i") {
                    let at = wrapped.iter().position(|word| word == "exec").map_or(0, |i| i + 1);
                    wrapped.insert(at, "-i".to_string());
                }
                wrapped.extend(argv);
                wrapped
            }
            RemoteLayer::Ssh(host) => {
                let mut wrapped = vec!["ssh".to_string()];
                if Some(index) == outermost_ssh {
                    wrapped.extend(control.iter().cloned());
                }
                wrapped.push(host.clone());
                wrapped.push("--".to_string());
                wrapped.extend(crate::runtime::shell_quote_argv(&argv));
                wrapped
            }
        };
    }
    argv
}

/// Reject host and container names that ssh/docker would read as options.
fn validate_remote_name(what: &str, name: &str) -> Result<(), RillError> {
    if name.is_empty()
        || name.starts_with('-')
        || name.chars().any(|c| c.is_whitespace() || c == ';')
    {
        return Err(RillError::new(format!("invalid {what} '{name}'"))
            .with_help("names must be nonempty and must not start with '-' or contain spaces"));
    }
    Ok(())
}

/// Normalise slice bounds: negative values count from the end and
/// out-of-range values are clamped.  `to` may end up before `from` for a
/// reversed range; callers decide whether that is empty or an error.
fn slice_bounds(
    len: usize,
    start: Option<i64>,
    end: Option<i64>,
    inclusive: bool,
) -> (usize, usize) {
    let len_i64 = len as i64;
    let normalise = |index: i64| if index < 0 { len_i64 + index } else { index };
    let from = start.map_or(0, normalise).clamp(0, len_i64) as usize;
    let to =
        end.map_or(len_i64, |end| normalise(end) + i64::from(inclusive)).clamp(0, len_i64) as usize;
    (from, to)
}

/// Positional values and `name = value` pairs of a call.
type CallArgs = (Vec<Value>, Vec<(String, Value)>);

/// Whether an expression is literal source text, i.e. a string literal or an
/// interpolation, for the purpose of tilde expansion.
fn is_source_text(expr: &Expr) -> bool {
    matches!(expr.unspanned(), Expr::Literal(Literal::Str(_)) | Expr::Interpolated(_))
}

fn interpolated_text(value: &Value) -> String {
    match value {
        Value::Option(_) => value.to_string(),
        other => other.as_string_lossy(),
    }
}

fn wrap_declared_result(function: &Function, value: Value) -> Value {
    if function.return_type.is_none() {
        // An unannotated body that can propagate a failure always returns the
        // inferred wrapper, so success and failure have the same shape.
        return match function.propagation.get() {
            Propagation::None => value,
            Propagation::Result => match value {
                Value::Result(_) | Value::Option(_) => value,
                other => Value::Result(Ok(Box::new(other))),
            },
            Propagation::Option => match value {
                Value::Result(_) | Value::Option(_) => value,
                other => Value::Option(Some(Box::new(other))),
            },
        };
    }
    match function.return_type.as_ref() {
        Some(TypeName::Result(..)) if !matches!(value, Value::Result(_)) => {
            Value::Result(Ok(Box::new(value)))
        }
        Some(TypeName::Result(_, Some(error_type))) if **error_type == TypeName::Error => {
            match value {
                Value::Result(Err(error)) if !matches!(*error, Value::Error(_)) => {
                    Value::Result(Err(Box::new(Value::Error(Box::new(ErrorValue {
                        kind: ErrorKind::Runtime,
                        message: error.as_string_lossy(),
                        operation: None,
                        status: None,
                        span: None,
                    })))))
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
    Arc::unwrap_or_clone(items)
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

#[cfg(test)]
mod remote_tests {
    use super::*;

    #[test]
    fn layers_nest_with_quoting_and_control_on_outermost_ssh() {
        let control = vec!["-o".to_string(), "ControlMaster=auto".to_string()];
        let layers = vec![
            RemoteLayer::Ssh("outer".into()),
            RemoteLayer::Exec(vec!["sudo".into(), "docker".into(), "exec".into(), "c".into()]),
            RemoteLayer::Ssh("inner".into()),
        ];
        let argv = vec!["printf".to_string(), "a b".to_string()];
        let wrapped = wrap_remote_layers(&layers, argv, true, &control);
        assert_eq!(
            wrapped,
            vec![
                "ssh",
                "-o",
                "ControlMaster=auto",
                "outer",
                "--",
                "sudo",
                "docker",
                "exec",
                "-i",
                "c",
                "ssh",
                "inner",
                "--",
                "printf",
                "''\\''a b'\\'''"
            ]
        );
        let plain = wrap_remote_layers(&layers[1..2], vec!["ls".into()], false, &control);
        assert_eq!(plain, vec!["sudo", "docker", "exec", "c", "ls"]);
    }
}
