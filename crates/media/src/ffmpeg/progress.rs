//! Runs FFmpeg with `-progress pipe:1`, reporting fraction done and honouring
//! cancellation by killing the process.

use super::FfmpegCli;
use crate::{MediaError, Progress, Result};
use kadr_core::{CancelToken, Time};
use std::io::{BufRead, BufReader, Read};
use std::process::{Command, Stdio};

pub(crate) fn run_with_progress(
    _ff: &FfmpegCli,
    mut cmd: Command,
    total: Time,
    progress: Progress,
    cancel: &CancelToken,
) -> Result<()> {
    cmd.args(["-progress", "pipe:1", "-nostats"]);
    tracing::debug!(?cmd, "ffmpeg job");
    let mut child = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|source| MediaError::Spawn { tool: "ffmpeg".into(), source })?;

    // Drain stderr on a thread so a chatty FFmpeg can't deadlock on a full pipe.
    let mut stderr = child.stderr.take().expect("piped");
    let err_thread = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = stderr.read_to_string(&mut s);
        s
    });

    let stdout = child.stdout.take().expect("piped");
    let total_us = total.as_micros().max(1) as f64;
    for line in BufReader::new(stdout).lines() {
        if cancel.is_cancelled() {
            let _ = child.kill();
            let _ = child.wait();
            return Err(MediaError::Cancelled);
        }
        let Ok(line) = line else { break };
        if let Some(v) = line.strip_prefix("out_time_us=").or_else(|| line.strip_prefix("out_time_ms=")) {
            // Both keys are microseconds (out_time_ms is a historical misnomer).
            if let Ok(us) = v.trim().parse::<i64>() {
                progress((us as f64 / total_us).clamp(0.0, 1.0) as f32);
            }
        }
    }
    let status = child.wait()?;
    let stderr = err_thread.join().unwrap_or_default();
    if cancel.is_cancelled() {
        return Err(MediaError::Cancelled);
    }
    if !status.success() {
        tracing::warn!(%status, stderr = %stderr.trim(), "ffmpeg job failed");
        return Err(MediaError::ToolFailed { tool: "ffmpeg".into(), status: status.to_string(), stderr: stderr.trim().chars().take(2000).collect() });
    }
    progress(1.0);
    Ok(())
}
