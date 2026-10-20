//! The full-screen views driven through a real pseudo-terminal: the terminal must come back (main
//! screen, cursor shown) however they end, and nothing may be printed over them (SPEC §2.1, §2.5).
#![cfg(unix)]

mod common;

use std::fs::File;
use std::io::{Read, Write};
use std::os::fd::{FromRawFd, OwnedFd};
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use common::Env;

const BIN: &str = env!("CARGO_BIN_EXE_agentdiet");
const LEAVE_ALT_SCREEN: &[u8] = b"\x1b[?1049l";
const SHOW_CURSOR: &[u8] = b"\x1b[?25h";

struct Session {
    child: Child,
    master: File,
    out: Arc<Mutex<Vec<u8>>>,
}

fn spawn(env: &Env, args: &[&str]) -> Session {
    let (mut master, mut slave) = (0, 0);
    let mut size = libc::winsize {
        ws_row: 24,
        ws_col: 80,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    // SAFETY: openpty writes two fds into the out-pointers; the window size is a valid struct.
    let rc = unsafe {
        libc::openpty(
            &mut master,
            &mut slave,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &mut size,
        )
    };
    assert_eq!(rc, 0, "openpty failed");
    // SAFETY: both fds were just returned by openpty and are owned by nothing else.
    let (master, slave) = unsafe { (OwnedFd::from_raw_fd(master), OwnedFd::from_raw_fd(slave)) };
    let stdio = |fd: &OwnedFd| Stdio::from(fd.try_clone().unwrap());
    let mut cmd = Command::new(BIN);
    cmd.args(args)
        .env("HOME", &env.paths.home)
        .env("TERM", "xterm-256color")
        .env("LANG", "en_US.UTF-8")
        .env_remove("LC_ALL")
        .env_remove("NO_COLOR")
        .current_dir(&env.paths.project_root)
        .stdin(stdio(&slave))
        .stdout(stdio(&slave))
        .stderr(stdio(&slave));
    // SAFETY: only async-signal-safe calls between fork and exec: a new session, and the pty
    // (stdin) as its controlling terminal, as a real terminal emulator would set up.
    unsafe {
        cmd.pre_exec(|| {
            libc::setsid();
            libc::ioctl(0, libc::TIOCSCTTY as _, 0);
            Ok(())
        });
    }
    let child = cmd.spawn().unwrap();
    drop(slave);
    let master = File::from(master);
    let out = Arc::new(Mutex::new(Vec::new()));
    let mut reader = master.try_clone().unwrap();
    let sink = Arc::clone(&out);
    std::thread::spawn(move || {
        let mut buf = [0u8; 4096];
        // Ends with EIO once the child has exited and the slave is closed.
        while let Ok(n) = reader.read(&mut buf) {
            if n == 0 {
                break;
            }
            sink.lock().unwrap().extend_from_slice(&buf[..n]);
        }
    });
    Session { child, master, out }
}

impl Session {
    fn output(&self) -> Vec<u8> {
        self.out.lock().unwrap().clone()
    }

    fn wait_for(&self, needle: &str) {
        let start = Instant::now();
        while !String::from_utf8_lossy(&self.output()).contains(needle) {
            assert!(
                start.elapsed() < Duration::from_secs(10),
                "never saw {needle:?}; got {:?}",
                String::from_utf8_lossy(&self.output())
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    fn keys(&mut self, keys: &str) {
        for key in keys.bytes() {
            // EIO once the app has quit: the remaining keys have no one to go to.
            if self.master.write_all(&[key]).is_err() {
                return;
            }
            std::thread::sleep(Duration::from_millis(150));
        }
    }

    fn finish(mut self) -> (std::process::ExitStatus, Vec<u8>) {
        let start = Instant::now();
        let status = loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                break status;
            }
            assert!(
                start.elapsed() < Duration::from_secs(10),
                "did not exit; output {:?}",
                String::from_utf8_lossy(&self.output())
            );
            std::thread::sleep(Duration::from_millis(20));
        };
        std::thread::sleep(Duration::from_millis(100)); // let the reader drain
        (status, self.output())
    }
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle)
}

fn assert_restored(out: &[u8]) {
    assert!(contains(out, LEAVE_ALT_SCREEN), "main screen not restored");
    assert!(contains(out, SHOW_CURSOR), "cursor not shown again");
}

#[test]
fn stop_signals_restore_the_terminal_then_end_the_process_by_that_signal() {
    for (args, title) in [(&[][..], "Assistants"), (&["top"][..], "Metrics")] {
        for sig in [libc::SIGTERM, libc::SIGHUP, libc::SIGINT] {
            let env = Env::empty();
            let session = spawn(&env, args);
            session.wait_for(title);
            // SAFETY: sending a signal to our own child.
            unsafe { libc::kill(session.child.id() as i32, sig) };
            let (status, out) = session.finish();
            assert_eq!(status.signal(), Some(sig), "{args:?}: ended by {status:?}");
            assert_restored(&out);
        }
    }
}

#[test]
fn quitting_with_keys_restores_the_terminal() {
    let env = Env::empty();
    for keys in [
        "q", "\x03", "tqq", "t\x03", "?\x1bq", "d\x1bq", "m\x1bq", "a\x1bq",
    ] {
        let mut session = spawn(&env, &[]);
        session.wait_for("Assistants");
        session.keys(keys);
        let (status, out) = session.finish();
        assert!(status.success(), "{keys:?}: {status:?}");
        assert_restored(&out);
    }
}

#[test]
fn a_corrupt_state_file_never_prints_over_the_screen() {
    let env = Env::empty();
    env.write(env.paths.state_file(), "{ not json");
    let mut session = spawn(&env, &[]);
    session.wait_for("Assistants");
    session.keys("pmq"); // pause (rewrites state.json), open the mode picker, back, quit
    session.keys("q");
    let (status, out) = session.finish();
    assert!(status.success());
    let text = String::from_utf8_lossy(&out);
    assert!(
        !text.contains("ignoring corrupt"),
        "stderr landed on the screen"
    );
    assert_restored(&out);

    // The same file under `top`: shown in its footer, still never printed.
    env.write(env.paths.state_file(), "{ not json");
    let mut session = spawn(&env, &["top"]);
    session.wait_for("state.json");
    session.keys("pq");
    let (status, out) = session.finish();
    assert!(status.success());
    let text = String::from_utf8_lossy(&out);
    assert!(!text.contains("agentdiet: ignoring corrupt"), "{text}");
    assert_restored(&out);
}
