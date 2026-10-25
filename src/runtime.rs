use crate::diagnostic::RillError;
use crate::types::ErrorKind;
use crate::types::ProcessResult;
use crate::types::StreamHandle;
use std::collections::BTreeMap;
use std::fs::File;
use std::io::{self, Read, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio_util::sync::CancellationToken;

#[derive(Debug, Clone)]
pub struct ProcessSpec {
    pub argv: Vec<String>,
    pub input: Option<Vec<u8>>,
    pub io: CommandIo,
    /// Cancelling this token (or dropping the handle) terminates the process.
    pub cancellation: Option<CancellationToken>,
}

/// How long a process group gets to exit after SIGTERM before SIGKILL.
const TERMINATION_GRACE: Duration = Duration::from_secs(1);

/// Quote a word for a POSIX shell so it arrives as exactly one argument.
pub fn shell_quote(word: &str) -> String {
    let bare = !word.is_empty()
        && word.chars().all(|c| c.is_ascii_alphanumeric() || "_./:=@%+,-".contains(c));
    if bare {
        return word.to_string();
    }
    format!("'{}'", word.replace('\'', "'\\''"))
}

pub fn shell_quote_argv(argv: &[String]) -> Vec<String> {
    argv.iter().map(|word| shell_quote(word)).collect()
}

static SSH_RUNTIME_DIR: OnceLock<Result<PathBuf, String>> = OnceLock::new();

/// `sizeof(sockaddr_un.sun_path)` on Linux and the BSDs is 104 to 108 bytes;
/// OpenSSH rejects a path that does not fit with its trailing NUL.
const OPENSSH_CONTROL_PATH_LIMIT: usize = 104;
/// `%C` expands to a 40-character SHA-1 hex digest.
const OPENSSH_HASH_TOKEN_LEN: usize = 40;
/// While binding the socket, OpenSSH appends `.XXXXXXXXXXXXXXXX` (16 random
/// characters) to the control path and renames it into place afterwards.
const OPENSSH_MUX_TEMP_SUFFIX_LEN: usize = 17;

/// Picks the directory holding the ssh control sockets: `temp_dir` when the
/// expanded socket path (including OpenSSH's temporary suffix) fits in
/// `sun_path`, otherwise the short system temp root. Nested Nix shells and
/// macOS put `TMPDIR` deep enough that the default would not fit.
fn choose_ssh_runtime_dir(temp_dir: &std::path::Path, name: &str) -> PathBuf {
    let candidate = temp_dir.join(name);
    let expanded_len =
        candidate.as_os_str().len() + 1 + OPENSSH_HASH_TOKEN_LEN + OPENSSH_MUX_TEMP_SUFFIX_LEN;
    if expanded_len < OPENSSH_CONTROL_PATH_LIMIT {
        candidate
    } else {
        PathBuf::from("/tmp").join(name)
    }
}

fn ssh_runtime_dir() -> Result<PathBuf, RillError> {
    SSH_RUNTIME_DIR
        .get_or_init(|| {
            let nonce = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|duration| duration.as_nanos())
                .unwrap_or_default();
            let name = format!("rill-ssh-{}-{nonce:x}", std::process::id());
            let path = choose_ssh_runtime_dir(&std::env::temp_dir(), &name);
            std::fs::create_dir(&path).map_err(|error| error.to_string())?;

            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700))
                    .map_err(|error| error.to_string())?;
            }

            Ok(path)
        })
        .clone()
        .map_err(|error| RillError::new(format!("failed to create SSH runtime directory: {error}")))
}

pub(crate) fn ssh_control_options() -> Result<Vec<String>, RillError> {
    let control_path = ssh_runtime_dir()?.join("%C");
    Ok(vec![
        "-o".to_string(),
        "ControlMaster=auto".to_string(),
        "-o".to_string(),
        format!("ControlPath={}", control_path.display()),
        "-o".to_string(),
        "ControlPersist=30s".to_string(),
    ])
}

type ProcessTask = thread::JoinHandle<Result<ProcessResult, RillError>>;

#[derive(Clone)]
pub struct ProcessHandle {
    state: Arc<ProcessState>,
}

struct ProcessState {
    result: Mutex<Option<ProcessTask>>,
    cancellation: CancellationToken,
}

impl Drop for ProcessState {
    fn drop(&mut self) {
        self.cancellation.cancel();
        let task = self.result.get_mut().unwrap_or_else(|error| error.into_inner()).take();
        if let Some(task) = task {
            let _ = task.join();
        }
    }
}

impl std::fmt::Debug for ProcessHandle {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.debug_struct("ProcessHandle").finish_non_exhaustive()
    }
}

impl PartialEq for ProcessHandle {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.state, &other.state)
    }
}

pub trait ProcessRuntime {
    fn spawn(&self, spec: ProcessSpec) -> Result<ProcessHandle, RillError>;
    fn await_process(&self, handle: ProcessHandle) -> Result<ProcessResult, RillError>;
    /// Whether the process behind `handle` has finished (or was already awaited).
    fn is_finished(&self, handle: &ProcessHandle) -> bool;
    /// Run every spec, at most `limit` at a time, returning results in order.
    fn parallel(
        &self,
        specs: Vec<ProcessSpec>,
        limit: usize,
    ) -> Result<Vec<ProcessResult>, RillError>;
}

#[derive(Debug, Default, Clone, Copy)]
pub struct TokioProcessRuntime;

impl ProcessRuntime for TokioProcessRuntime {
    fn spawn(&self, spec: ProcessSpec) -> Result<ProcessHandle, RillError> {
        validate_argv(&spec.argv)?;
        let cancellation =
            spec.cancellation.as_ref().map(CancellationToken::child_token).unwrap_or_default();
        let task_cancellation = cancellation.clone();
        let result = thread::Builder::new()
            .name("rill-process".to_string())
            .spawn(move || {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_io()
                    .build()
                    .map_err(|err| {
                        RillError::new(format!("failed to create process runtime: {err}"))
                            .with_kind(ErrorKind::Runtime)
                    })?;
                runtime.block_on(run_command_async(spec, task_cancellation))
            })
            .map_err(|err| {
                RillError::new(format!("failed to start process task: {err}"))
                    .with_kind(ErrorKind::Runtime)
            })?;
        Ok(ProcessHandle {
            state: Arc::new(ProcessState { result: Mutex::new(Some(result)), cancellation }),
        })
    }

    fn is_finished(&self, handle: &ProcessHandle) -> bool {
        handle
            .state
            .result
            .lock()
            .map(|task| task.as_ref().map_or(true, |task| task.is_finished()))
            .unwrap_or(true)
    }

    fn await_process(&self, handle: ProcessHandle) -> Result<ProcessResult, RillError> {
        let join = handle
            .state
            .result
            .lock()
            .map_err(|_| {
                RillError::new("process handle lock poisoned").with_kind(ErrorKind::Runtime)
            })?
            .take()
            .ok_or_else(|| {
                RillError::new("process handle has already been awaited")
                    .with_kind(ErrorKind::Runtime)
            })?;
        join.join()
            .map_err(|_| RillError::new("process task panicked").with_kind(ErrorKind::Runtime))?
    }

    fn parallel(
        &self,
        specs: Vec<ProcessSpec>,
        limit: usize,
    ) -> Result<Vec<ProcessResult>, RillError> {
        let mut results = Vec::with_capacity(specs.len());
        for batch in specs.chunks(limit.max(1)) {
            let handles = batch
                .iter()
                .cloned()
                .map(|spec| self.spawn(spec))
                .collect::<Result<Vec<_>, _>>()?;
            for handle in handles {
                results.push(self.await_process(handle)?);
            }
        }
        Ok(results)
    }
}

fn validate_argv(argv: &[String]) -> Result<(), RillError> {
    if argv.is_empty() {
        Err(RillError::new("empty command").with_kind(ErrorKind::Process))
    } else {
        Ok(())
    }
}

async fn run_command_async(
    spec: ProcessSpec,
    cancellation: CancellationToken,
) -> Result<ProcessResult, RillError> {
    if cancellation.is_cancelled() {
        return Err(cancelled_error());
    }
    let command = prepare_command(&spec.argv, &spec.io, spec.input.is_some(), true)?;
    let mut command = tokio::process::Command::from(command);
    command.kill_on_drop(true);
    let mut child = command.spawn().map_err(process_error)?;
    let writer = match spec.input {
        Some(bytes) => {
            let mut stdin = child.stdin.take().expect("configured piped stdin");
            Some(tokio::spawn(async move {
                use tokio::io::AsyncWriteExt;
                stdin.write_all(&bytes).await
            }))
        }
        None => None,
    };
    let stdout = child.stdout.take().map(|mut stdout| {
        tokio::spawn(async move {
            use tokio::io::AsyncReadExt;
            let mut bytes = Vec::new();
            stdout.read_to_end(&mut bytes).await.map(|_| bytes)
        })
    });
    let stderr = child.stderr.take().map(|mut stderr| {
        tokio::spawn(async move {
            use tokio::io::AsyncReadExt;
            let mut bytes = Vec::new();
            stderr.read_to_end(&mut bytes).await.map(|_| bytes)
        })
    });
    let mut was_cancelled = false;
    let status = tokio::select! {
        result = child.wait() => result.map_err(process_error),
        () = cancellation.cancelled() => {
            was_cancelled = true;
            // SIGTERM first so the process can clean up, SIGKILL if it lingers.
            if let Some(id) = child.id() {
                signal_process_group(id, libc::SIGTERM);
            }
            match tokio::time::timeout(TERMINATION_GRACE, child.wait()).await {
                Ok(status) => status.map_err(process_error),
                Err(_) => {
                    if let Some(id) = child.id() {
                        signal_process_group(id, libc::SIGKILL);
                    }
                    let _ = child.kill().await;
                    child.wait().await.map_err(process_error)
                }
            }
        }
    };
    if let Some(writer) = writer {
        check_write(writer.await.map_err(|err| RillError::new(err.to_string()))?)?;
    }
    let out = join_async_reader(stdout, "stdout").await?;
    let err = join_async_reader(stderr, "stderr").await?;
    if was_cancelled {
        return Err(cancelled_error());
    }
    Ok(process_result(status?, out, err))
}

async fn join_async_reader(
    reader: Option<tokio::task::JoinHandle<io::Result<Vec<u8>>>>,
    name: &str,
) -> Result<Vec<u8>, RillError> {
    match reader {
        Some(reader) => {
            reader.await.map_err(|_| io_error(format!("{name} reader panicked")))?.map_err(io_error)
        }
        None => Ok(Vec::new()),
    }
}

#[derive(Debug, Clone)]
pub enum PipelineValue {
    Value(crate::types::Value),
    ByteStream(Vec<u8>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum StreamMode {
    #[default]
    Capture,
    Inherit,
    Null,
}

#[derive(Debug, Clone, Default)]
pub struct CommandIo {
    pub cwd: Option<PathBuf>,
    pub env: BTreeMap<String, String>,
    pub stdin_file: Option<PathBuf>,
    pub stdin_null: bool,
    /// `stdin: inherit` was requested explicitly (conflicts with piped input).
    pub stdin_inherit: bool,
    pub stdout_file: Option<PathBuf>,
    pub stderr_file: Option<PathBuf>,
    pub all_file: Option<PathBuf>,
    pub stdout_mode: StreamMode,
    pub stderr_mode: StreamMode,
}

pub fn run_command(
    argv: &[String],
    input: Option<Vec<u8>>,
    io: &CommandIo,
) -> Result<ProcessResult, RillError> {
    run_graph(&[argv.to_vec()], std::slice::from_ref(io), input, &CancellationToken::new(), false)
}

pub(crate) fn run_command_cancellable(
    argv: &[String],
    input: Option<Vec<u8>>,
    io: &CommandIo,
    cancellation: &CancellationToken,
) -> Result<ProcessResult, RillError> {
    run_graph(&[argv.to_vec()], std::slice::from_ref(io), input, cancellation, true)
}

pub fn run_pipeline(
    argv: &[Vec<String>],
    io_configs: &[CommandIo],
) -> Result<ProcessResult, RillError> {
    run_graph(argv, io_configs, None, &CancellationToken::new(), false)
}

pub(crate) fn run_pipeline_cancellable(
    argv: &[Vec<String>],
    io_configs: &[CommandIo],
    cancellation: &CancellationToken,
) -> Result<ProcessResult, RillError> {
    run_graph(argv, io_configs, None, cancellation, true)
}

// Own children until every exit path has reaped them, including partial spawn failures.
#[derive(Default)]
struct Children(Vec<std::process::Child>);

impl Drop for Children {
    fn drop(&mut self) {
        for child in &mut self.0 {
            terminate_child(child);
        }
        for child in &mut self.0 {
            let _ = child.wait();
        }
    }
}

fn run_graph(
    argv: &[Vec<String>],
    configs: &[CommandIo],
    input: Option<Vec<u8>>,
    cancellation: &CancellationToken,
    isolate_process_group: bool,
) -> Result<ProcessResult, RillError> {
    if cancellation.is_cancelled() {
        return Err(cancelled_error());
    }
    if argv.is_empty() || argv.len() != configs.len() {
        return Err(RillError::new("invalid pipeline command/config count"));
    }
    for (index, config) in configs.iter().enumerate() {
        validate_io(config, index > 0 || input.is_some())?;
        if index + 1 < configs.len()
            && (config.stdout_file.is_some()
                || config.all_file.is_some()
                || config.stdout_mode != StreamMode::Capture)
        {
            return Err(io_error("nonterminal pipeline stdout must be piped"));
        }
    }
    // Open redirections and validate all commands before spawning any child.
    let mut commands = argv
        .iter()
        .zip(configs)
        .enumerate()
        .map(|(i, (args, config))| {
            prepare_command(args, config, i > 0 || input.is_some(), isolate_process_group)
        })
        .collect::<Result<Vec<_>, _>>()?;
    let mut children = Children::default();
    for (index, command) in commands.iter_mut().enumerate() {
        if index > 0 {
            let stdout = children.0[index - 1].stdout.take().expect("pipeline stdout");
            command.stdin(Stdio::from(stdout));
        }
        children.0.push(command.spawn().map_err(process_error)?);
    }
    thread::scope(|scope| {
        let writer = input.map(|bytes| {
            let mut stdin = children.0[0].stdin.take().expect("configured piped stdin");
            scope.spawn(move || stdin.write_all(&bytes))
        });
        let mut readers = Vec::new();
        for child in &mut children.0 {
            let stderr = child.stderr.take();
            readers.push(scope.spawn(move || read_stream(stderr)));
        }
        let stdout = children.0.last_mut().expect("nonempty pipeline").stdout.take();
        let output = scope.spawn(move || read_stream(stdout));
        let mut status = None;
        let mut wait_error = None;
        let terminal = children.0.len() - 1;
        loop {
            if cancellation.is_cancelled() {
                wait_error = Some(cancelled_error());
                for child in &mut children.0 {
                    terminate_child(child);
                }
                break;
            }
            match children.0[terminal].try_wait() {
                Ok(Some(value)) => {
                    status = Some(value);
                    for child in &mut children.0[..terminal] {
                        terminate_child(child);
                    }
                    break;
                }
                Ok(None) => thread::sleep(Duration::from_millis(5)),
                Err(error) => {
                    wait_error = Some(process_error(error));
                    break;
                }
            }
        }
        if wait_error.is_none() {
            for child in &mut children.0[..terminal] {
                if let Err(error) = child.wait() {
                    wait_error = Some(process_error(error));
                    break;
                }
            }
        }
        if wait_error.is_some() {
            for child in &mut children.0 {
                terminate_child(child);
            }
        }
        if let Some(writer) = writer {
            check_write(writer.join().map_err(|_| io_error("stdin writer panicked"))?)?;
        }
        let out = output.join().map_err(|_| io_error("stdout reader panicked"))??;
        let mut err = Vec::new();
        for reader in readers {
            err.extend(reader.join().map_err(|_| io_error("stderr reader panicked"))??);
        }
        if let Some(error) = wait_error {
            return Err(error);
        }
        Ok(process_result(status.expect("terminal process waited"), out, err))
    })
}

fn read_stream(stream: Option<impl Read>) -> Result<Vec<u8>, RillError> {
    let mut bytes = Vec::new();
    if let Some(mut stream) = stream {
        stream.read_to_end(&mut bytes).map_err(io_error)?;
    }
    Ok(bytes)
}

fn process_result(status: std::process::ExitStatus, out: Vec<u8>, err: Vec<u8>) -> ProcessResult {
    #[cfg(unix)]
    let signal = {
        use std::os::unix::process::ExitStatusExt;
        status.signal()
    };
    #[cfg(not(unix))]
    let signal: Option<i32> = None;
    let status = status.code().or(signal.map(|signal| 128 + signal)).unwrap_or(128);
    ProcessResult { status, signal, out, err }
}

fn check_write(result: io::Result<()>) -> Result<(), RillError> {
    match result {
        Err(error) if error.kind() != io::ErrorKind::BrokenPipe => Err(io_error(error)),
        _ => Ok(()),
    }
}

fn process_error(error: impl std::fmt::Display) -> RillError {
    RillError::new(format!("process operation failed: {error}")).with_kind(ErrorKind::Process)
}

fn io_error(error: impl std::fmt::Display) -> RillError {
    RillError::new(format!("process I/O failed: {error}")).with_kind(ErrorKind::Io)
}

fn cancelled_error() -> RillError {
    RillError::new("process cancelled").with_kind(ErrorKind::Runtime)
}

fn validate_io(io: &CommandIo, piped: bool) -> Result<(), RillError> {
    if (piped && (io.stdin_file.is_some() || io.stdin_null || io.stdin_inherit))
        || (io.stdin_null && io.stdin_file.is_some())
        || (io.stdin_inherit && (io.stdin_null || io.stdin_file.is_some()))
    {
        return Err(io_error("conflicting stdin settings"));
    }
    if io.all_file.is_some()
        && (io.stdout_file.is_some()
            || io.stderr_file.is_some()
            || io.stdout_mode != StreamMode::Capture
            || io.stderr_mode != StreamMode::Capture)
    {
        return Err(io_error("combined output conflicts with stdout/stderr settings"));
    }
    if (io.stdout_file.is_some() && io.stdout_mode != StreamMode::Capture)
        || (io.stderr_file.is_some() && io.stderr_mode != StreamMode::Capture)
    {
        return Err(io_error("file redirection conflicts with stream mode"));
    }
    Ok(())
}

fn stream_stdio(mode: StreamMode) -> Stdio {
    match mode {
        StreamMode::Capture => Stdio::piped(),
        StreamMode::Inherit => Stdio::inherit(),
        StreamMode::Null => Stdio::null(),
    }
}

fn prepare_command(
    argv: &[String],
    io: &CommandIo,
    piped: bool,
    isolate_process_group: bool,
) -> Result<Command, RillError> {
    validate_argv(argv)?;
    validate_io(io, piped)?;
    let mut command = Command::new(&argv[0]);
    command.args(&argv[1..]);
    #[cfg(unix)]
    if isolate_process_group {
        use std::os::unix::process::CommandExt;
        unsafe {
            command.pre_exec(|| {
                if libc::setpgid(0, 0) == -1 {
                    Err(io::Error::last_os_error())
                } else {
                    Ok(())
                }
            });
        }
    }
    #[cfg(not(unix))]
    let _ = isolate_process_group;
    if let Some(cwd) = &io.cwd {
        command.current_dir(cwd);
    }
    command.envs(&io.env);
    command.stdin(if piped {
        Stdio::piped()
    } else if let Some(path) = &io.stdin_file {
        Stdio::from(File::open(path).map_err(io_error)?)
    } else if io.stdin_null {
        Stdio::null()
    } else {
        Stdio::inherit()
    });
    if let Some(path) = &io.all_file {
        let file = File::create(path).map_err(io_error)?;
        command.stdout(Stdio::from(file.try_clone().map_err(io_error)?));
        command.stderr(Stdio::from(file));
    } else {
        command.stdout(match &io.stdout_file {
            Some(path) => Stdio::from(File::create(path).map_err(io_error)?),
            None => stream_stdio(io.stdout_mode),
        });
        command.stderr(match &io.stderr_file {
            Some(path) => Stdio::from(File::create(path).map_err(io_error)?),
            None => stream_stdio(io.stderr_mode),
        });
    }
    Ok(command)
}

/// Stop a child and its process group: SIGTERM, a short grace period, then
/// SIGKILL for anything still running.
fn terminate_child(child: &mut std::process::Child) {
    if matches!(child.try_wait(), Ok(Some(_))) {
        return;
    }
    signal_process_group(child.id(), libc::SIGTERM);
    let deadline = std::time::Instant::now() + TERMINATION_GRACE;
    while std::time::Instant::now() < deadline {
        if matches!(child.try_wait(), Ok(Some(_))) {
            // The direct child is gone; make sure the rest of its group is too.
            signal_process_group(child.id(), libc::SIGKILL);
            return;
        }
        thread::sleep(Duration::from_millis(10));
    }
    signal_process_group(child.id(), libc::SIGKILL);
    let _ = child.kill();
}

/// Send `signal` to the process group led by `id` (a no-op when the child
/// was not started in its own group).
fn signal_process_group(id: u32, signal: libc::c_int) {
    #[cfg(unix)]
    if let Ok(group) = i32::try_from(id) {
        unsafe {
            libc::kill(-group, signal);
        }
    }
    #[cfg(not(unix))]
    let _ = (id, signal);
}

pub fn spawn_tail(hosts: Vec<String>, path: String) -> Result<StreamHandle, RillError> {
    use std::sync::mpsc;
    use tokio::io::AsyncBufReadExt;

    let ssh_options = ssh_control_options()?;
    let (tx, rx) = mpsc::channel::<(String, String)>();

    for host in hosts {
        let tx = tx.clone();
        let path = path.clone();
        let host_clone = host.clone();
        let ssh_options = ssh_options.clone();
        thread::Builder::new()
            .name("rill-tail".to_string())
            .spawn(move || {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_io()
                    .build()
                    .expect("tokio runtime");
                runtime.block_on(async move {
                    let mut cmd = tokio::process::Command::new("ssh");
                    cmd.args(&ssh_options);
                    cmd.args([&host_clone, "--", "tail", "-f", &path]);
                    cmd.stdout(std::process::Stdio::piped());
                    cmd.stderr(std::process::Stdio::null());
                    let mut child = match cmd.spawn() {
                        Ok(c) => c,
                        Err(_) => return,
                    };
                    let stdout = match child.stdout.take() {
                        Some(s) => s,
                        None => return,
                    };
                    let reader = tokio::io::BufReader::new(stdout);
                    let mut lines = reader.lines();
                    while let Ok(Some(line)) = lines.next_line().await {
                        if tx.send((line, host_clone.clone())).is_err() {
                            break;
                        }
                    }
                    let _ = child.wait().await;
                });
            })
            .map_err(|e| RillError::new(format!("failed to spawn tail thread: {e}")))?;
    }

    Ok(StreamHandle::new(rx))
}

#[cfg(test)]
mod tests {
    use super::ssh_control_options;

    #[test]
    fn shell_quoting_keeps_each_word_intact() {
        let words: Vec<String> =
            ["plain", "two words", "it's", "", "$HOME", "a;b", "ünï", "/usr/bin:x"]
                .iter()
                .map(|w| w.to_string())
                .collect();
        assert_eq!(
            super::shell_quote_argv(&words),
            vec![
                "plain",
                "'two words'",
                "'it'\\''s'",
                "''",
                "'$HOME'",
                "'a;b'",
                "'ünï'",
                "/usr/bin:x"
            ]
        );
    }

    #[test]
    fn ssh_control_path_uses_private_runtime_directory_and_hash_token() {
        let options = ssh_control_options().expect("SSH options");
        let control_path = options
            .iter()
            .find_map(|option| option.strip_prefix("ControlPath="))
            .expect("ControlPath option");

        assert!(control_path.ends_with("/%C"));
        assert!(!control_path.contains("example.com"));
        let expanded_control_path =
            control_path.replace("%C", &"x".repeat(40)) + ".XXXXXXXXXXXXXXXX";
        assert!(expanded_control_path.len() < 104);

        let directory = std::path::Path::new(control_path.trim_end_matches("/%C"));
        assert!(directory.is_dir());
        assert!(directory
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.starts_with("rill-ssh-")));

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(directory.metadata().unwrap().permissions().mode() & 0o777, 0o700);
        }

        let second = ssh_control_options().expect("SSH options again");
        assert_eq!(options, second);
    }

    #[test]
    fn ssh_runtime_dir_falls_back_to_tmp_when_the_socket_path_would_not_fit() {
        use super::choose_ssh_runtime_dir;
        let name = "rill-ssh-1709546-18df7a8f4c3b2a1d";
        let expanded = |dir: &std::path::Path| dir.join("x".repeat(40)).as_os_str().len() + 17;

        let short = std::path::Path::new("/tmp");
        assert_eq!(choose_ssh_runtime_dir(short, name), short.join(name));

        // A nested nix shell: 99 bytes with the hash, 116 with ssh's temp suffix.
        let nested = std::path::Path::new("/tmp/nix-shell.AkYKWK");
        let chosen = choose_ssh_runtime_dir(nested, name);
        assert_eq!(chosen, short.join(name));
        assert!(expanded(&chosen) < 104);

        let deep = std::path::Path::new("/var/folders/9f/abcdefghijklmnopqrstuvwxyz01234/T");
        assert_eq!(choose_ssh_runtime_dir(deep, name), short.join(name));
    }
}
