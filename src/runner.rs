//! Every external command (herdr, git, gh, ssh, scp, rsync, sh) goes through `Runner`.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};

#[derive(Debug, Clone, PartialEq)]
pub struct Cmd {
    pub program: String,
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
    pub env_remove: Vec<String>,
    pub cwd: Option<PathBuf>,
    pub stdin: Option<String>,
    pub timeout: Duration,
    /// Spawn in its own process group and kill the whole group on timeout.
    pub own_group: bool,
}

impl Cmd {
    pub fn new(program: impl Into<String>, timeout: Duration) -> Self {
        Cmd {
            program: program.into(),
            args: Vec::new(),
            env: Vec::new(),
            env_remove: Vec::new(),
            cwd: None,
            stdin: None,
            timeout,
            own_group: false,
        }
    }

    pub fn arg(mut self, arg: impl Into<String>) -> Self {
        self.args.push(arg.into());
        self
    }

    pub fn args<I, S>(mut self, args: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.args.extend(args.into_iter().map(Into::into));
        self
    }

    pub fn env(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.env.push((key.into(), value.into()));
        self
    }

    pub fn env_remove(mut self, key: impl Into<String>) -> Self {
        self.env_remove.push(key.into());
        self
    }

    pub fn cwd(mut self, cwd: impl Into<PathBuf>) -> Self {
        self.cwd = Some(cwd.into());
        self
    }

    pub fn stdin(mut self, text: impl Into<String>) -> Self {
        self.stdin = Some(text.into());
        self
    }

    pub fn own_group(mut self) -> Self {
        self.own_group = true;
        self
    }

    /// The command as one line; the scripted fake matches on it.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn display(&self) -> String {
        let mut line = self.program.clone();
        for arg in &self.args {
            line.push(' ');
            line.push_str(arg);
        }
        line
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Output {
    /// `None` when the process was killed (timeout or signal).
    pub code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    pub timed_out: bool,
}

impl Output {
    pub fn success(&self) -> bool {
        self.code == Some(0) && !self.timed_out
    }

    /// stderr when it has text, else stdout, trimmed; for error messages.
    pub fn error_text(&self) -> String {
        if self.timed_out {
            return "timed out".to_string();
        }
        let text = if self.stderr.trim().is_empty() {
            self.stdout.trim()
        } else {
            self.stderr.trim()
        };
        text.to_string()
    }
}

pub trait Runner {
    /// `Err` means the command could not be spawned at all (for example the
    /// program is missing). A non-zero exit or a timeout is an `Ok(Output)`.
    fn run(&self, cmd: &Cmd) -> Result<Output>;

    /// One JSON line to a herdr socket, one line back. The single exception to
    /// "talk to herdr through its CLI" (client decision during the build):
    /// herdr 0.9.1 has no CLI command for `agent.view.set` / `agent.view.clear`.
    fn socket_request(&self, socket: &Path, line: &str, timeout: Duration) -> Result<String>;

    /// Runs `cmd` on this process's terminal (an agent `open` starts in its own
    /// pane) and waits for it; `cmd.timeout` and `cmd.stdin` are ignored.
    /// While it runs, `poll` is called about twice a second until it returns
    /// true. Returns the exit code, `None` when a signal ended it.
    fn run_foreground(&self, cmd: &Cmd, poll: &mut dyn FnMut() -> bool) -> Result<Option<i32>>;
}

pub struct RealRunner;

const POLL: Duration = Duration::from_millis(20);

impl Runner for RealRunner {
    fn run(&self, cmd: &Cmd) -> Result<Output> {
        let mut command = Command::new(&cmd.program);
        command.args(&cmd.args);
        for key in &cmd.env_remove {
            command.env_remove(key);
        }
        for (key, value) in &cmd.env {
            command.env(key, value);
        }
        if let Some(cwd) = &cmd.cwd {
            command.current_dir(cwd);
        }
        command
            .stdin(if cmd.stdin.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        #[cfg(unix)]
        if cmd.own_group {
            use std::os::unix::process::CommandExt;
            command.process_group(0);
        }
        #[cfg(windows)]
        if cmd.own_group {
            use std::os::windows::process::CommandExt;
            command.creation_flags(windows_sys::Win32::System::Threading::CREATE_NEW_PROCESS_GROUP);
        }

        let mut child = command
            .spawn()
            .with_context(|| format!("could not run `{}`", cmd.program))?;

        // Readers and the writer run on their own threads so a full pipe in
        // either direction cannot deadlock against the deadline loop below.
        let stdin_thread = child.stdin.take().zip(cmd.stdin.clone()).map(|(mut pipe, text)| {
            std::thread::spawn(move || {
                let _ = pipe.write_all(text.as_bytes());
            })
        });
        let stdout_thread = child.stdout.take().map(read_all);
        let stderr_thread = child.stderr.take().map(read_all);

        let deadline = Instant::now() + cmd.timeout;
        let mut timed_out = false;
        let status = loop {
            if let Some(status) = child.try_wait()? {
                break Some(status);
            }
            if Instant::now() >= deadline {
                timed_out = true;
                kill(&mut child, cmd.own_group);
                break child.wait().ok();
            }
            std::thread::sleep(POLL);
        };

        if let Some(thread) = stdin_thread {
            let _ = thread.join();
        }
        let stdout = stdout_thread.map(join_text).unwrap_or_default();
        let stderr = stderr_thread.map(join_text).unwrap_or_default();

        Ok(Output {
            code: if timed_out {
                None
            } else {
                status.and_then(|s| s.code())
            },
            stdout,
            stderr,
            timed_out,
        })
    }

    fn socket_request(&self, socket: &Path, line: &str, timeout: Duration) -> Result<String> {
        socket_round_trip(socket, line, timeout)
    }

    fn run_foreground(&self, cmd: &Cmd, poll: &mut dyn FnMut() -> bool) -> Result<Option<i32>> {
        let mut command = Command::new(&cmd.program);
        command.args(&cmd.args);
        for key in &cmd.env_remove {
            command.env_remove(key);
        }
        for (key, value) in &cmd.env {
            command.env(key, value);
        }
        if let Some(cwd) = &cmd.cwd {
            // This process leads the pane's foreground group, and Herdr reports
            // the leader's directory as the pane's `foreground_cwd`: it moves too.
            std::env::set_current_dir(cwd).with_context(|| format!("could not enter {}", cwd.display()))?;
            command.current_dir(cwd);
        }
        let mut child = command
            .spawn()
            .with_context(|| format!("could not run `{}`", cmd.program))?;
        // Ctrl-C and Ctrl-\ reach the whole foreground group: they are the
        // agent's to handle, and this process must outlive it so the shell
        // does not take the terminal back from a running agent.
        let _ignored = IgnoreInterrupts::new();
        let mut polling = true;
        let status = loop {
            if let Some(status) = child.try_wait()? {
                break status;
            }
            if polling {
                polling = !poll();
            }
            std::thread::sleep(Duration::from_millis(500));
        };
        Ok(status.code())
    }
}

#[cfg(unix)]
unsafe extern "C" {
    fn signal(signum: i32, handler: usize) -> usize;
}

#[cfg(unix)]
const SIGINT: i32 = 2;
#[cfg(unix)]
const SIGQUIT: i32 = 3;
#[cfg(unix)]
const SIG_IGN: usize = 1;

#[cfg(unix)]
/// SIGINT and SIGQUIT ignored in this process (set after the child's exec, so
/// the child keeps the default), restored on drop.
struct IgnoreInterrupts(usize, usize);

#[cfg(unix)]
impl IgnoreInterrupts {
    fn new() -> Self {
        // SAFETY: plain signal(2) calls with the ignore disposition.
        unsafe { IgnoreInterrupts(signal(SIGINT, SIG_IGN), signal(SIGQUIT, SIG_IGN)) }
    }
}

#[cfg(unix)]
impl Drop for IgnoreInterrupts {
    fn drop(&mut self) {
        // SAFETY: restores the dispositions `new` returned.
        unsafe {
            signal(SIGINT, self.0);
            signal(SIGQUIT, self.1);
        }
    }
}

#[cfg(windows)]
struct IgnoreInterrupts(bool);

#[cfg(windows)]
unsafe extern "system" fn ignore_console_interrupt(event: u32) -> windows_sys::core::BOOL {
    use windows_sys::Win32::System::Console::{CTRL_BREAK_EVENT, CTRL_C_EVENT};
    i32::from(matches!(event, CTRL_C_EVENT | CTRL_BREAK_EVENT))
}

#[cfg(windows)]
impl IgnoreInterrupts {
    fn new() -> Self {
        // A handler is local to this process: the already-spawned agent still
        // receives Ctrl-C/Ctrl-Break. Detached processes have no console.
        Self(unsafe {
            windows_sys::Win32::System::Console::SetConsoleCtrlHandler(Some(ignore_console_interrupt), 1) != 0
        })
    }
}

#[cfg(windows)]
impl Drop for IgnoreInterrupts {
    fn drop(&mut self) {
        if self.0 {
            unsafe {
                windows_sys::Win32::System::Console::SetConsoleCtrlHandler(Some(ignore_console_interrupt), 0);
            }
        }
    }
}

#[cfg(unix)]
fn socket_round_trip(socket: &Path, line: &str, timeout: Duration) -> Result<String> {
    use std::io::{BufRead, BufReader};
    use std::os::unix::net::UnixStream;
    let mut stream = UnixStream::connect(socket)
        .with_context(|| format!("could not connect to {}", socket.display()))?;
    stream.set_read_timeout(Some(timeout))?;
    stream.set_write_timeout(Some(timeout))?;
    stream.write_all(line.as_bytes())?;
    stream.write_all(b"\n")?;
    let mut reply = String::new();
    BufReader::new(stream).read_line(&mut reply)?;
    Ok(reply)
}

#[cfg(windows)]
fn socket_round_trip(socket: &Path, line: &str, timeout: Duration) -> Result<String> {
    use std::io::{BufRead, BufReader};
    let mut pipe = WindowsPipe::connect(socket, timeout)
        .with_context(|| format!("could not connect to {}", socket.display()))?;
    pipe.write_all(line.as_bytes())?;
    pipe.write_all(b"\n")?;
    let mut reply = String::new();
    BufReader::new(pipe).read_line(&mut reply)?;
    Ok(reply)
}

/// Herdr uses the exact socket-path string as its named-pipe suffix. The disk
/// file is only a liveness marker; do not canonicalize it or read it as an address.
#[cfg(windows)]
struct WindowsPipe {
    file: std::fs::File,
    event: std::os::windows::io::OwnedHandle,
    deadline: Instant,
}

#[cfg(windows)]
impl WindowsPipe {
    fn connect(socket: &Path, timeout: Duration) -> std::io::Result<Self> {
        use std::os::windows::fs::OpenOptionsExt;
        use windows_sys::Win32::Storage::FileSystem::FILE_FLAG_OVERLAPPED;
        let deadline = Instant::now() + timeout;
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(FILE_FLAG_OVERLAPPED)
            .open(format!(r"\\.\pipe\{}", socket.display()))?;
        Self::from_file(file, deadline)
    }

    fn from_file(file: std::fs::File, deadline: Instant) -> std::io::Result<Self> {
        use std::os::windows::io::FromRawHandle;
        // One manual-reset event is reused by the sequential reads and writes.
        let event = unsafe {
            windows_sys::Win32::System::Threading::CreateEventW(std::ptr::null(), 1, 0, std::ptr::null())
        };
        if event.is_null() {
            return Err(std::io::Error::last_os_error());
        }
        Ok(Self { file, event: unsafe { std::os::windows::io::OwnedHandle::from_raw_handle(event) }, deadline })
    }

    fn transfer(&mut self, buffer: *mut u8, len: usize, write: bool) -> std::io::Result<usize> {
        use std::os::windows::io::AsRawHandle;
        use windows_sys::Win32::Foundation::{ERROR_BROKEN_PIPE, ERROR_IO_PENDING, WAIT_OBJECT_0, WAIT_TIMEOUT};
        use windows_sys::Win32::Storage::FileSystem::{ReadFile, WriteFile};
        use windows_sys::Win32::System::IO::{CancelIoEx, GetOverlappedResult, OVERLAPPED};
        use windows_sys::Win32::System::Threading::{ResetEvent, WaitForSingleObject};
        if len == 0 {
            return Ok(0);
        }
        let timeout = || std::io::Error::new(std::io::ErrorKind::TimedOut, "Herdr pipe request timed out");
        if Instant::now() >= self.deadline {
            return Err(timeout());
        }
        let handle = self.file.as_raw_handle();
        let event = self.event.as_raw_handle();
        let mut operation = OVERLAPPED { hEvent: event, ..Default::default() };
        let mut count = 0;
        let len = len.min(u32::MAX as usize) as u32;
        // SAFETY: both handles are owned here. The buffer and OVERLAPPED remain
        // alive until completion, including the cancellation path below.
        unsafe {
            if ResetEvent(event) == 0 {
                return Err(std::io::Error::last_os_error());
            }
            let completed = if write {
                WriteFile(handle, buffer, len, &mut count, &mut operation)
            } else {
                ReadFile(handle, buffer, len, &mut count, &mut operation)
            };
            if completed == 0 {
                let error = std::io::Error::last_os_error();
                if !write && error.raw_os_error() == Some(ERROR_BROKEN_PIPE as i32) {
                    return Ok(0);
                }
                if error.raw_os_error() != Some(ERROR_IO_PENDING as i32) {
                    return Err(error);
                }
                let millis = self.deadline.saturating_duration_since(Instant::now()).as_millis()
                    .min((u32::MAX - 1) as u128) as u32;
                let wait = WaitForSingleObject(event, millis);
                if wait != WAIT_OBJECT_0 {
                    let error = if wait == WAIT_TIMEOUT { timeout() } else { std::io::Error::last_os_error() };
                    CancelIoEx(handle, &operation);
                    // Cancellation is asynchronous. Reap it before releasing
                    // the operation or its caller's buffer.
                    GetOverlappedResult(handle, &operation, &mut count, 1);
                    return Err(error);
                }
            }
            if GetOverlappedResult(handle, &operation, &mut count, 0) == 0 {
                let error = std::io::Error::last_os_error();
                if !write && error.raw_os_error() == Some(ERROR_BROKEN_PIPE as i32) {
                    return Ok(0);
                }
                return Err(error);
            }
        }
        Ok(count as usize)
    }
}

#[cfg(windows)]
impl Read for WindowsPipe {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        self.transfer(buffer.as_mut_ptr(), buffer.len(), false)
    }
}

#[cfg(windows)]
impl Write for WindowsPipe {
    fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
        self.transfer(buffer.as_ptr().cast_mut(), buffer.len(), true)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        // FlushFileBuffers waits for the peer to read and has no deadline.
        // WriteFile already submits all bytes; no flush is needed for framing.
        Ok(())
    }
}

fn read_all<R: Read + Send + 'static>(mut pipe: R) -> std::thread::JoinHandle<Vec<u8>> {
    std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = pipe.read_to_end(&mut buf);
        buf
    })
}

fn join_text(thread: std::thread::JoinHandle<Vec<u8>>) -> String {
    String::from_utf8_lossy(&thread.join().unwrap_or_default()).into_owned()
}

fn kill(child: &mut std::process::Child, _own_group: bool) {
    #[cfg(unix)]
    if _own_group {
        // The child is its group's leader, so its pid is the pgid. Grandchildren
        // hold the pipes open; killing only the child would leave readers hanging.
        let _ = Command::new("/bin/kill")
            .args(["-TERM", "--", &format!("-{}", child.id())])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        std::thread::sleep(Duration::from_millis(200));
        let _ = Command::new("/bin/kill")
            .args(["-KILL", "--", &format!("-{}", child.id())])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
    #[cfg(windows)]
    {
        // Windows descendants do not need a process group to be addressed.
        // Terminate only this owned child's tree, never processes by name.
        let taskkill = std::env::var_os("SystemRoot")
            .map(|root| PathBuf::from(root).join("System32").join("taskkill.exe"))
            .unwrap_or_else(|| PathBuf::from("taskkill.exe"));
        let _ = Command::new(taskkill)
            .args(["/F", "/T", "/PID", &child.id().to_string()])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
    let _ = child.kill();
}

#[cfg(test)]
pub mod fake {
    use super::*;
    use std::cell::RefCell;

    type Matcher = Box<dyn Fn(&Cmd) -> bool>;

    /// A scripted runner: the first rule whose matcher accepts the command
    /// answers it. Every command is recorded, matched or not.
    #[derive(Default)]
    pub struct FakeRunner {
        rules: RefCell<Vec<(Matcher, Box<dyn Fn(&Cmd) -> Result<Output>>)>>,
        pub calls: RefCell<Vec<Cmd>>,
        /// (socket, request line) of every socket request.
        pub socket_requests: RefCell<Vec<(PathBuf, String)>>,
    }

    impl FakeRunner {
        pub fn new() -> Self {
            Self::default()
        }

        /// Answer commands whose display line contains `needle`.
        pub fn on(&self, needle: &str, output: Output) -> &Self {
            let needle = needle.to_string();
            self.rules.borrow_mut().push((
                Box::new(move |cmd| cmd.display().contains(&needle)),
                Box::new(move |_| Ok(output.clone())),
            ));
            self
        }

        pub fn on_fn(
            &self,
            matcher: impl Fn(&Cmd) -> bool + 'static,
            answer: impl Fn(&Cmd) -> Result<Output> + 'static,
        ) -> &Self {
            self.rules
                .borrow_mut()
                .push((Box::new(matcher), Box::new(answer)));
            self
        }

        pub fn count(&self, needle: &str) -> usize {
            self.calls
                .borrow()
                .iter()
                .filter(|cmd| cmd.display().contains(needle))
                .count()
        }
    }

    pub fn ok(stdout: &str) -> Output {
        Output {
            code: Some(0),
            stdout: stdout.to_string(),
            ..Output::default()
        }
    }

    pub fn fail(code: i32, stderr: &str) -> Output {
        Output {
            code: Some(code),
            stderr: stderr.to_string(),
            ..Output::default()
        }
    }

    pub fn timeout() -> Output {
        Output {
            timed_out: true,
            ..Output::default()
        }
    }

    impl Runner for FakeRunner {
        fn run(&self, cmd: &Cmd) -> Result<Output> {
            self.calls.borrow_mut().push(cmd.clone());
            for (matcher, answer) in self.rules.borrow().iter() {
                if matcher(cmd) {
                    return answer(cmd);
                }
            }
            anyhow::bail!("FakeRunner: no rule for `{}`", cmd.display())
        }

        fn socket_request(&self, socket: &Path, line: &str, _timeout: Duration) -> Result<String> {
            self.socket_requests.borrow_mut().push((socket.to_path_buf(), line.to_string()));
            Ok(r#"{"id":"hp","result":{"type":"agent_view","active":true}}"#.to_string())
        }

        /// The first matching rule answers (it may change what later calls
        /// see, as a starting agent does), then `poll` runs once and the
        /// command exits with the rule's code.
        fn run_foreground(&self, cmd: &Cmd, poll: &mut dyn FnMut() -> bool) -> Result<Option<i32>> {
            self.calls.borrow_mut().push(cmd.clone());
            let out = {
                let rules = self.rules.borrow();
                let Some((_, answer)) = rules.iter().find(|(matcher, _)| matcher(cmd)) else {
                    anyhow::bail!("FakeRunner: no rule for `{}`", cmd.display())
                };
                answer(cmd)?
            };
            poll();
            Ok(out.code)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn captures_output_and_exit_code() {
        #[cfg(unix)]
        let cmd = Cmd::new("sh", Duration::from_secs(5)).args(["-c", "echo hi; echo err >&2; exit 3"]);
        #[cfg(windows)]
        let cmd = powershell("[Console]::Out.Write(\"hi`n\"); [Console]::Error.Write(\"err`n\"); exit 3", Duration::from_secs(10));
        let out = RealRunner.run(&cmd).unwrap();
        assert_eq!(out.code, Some(3));
        assert_eq!(out.stdout, "hi\n");
        assert_eq!(out.stderr, "err\n");
        assert!(!out.success());
    }

    #[test]
    fn passes_stdin() {
        #[cfg(unix)]
        let cmd = Cmd::new("cat", Duration::from_secs(5));
        #[cfg(windows)]
        let cmd = powershell("[Console]::Out.Write([Console]::In.ReadToEnd())", Duration::from_secs(10));
        let out = RealRunner.run(&cmd.stdin("hello")).unwrap();
        assert_eq!(out.stdout, "hello");
    }

    #[test]
    fn missing_program_is_an_error() {
        assert!(
            RealRunner
                .run(&Cmd::new("hp-no-such-program", Duration::from_secs(1)))
                .is_err()
        );
    }

    #[test]
    fn times_out_a_chatty_child() {
        // Fill the output pipe past its buffer while the deadline stays active.
        #[cfg(unix)]
        let cmd = Cmd::new("yes", Duration::from_millis(300));
        #[cfg(windows)]
        let cmd = powershell("$chunk = 'x' * 8192; while ($true) { [Console]::Out.Write($chunk) }", Duration::from_secs(3));
        let start = Instant::now();
        let out = RealRunner.run(&cmd).unwrap();
        assert!(out.timed_out);
        assert!(!out.success());
        assert!(start.elapsed() < Duration::from_secs(8));
        assert!(out.stdout.len() > 65_536);
    }

    #[cfg(unix)]
    #[test]
    fn group_kill_reaches_grandchildren() {
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("survived");
        let script = format!("(sleep 2; touch '{}') & wait", marker.display());
        let start = Instant::now();
        let out = RealRunner
            .run(
                &Cmd::new("sh", Duration::from_millis(300))
                    .args(["-c", &script])
                    .own_group(),
            )
            .unwrap();
        assert!(out.timed_out);
        assert!(start.elapsed() < Duration::from_secs(2));
        std::thread::sleep(Duration::from_millis(2300));
        assert!(!marker.exists(), "grandchild outlived the group kill");
    }

    #[cfg(windows)]
    fn powershell(script: &str, timeout: Duration) -> Cmd {
        Cmd::new("powershell.exe", timeout).args(["-NoLogo", "-NoProfile", "-NonInteractive", "-Command", script])
    }

    #[cfg(windows)]
    #[test]
    fn group_kill_reaches_grandchildren() {
        use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
        use windows_sys::Win32::Foundation::{FILETIME, HANDLE, WAIT_OBJECT_0, WAIT_TIMEOUT};
        use windows_sys::Win32::System::Threading::{
            GetCurrentProcess, GetProcessTimes, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
            PROCESS_SYNCHRONIZE, PROCESS_TERMINATE, TerminateProcess, WaitForSingleObject,
        };

        const FIXTURE_DIR: &str = "HERDR_PROJECTS_RUNNER_TREE_FIXTURE_DIR";
        const FIXTURE_LEAF: &str = "HERDR_PROJECTS_RUNNER_TREE_FIXTURE_LEAF";
        const FIXTURE_ARGS: [&str; 4] = [
            "--exact", "runner::tests::group_kill_reaches_grandchildren",
            "--nocapture", "--test-threads=1",
        ];

        fn creation_time(handle: HANDLE) -> u64 {
            let mut created = FILETIME::default();
            let mut exited = FILETIME::default();
            let mut kernel = FILETIME::default();
            let mut user = FILETIME::default();
            assert_ne!(unsafe { GetProcessTimes(handle, &mut created, &mut exited, &mut kernel, &mut user) }, 0,
                "could not identify fixture process: {}", std::io::Error::last_os_error());
            (u64::from(created.dwHighDateTime) << 32) | u64::from(created.dwLowDateTime)
        }

        // Reuse the already-loaded native test binary, not two cold PowerShells.
        if let Some(dir) = std::env::var_os(FIXTURE_DIR) {
            let dir = PathBuf::from(dir);
            if std::env::var_os(FIXTURE_LEAF).is_some() {
                let starting = dir.join("starting");
                let identity = format!("{} {}", std::process::id(), creation_time(unsafe { GetCurrentProcess() }));
                std::fs::write(&starting, identity).unwrap();
                std::fs::rename(starting, dir.join("ready")).unwrap();
                // Bound an orphan's lifetime even if the test itself panics.
                std::thread::sleep(Duration::from_secs(20));
            } else {
                // Inherited output pipes make a surviving leaf block Runner's
                // readers until the leaf exits.
                Command::new(std::env::current_exe().unwrap())
                    .args(FIXTURE_ARGS)
                    .env(FIXTURE_LEAF, "1")
                    .stdin(Stdio::null())
                    .spawn().unwrap().wait().unwrap();
            }
            return;
        }

        struct Descendant(OwnedHandle);
        impl Drop for Descendant {
            fn drop(&mut self) {
                let handle = self.0.as_raw_handle();
                unsafe {
                    if WaitForSingleObject(handle, 0) != WAIT_OBJECT_0 {
                        // Cleanup only this retained process object, never a
                        // name or a PID that Windows could have reused.
                        if TerminateProcess(handle, 1) == 0 {
                            eprintln!("fixture cleanup failed: {}", std::io::Error::last_os_error());
                        } else if WaitForSingleObject(handle, 5000) != WAIT_OBJECT_0 {
                            eprintln!("fixture cleanup did not finish within 5 seconds");
                        }
                    }
                }
            }
        }

        let dir = tempfile::tempdir().unwrap();
        let ready = dir.path().join("ready");
        let cmd = Cmd::new(
            std::env::current_exe().unwrap().to_str().unwrap(),
            Duration::from_secs(3),
        )
            .args(FIXTURE_ARGS)
            .env(FIXTURE_DIR, dir.path().to_str().unwrap())
            .env_remove(FIXTURE_LEAF)
            .own_group();
        let start = Instant::now();
        let deadline = start + Duration::from_secs(8);
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::scope(|scope| {
            scope.spawn(|| { let _ = tx.send(RealRunner.run(&cmd)); });
            while !ready.exists() {
                if let Ok(out) = rx.try_recv() {
                    panic!("native descendant was not ready before Runner returned: {out:?}");
                }
                assert!(Instant::now() < deadline, "native descendant readiness timed out");
                std::thread::sleep(POLL);
            }
            let identity = std::fs::read_to_string(&ready).unwrap();
            let (pid, created) = identity.split_once(' ').unwrap();
            let pid = pid.parse::<u32>().unwrap();
            let created = created.parse::<u64>().unwrap();
            let handle = unsafe {
                OpenProcess(PROCESS_SYNCHRONIZE | PROCESS_TERMINATE | PROCESS_QUERY_LIMITED_INFORMATION, 0, pid)
            };
            assert!(!handle.is_null(), "could not open descendant {pid}: {}", std::io::Error::last_os_error());
            let handle = unsafe { OwnedHandle::from_raw_handle(handle) };
            // Validate identity before allowing cleanup to terminate the handle.
            assert_eq!(creation_time(handle.as_raw_handle()), created, "descendant PID {pid} was reused");
            let descendant = Descendant(handle);
            assert_eq!(unsafe { WaitForSingleObject(descendant.0.as_raw_handle(), 0) }, WAIT_TIMEOUT,
                "descendant {pid} was not alive before the timeout");
            let out = rx.recv_timeout(deadline.saturating_duration_since(Instant::now()))
                .expect("Runner did not close descendant-held pipes within 8 seconds").unwrap();
            assert!(out.timed_out);
            assert!(!out.success());
            assert!(start.elapsed() < Duration::from_secs(8));
            assert_eq!(unsafe { WaitForSingleObject(descendant.0.as_raw_handle(), 0) }, WAIT_OBJECT_0,
                "grandchild {pid} outlived the tree kill");
        });
    }

    #[test]
    fn timeout_closes_unconsumed_stdin() {
        #[cfg(unix)]
        let cmd = Cmd::new("sh", Duration::from_millis(300)).args(["-c", "sleep 10"]).own_group();
        #[cfg(windows)]
        let cmd = powershell("Start-Sleep -Seconds 10", Duration::from_millis(300)).own_group();
        let start = Instant::now();
        let out = RealRunner.run(&cmd.stdin("x".repeat(1_048_576))).unwrap();
        assert!(out.timed_out);
        assert!(start.elapsed() < Duration::from_secs(5));
    }

    #[cfg(windows)]
    fn pipe_server(handler: impl FnOnce(&mut WindowsPipe) + Send + 'static) -> (tempfile::TempDir, PathBuf, std::thread::JoinHandle<()>) {
        use std::os::windows::{ffi::OsStrExt, io::{AsRawHandle, FromRawHandle}};
        use windows_sys::Win32::Foundation::{ERROR_IO_PENDING, ERROR_PIPE_CONNECTED, INVALID_HANDLE_VALUE, WAIT_OBJECT_0};
        use windows_sys::Win32::Storage::FileSystem::{FILE_FLAG_OVERLAPPED, PIPE_ACCESS_DUPLEX};
        use windows_sys::Win32::System::IO::{CancelIoEx, GetOverlappedResult, OVERLAPPED};
        use windows_sys::Win32::System::Pipes::{ConnectNamedPipe, CreateNamedPipeW, PIPE_TYPE_BYTE, PIPE_WAIT};
        use windows_sys::Win32::System::Threading::WaitForSingleObject;
        let dir = tempfile::tempdir().unwrap();
        let socket = crate::paths::canonicalize(dir.path()).unwrap().join("herdr.sock");
        // The marker is not an address and must never be parsed by the client.
        std::fs::write(&socket, format!("{}:123456789", std::process::id())).unwrap();
        let name: Vec<u16> = std::ffi::OsStr::new(&format!(r"\\.\pipe\{}", socket.display()))
            .encode_wide().chain(Some(0)).collect();
        let handle = unsafe {
            CreateNamedPipeW(name.as_ptr(), PIPE_ACCESS_DUPLEX | FILE_FLAG_OVERLAPPED,
                PIPE_TYPE_BYTE | PIPE_WAIT, 1, 4096, 4096, 0, std::ptr::null())
        };
        assert_ne!(handle, INVALID_HANDLE_VALUE);
        let file = unsafe { std::fs::File::from_raw_handle(handle) };
        let mut pipe = WindowsPipe::from_file(file, Instant::now() + Duration::from_secs(5)).unwrap();
        let server = std::thread::spawn(move || {
            let mut operation = OVERLAPPED { hEvent: pipe.event.as_raw_handle(), ..Default::default() };
            let mut count = 0;
            unsafe {
                if ConnectNamedPipe(pipe.file.as_raw_handle(), &mut operation) == 0 {
                    let error = std::io::Error::last_os_error().raw_os_error();
                    if error == Some(ERROR_IO_PENDING as i32) {
                        let wait = WaitForSingleObject(pipe.event.as_raw_handle(), 5000);
                        if wait != WAIT_OBJECT_0 {
                            CancelIoEx(pipe.file.as_raw_handle(), &operation);
                            GetOverlappedResult(pipe.file.as_raw_handle(), &operation, &mut count, 1);
                        }
                        assert_eq!(wait, WAIT_OBJECT_0, "test pipe connection timed out");
                        assert_ne!(GetOverlappedResult(pipe.file.as_raw_handle(), &operation, &mut count, 0), 0);
                    } else {
                        assert_eq!(error, Some(ERROR_PIPE_CONNECTED as i32));
                    }
                }
            }
            handler(&mut pipe);
        });
        (dir, socket, server)
    }

    #[cfg(windows)]
    #[test]
    fn named_pipe_round_trip_uses_host_path_and_json_lines() {
        use std::io::{BufRead, BufReader};
        let (_dir, socket, server) = pipe_server(|pipe| {
            let mut request = String::new();
            BufReader::new(&mut *pipe).read_line(&mut request).unwrap();
            assert_eq!(request, "{\"id\":\"hp\",\"method\":\"ping\"}\n");
            pipe.write_all(b"{\"id\":\"hp\",\"result\":").unwrap();
            std::thread::sleep(Duration::from_millis(10));
            pipe.write_all(b"\"pong\"}\n").unwrap();
            // Keep the server handle alive until the client consumes its reply
            // and disconnects; closing a pipe can discard its unread bytes.
            let _ = pipe.read(&mut [0u8; 1]);
        });
        let reply = socket_round_trip(&socket, r#"{"id":"hp","method":"ping"}"#, Duration::from_secs(3)).unwrap();
        assert_eq!(reply, "{\"id\":\"hp\",\"result\":\"pong\"}\n");
        server.join().unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn named_pipe_deadline_cancels_partial_reply_and_blocked_write() {
        use std::io::{BufRead, BufReader};
        let (_dir, socket, server) = pipe_server(|pipe| {
            let mut request = String::new();
            BufReader::new(&mut *pipe).read_line(&mut request).unwrap();
            pipe.write_all(b"{\"result\":").unwrap();
            std::thread::sleep(Duration::from_millis(300));
        });
        let start = Instant::now();
        let error = socket_round_trip(&socket, "{}", Duration::from_millis(100)).unwrap_err();
        assert_eq!(error.downcast_ref::<std::io::Error>().unwrap().kind(), std::io::ErrorKind::TimedOut);
        assert!(start.elapsed() < Duration::from_secs(1));
        server.join().unwrap();

        let (_dir, socket, server) = pipe_server(|_| std::thread::sleep(Duration::from_millis(300)));
        let start = Instant::now();
        let error = socket_round_trip(&socket, &"x".repeat(1_048_576), Duration::from_millis(100)).unwrap_err();
        assert_eq!(error.downcast_ref::<std::io::Error>().unwrap().kind(), std::io::ErrorKind::TimedOut);
        assert!(start.elapsed() < Duration::from_secs(1));
        server.join().unwrap();
    }
}
