use std::fs::{self, File};
use std::io::{BufRead, BufReader, Read, Write};
use std::path::Path;
use std::process::{Child, ChildStdout, Command, Stdio};

/// Hold a provider-compatible native lock in another process, rather than relying
/// on platform-specific same-process locking behavior.
pub struct HeldLock {
    child: Child,
    _output: BufReader<ChildStdout>,
}

impl HeldLock {
    pub fn new(path: &Path) -> Self {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "lock_holder::lock_holder_process", "--nocapture"])
            .env("AICS_TEST_HELD_LOCK", path)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let mut output = BufReader::new(child.stdout.take().unwrap());
        loop {
            let mut line = String::new();
            if output.read_line(&mut line).unwrap() == 0 {
                let status = child.wait().unwrap();
                panic!("lock holder exited before acquiring its lock: {status}");
            }
            if line.trim() == "locked" {
                break;
            }
        }
        Self {
            child,
            _output: output,
        }
    }
}

impl Drop for HeldLock {
    fn drop(&mut self) {
        if let Some(mut input) = self.child.stdin.take() {
            let _ = input.write_all(b"x");
        }
        let _ = self.child.wait();
    }
}

#[test]
fn lock_holder_process() {
    let Some(path) = std::env::var_os("AICS_TEST_HELD_LOCK") else {
        return;
    };
    let file = File::create(path).unwrap();
    file.lock().unwrap();
    println!("locked");
    std::io::stdout().flush().unwrap();
    let _ = std::io::stdin().read_exact(&mut [0]);
}
