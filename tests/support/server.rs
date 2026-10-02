//! Local fixture child lifetime and startup handshake.
#![allow(dead_code)]
use std::{
    io::{BufRead, BufReader},
    path::Path,
    process::{Child, Command, Stdio},
};
pub struct Server(pub Child);
impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
impl Server {
    pub fn start(mut command: Command) -> (Self, u16) {
        let mut server = Self(
            command
                .stdout(Stdio::piped())
                .stderr(Stdio::inherit())
                .spawn()
                .unwrap(),
        );
        let mut line = String::new();
        BufReader::new(server.0.stdout.take().unwrap())
            .read_line(&mut line)
            .unwrap();
        let port = line.trim().parse().expect("fixture server startup port");
        (server, port)
    }
    pub fn script(server_path: &str, script: &Path, requests: &Path) -> (Self, u16) {
        let mut command = Command::new("python3");
        command
            .arg("-B")
            .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join(server_path))
            .arg("--script")
            .arg(script)
            .arg("--requests")
            .arg(requests);
        Self::start(command)
    }
}
