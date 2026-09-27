use std::collections::{HashMap, VecDeque};
use std::io::{self, BufRead, IsTerminal, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use crate::error::{Span, SparError};

use super::resource::{ResourceId, ResourceTable};
use super::stream::StreamResource;
use super::value::Value;

#[derive(Clone, Debug)]
pub enum RuntimeOutput {
    Stdout,
    Stderr,
    Buffer(Arc<Mutex<Vec<u8>>>),
}

impl RuntimeOutput {
    fn write_all(&self, bytes: &[u8]) -> io::Result<()> {
        match self {
            RuntimeOutput::Stdout => {
                let mut out = io::stdout().lock();
                out.write_all(bytes)?;
                out.flush()
            }
            RuntimeOutput::Stderr => {
                let mut out = io::stderr().lock();
                out.write_all(bytes)?;
                out.flush()
            }
            RuntimeOutput::Buffer(buffer) => {
                let mut guard = buffer
                    .lock()
                    .map_err(|_| io::Error::other("runtime output buffer lock poisoned"))?;
                guard.extend_from_slice(bytes);
                Ok(())
            }
        }
    }

    fn is_terminal(&self) -> bool {
        match self {
            RuntimeOutput::Stdout => io::stdout().is_terminal(),
            RuntimeOutput::Stderr => io::stderr().is_terminal(),
            RuntimeOutput::Buffer(_) => false,
        }
    }
}

#[derive(Clone, Debug, Default)]
pub enum RuntimeInput {
    #[default]
    Stdin,
    Buffer(Arc<Mutex<VecDeque<u8>>>),
}

impl RuntimeInput {
    pub fn from_bytes(bytes: impl Into<Vec<u8>>) -> Self {
        Self::Buffer(Arc::new(Mutex::new(bytes.into().into())))
    }

    pub fn read_remaining(&self) -> io::Result<Vec<u8>> {
        match self {
            RuntimeInput::Stdin => {
                let mut bytes = Vec::new();
                io::stdin().lock().read_to_end(&mut bytes)?;
                Ok(bytes)
            }
            RuntimeInput::Buffer(bytes) => {
                let mut guard = bytes
                    .lock()
                    .map_err(|_| io::Error::other("runtime input buffer lock poisoned"))?;
                Ok(guard.drain(..).collect())
            }
        }
    }

    pub fn read_line(&self) -> io::Result<Vec<u8>> {
        match self {
            RuntimeInput::Stdin => {
                let mut bytes = Vec::new();
                io::stdin().lock().read_until(b'\n', &mut bytes)?;
                Ok(bytes)
            }
            RuntimeInput::Buffer(bytes) => {
                let mut guard = bytes
                    .lock()
                    .map_err(|_| io::Error::other("runtime input buffer lock poisoned"))?;
                let end = guard
                    .iter()
                    .position(|byte| *byte == b'\n')
                    .map(|index| index + 1)
                    .unwrap_or(guard.len());
                Ok(guard.drain(..end).collect())
            }
        }
    }
}

/// Result of a mixed pipeline that ended at an interactive terminal: the
/// structured value itself (fully collected) and the `to FORMAT` it named, so
/// the shell can render it instead of receiving serialized bytes.
pub(crate) struct MixedCapture {
    pub value: Value,
    pub format: Option<&'static str>,
}

pub struct RuntimeContext {
    cwd: PathBuf,
    args: Vec<String>,
    environment: HashMap<String, String>,
    stdin: RuntimeInput,
    stdout: RuntimeOutput,
    stderr: RuntimeOutput,
    resources: ResourceTable,
    previous_value: Option<Value>,
    structured_terminal: bool,
    capture_mixed: bool,
    mixed_capture: Option<MixedCapture>,
    cancelled: bool,
    requested_exit: Option<i32>,
    inherit_exec_output: bool,
    last_exit_code: Option<i32>,
}

impl std::fmt::Debug for RuntimeContext {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RuntimeContext")
            .field("cwd", &self.cwd)
            .field("args", &self.args)
            .finish()
    }
}

impl RuntimeContext {
    pub fn new(cwd: PathBuf) -> Self {
        Self {
            cwd,
            args: Vec::new(),
            environment: std::env::vars().collect(),
            stdin: RuntimeInput::default(),
            stdout: RuntimeOutput::Stdout,
            stderr: RuntimeOutput::Stderr,
            resources: ResourceTable::new(),
            previous_value: None,
            structured_terminal: false,
            capture_mixed: false,
            mixed_capture: None,
            cancelled: false,
            requested_exit: None,
            inherit_exec_output: false,
            last_exit_code: None,
        }
    }

    /// Whether an `exec { }` statement's child stdout/stderr should be
    /// inherited (visible live) instead of captured into its returned
    /// `{ stdout, stderr }` fields. Off by default — `exec { }`'s designed
    /// behavior in the compiled runtime is to capture output as data. A
    /// task's native `run{}` block turns this on (see
    /// `set_inherit_exec_output`) to match the pre-Runtime-migration
    /// behavior task authors relied on: `exec` statements in a task write
    /// straight to the terminal, in declaration order against the rest of
    /// the block's commands.
    pub fn inherit_exec_output(&self) -> bool {
        self.inherit_exec_output
    }

    pub fn set_inherit_exec_output(&mut self, value: bool) {
        self.inherit_exec_output = value;
    }

    pub fn for_base_dir(base_dir: &Path) -> Self {
        let cwd = if base_dir.is_absolute() {
            base_dir.to_path_buf()
        } else {
            std::env::current_dir()
                .unwrap_or_else(|_| PathBuf::from("."))
                .join(base_dir)
        };
        Self::new(cwd)
    }

    /// Builds an independent execution context for a spawned async task: cwd,
    /// args, environment, and stdio are inherited (stdio via shared `Arc`s, so
    /// writes still land in the same place); `resources` starts empty because
    /// `Box<dyn Any + Send>` handles aren't `Clone` and sharing an open handle
    /// mutably across threads would be unsound — a spawned task owns its own
    /// resource lifecycle.
    pub(crate) fn spawn_child(&self) -> RuntimeContext {
        RuntimeContext {
            cwd: self.cwd.clone(),
            args: self.args.clone(),
            environment: self.environment.clone(),
            stdin: self.stdin.clone(),
            stdout: self.stdout.clone(),
            stderr: self.stderr.clone(),
            resources: ResourceTable::new(),
            previous_value: None,
            structured_terminal: self.structured_terminal,
            capture_mixed: false,
            mixed_capture: None,
            cancelled: false,
            requested_exit: None,
            inherit_exec_output: self.inherit_exec_output,
            last_exit_code: None,
        }
    }

    pub fn cwd(&self) -> &Path {
        &self.cwd
    }

    pub fn set_cwd(&mut self, cwd: PathBuf) {
        self.cwd = cwd;
    }

    pub fn resolve_path(&self, path: impl AsRef<Path>) -> PathBuf {
        let path = path.as_ref();
        if path.is_absolute() {
            path.to_path_buf()
        } else {
            self.cwd.join(path)
        }
    }

    pub fn args(&self) -> &[String] {
        &self.args
    }

    pub fn set_args(&mut self, args: Vec<String>) {
        self.args = args;
    }

    /// The exit code `$?` should report, when this context represents a
    /// continuation of a caller-tracked shell session (e.g. an interactive
    /// prompt's previous command) rather than a fresh compiled `Runtime`
    /// that tracks its own `shell_outcome` as it executes.
    pub fn last_exit_code(&self) -> Option<i32> {
        self.last_exit_code
    }

    pub fn set_last_exit_code(&mut self, code: i32) {
        self.last_exit_code = Some(code);
    }

    pub fn env_get(&self, key: &str) -> Option<&str> {
        self.environment.get(key).map(String::as_str)
    }

    pub fn env_contains(&self, key: &str) -> bool {
        self.environment.contains_key(key)
    }

    pub fn env_set(&mut self, key: impl Into<String>, value: impl Into<String>) {
        self.environment.insert(key.into(), value.into());
    }

    pub fn env_unset(&mut self, key: &str) -> Option<String> {
        self.environment.remove(key)
    }

    pub fn environment(&self) -> &HashMap<String, String> {
        &self.environment
    }

    /// Replaces the runtime-local environment without mutating the host
    /// process. Embedders such as Sparsh use this to keep `$NAME` expansion
    /// aligned with the shell session's exported environment.
    pub fn replace_environment(&mut self, entries: impl IntoIterator<Item = (String, String)>) {
        self.environment = entries.into_iter().collect();
    }

    pub fn environment_pairs(&self) -> Vec<(std::ffi::OsString, std::ffi::OsString)> {
        self.environment
            .iter()
            .map(|(key, value)| (key.as_str().into(), value.as_str().into()))
            .collect()
    }

    pub fn set_stdin(&mut self, input: RuntimeInput) {
        self.stdin = input;
    }

    pub fn set_stdout(&mut self, output: RuntimeOutput) {
        self.stdout = output;
    }

    pub fn set_stderr(&mut self, output: RuntimeOutput) {
        self.stderr = output;
    }

    pub fn read_stdin_remaining(&self) -> io::Result<Vec<u8>> {
        self.stdin.read_remaining()
    }

    pub fn read_stdin_line(&self) -> io::Result<Vec<u8>> {
        self.stdin.read_line()
    }

    pub fn write_stdout(&self, bytes: &[u8]) -> io::Result<()> {
        self.stdout.write_all(bytes)
    }

    pub fn write_stderr(&self, bytes: &[u8]) -> io::Result<()> {
        self.stderr.write_all(bytes)
    }

    pub fn stdout_is_terminal(&self) -> bool {
        self.stdout.is_terminal()
    }

    pub fn stderr_is_terminal(&self) -> bool {
        self.stderr.is_terminal()
    }

    pub fn resources(&self) -> &ResourceTable {
        &self.resources
    }

    pub fn resources_mut(&mut self) -> &mut ResourceTable {
        &mut self.resources
    }

    /// Sets the materialized value exposed to interactive code as `_`.
    /// Sparsh updates this before evaluating each interactive expression.
    pub fn set_previous_value(&mut self, value: Option<Value>) {
        self.previous_value = value;
    }

    /// Marks the output as an interactive terminal that renders structured
    /// results itself (see `MixedCapture`).
    pub fn set_structured_terminal(&mut self, enabled: bool) {
        self.structured_terminal = enabled;
    }

    pub(crate) fn structured_terminal(&self) -> bool {
        self.structured_terminal
    }

    pub(crate) fn set_capture_mixed(&mut self, enabled: bool) {
        self.capture_mixed = enabled;
    }

    pub(crate) fn capture_mixed(&self) -> bool {
        self.capture_mixed
    }

    pub(crate) fn set_mixed_capture(&mut self, capture: MixedCapture) {
        self.mixed_capture = Some(capture);
    }

    pub(crate) fn take_mixed_capture(&mut self) -> Option<MixedCapture> {
        self.mixed_capture.take()
    }

    pub fn previous_value(&self) -> Option<&Value> {
        self.previous_value.as_ref()
    }

    /// Stores a lazy structured stream in this runtime and returns the resource id
    /// carried by the corresponding Spar runtime value.
    pub fn insert_stream(&mut self, stream: StreamResource) -> ResourceId {
        self.resources.insert(stream)
    }

    /// Pulls one value from a stream resource. Completed and failed streams are
    /// removed immediately so one-shot producers do not leave stale runtime
    /// resources behind.
    pub fn stream_next(&mut self, id: ResourceId) -> Result<Option<Value>, SparError> {
        let result = {
            let stream = self
                .resources
                .get_mut::<StreamResource>(id)
                .ok_or_else(|| SparError::EvalError {
                    message: "stream handle is no longer valid".into(),
                    span: Span::dummy(),
                })?;
            stream.next()
        };

        match result {
            Ok(Some(value)) => Ok(Some(value)),
            Ok(None) => {
                let _ = self.resources.remove::<StreamResource>(id);
                Ok(None)
            }
            Err(error) => {
                let _ = self.resources.remove::<StreamResource>(id);
                Err(error)
            }
        }
    }

    /// Cancels and removes a live structured stream. Returns `false` when the
    /// handle is already completed, cancelled, failed, or otherwise invalid.
    pub fn cancel_stream(&mut self, id: ResourceId) -> bool {
        let Some(mut stream) = self.resources.remove::<StreamResource>(id) else {
            return false;
        };
        stream.cancel();
        true
    }

    pub fn request_exit(&mut self, code: i32) {
        self.requested_exit = Some(code);
    }

    pub fn requested_exit(&self) -> Option<i32> {
        self.requested_exit
    }

    pub fn cancel(&mut self) {
        self.cancelled = true;
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancelled
    }

    pub fn shutdown(&mut self) {
        self.cancelled = true;
        self.resources.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cwd_and_environment_are_context_local() {
        let root = std::env::temp_dir().join("spar_runtime_context_test");
        let mut first = RuntimeContext::new(root.join("one"));
        let second = RuntimeContext::new(root.join("two"));
        first.env_set("SPAR_CONTEXT_TEST", "one");
        assert_eq!(first.env_get("SPAR_CONTEXT_TEST"), Some("one"));
        assert_ne!(first.cwd(), second.cwd());
        assert_ne!(second.env_get("SPAR_CONTEXT_TEST"), Some("one"));
    }

    #[test]
    fn env_set_accepts_owned_strings() {
        let mut context = RuntimeContext::new(PathBuf::from("."));
        context.env_set(String::from("SPAR_OWNED_KEY"), String::from("owned-value"));
        assert_eq!(context.env_get("SPAR_OWNED_KEY"), Some("owned-value"));
    }

    #[test]
    fn buffered_input_reads_one_line_then_remaining_bytes() {
        let mut context = RuntimeContext::new(PathBuf::from("."));
        context.set_stdin(RuntimeInput::from_bytes(b"first\nsecond".to_vec()));

        assert_eq!(context.read_stdin_line().unwrap(), b"first\n");
        assert_eq!(context.read_stdin_remaining().unwrap(), b"second");
        assert!(context.read_stdin_remaining().unwrap().is_empty());
    }

    #[test]
    fn buffered_outputs_are_not_reported_as_terminals() {
        let buffer = Arc::new(Mutex::new(Vec::new()));
        let mut context = RuntimeContext::new(PathBuf::from("."));
        context.set_stdout(RuntimeOutput::Buffer(buffer.clone()));
        context.set_stderr(RuntimeOutput::Buffer(buffer));
        assert!(!context.stdout_is_terminal());
        assert!(!context.stderr_is_terminal());
    }

    #[test]
    fn buffer_output_is_capturable() {
        let buffer = Arc::new(Mutex::new(Vec::new()));
        let mut context = RuntimeContext::new(PathBuf::from("."));
        context.set_stdout(RuntimeOutput::Buffer(buffer.clone()));
        context.write_stdout(b"hello").unwrap();
        assert_eq!(&*buffer.lock().unwrap(), b"hello");
    }

    #[test]
    fn spawn_child_is_independent_but_shares_stdio() {
        let mut parent = RuntimeContext::new(PathBuf::from("/tmp"));
        parent.env_set("PARENT_ONLY", "1");
        let buffer = Arc::new(Mutex::new(Vec::new()));
        parent.set_stdout(RuntimeOutput::Buffer(buffer.clone()));

        let mut child = parent.spawn_child();
        child.env_set("CHILD_ONLY", "1");

        assert!(!parent.env_contains("CHILD_ONLY"));
        assert!(child.env_contains("PARENT_ONLY"));

        child.write_stdout(b"from child").unwrap();
        parent.write_stdout(b"from parent").unwrap();
        assert_eq!(&*buffer.lock().unwrap(), b"from childfrom parent");
    }
}
