use std::{
    io::{self, Write},
    mem,
    process::{Command, ExitStatus, Stdio},
};

use anyhow::{bail, Result};

// Use external python program to render a pretty graph of the game's eval
// Modified from Tiltak which was itself modified from Wilem
pub fn generate_graph(ptn: &[u8]) -> Result<Vec<u8>> {
    let python = if let Ok(path) = std::env::var("PYTHON_BIN") {
        path
    } else {
        bail!("Path to python binary not supplied in environment")
    };
    let mut child = Command::new(python)
        .arg("graph.py")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()?;

    let mut stdin = child
        .stdin
        .take()
        .ok_or(io::Error::other("Failed to open stdin"))?;

    stdin.write_all(ptn)?;
    mem::drop(stdin); // Close stdin file handle

    let output = child.wait_with_output()?;
    if !ExitStatus::success(&output.status) {
        return Err(anyhow::anyhow!(format!(
            "Got exit status {}",
            output.status
        )));
    } else {
        Ok(output.stdout)
    }
}
