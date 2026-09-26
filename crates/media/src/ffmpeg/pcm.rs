use super::progress::run_with_progress;
use super::FfmpegCli;
use crate::{Progress, Result};
use kadr_core::{CancelToken, Time};
use std::path::Path;

/// Writes to `<out>.part` and renames on success, so a cancelled or crashed
/// extraction never leaves a truncated file that looks complete.
#[allow(clippy::too_many_arguments)]
pub(super) fn extract_pcm(
    ff: &FfmpegCli,
    path: &Path,
    out: &Path,
    rate: u32,
    channels: u32,
    duration: Time,
    progress: Progress,
    cancel: &CancelToken,
) -> Result<()> {
    let part = out.with_extension("part");
    let mut cmd = ff.ffmpeg_cmd();
    cmd.arg("-y").arg("-i").arg(path).args([
        "-vn",
        "-sn",
        "-ac",
        &channels.to_string(),
        "-ar",
        &rate.to_string(),
        "-c:a",
        "pcm_s16le",
        "-f",
        "s16le",
    ]);
    cmd.arg(&part);
    let r = run_with_progress(ff, cmd, duration, progress, cancel);
    match r {
        Ok(()) => {
            std::fs::rename(&part, out)?;
            Ok(())
        }
        Err(e) => {
            let _ = std::fs::remove_file(&part);
            Err(e)
        }
    }
}
