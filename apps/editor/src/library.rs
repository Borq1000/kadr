//! Media library view-model: bins, search, asset cards, drag to timeline.

use crate::app::{App, AssetStatus, Confirm};
use crate::timeline_ui::{rows, AUDIO_H, VIDEO_H};
use crate::{AssetView, BinView};
use kadr_core::{AssetId, BinId, MediaKind};
use kadr_i18n::{duration, t, tf, tn};
use kadr_project::{Bin, TrackKind};
use kadr_timeline::InsertMode;
use slint::{ModelRc, VecModel};

pub struct LibDrag {
    pub asset: AssetId,
    pub start: (f64, f64),
    pub active: bool,
}

impl App {
    pub fn refresh_library(&mut self) {
        let ui = self.ui();
        let used: std::collections::HashSet<AssetId> =
            self.project.sequence().tracks.iter().flat_map(|t| t.clips.iter().map(|c| c.asset)).collect();
        let q = self.media_search.trim().to_lowercase();
        let views: Vec<AssetView> = self
            .project
            .assets
            .iter()
            .filter(|a| self.bin_filter.is_none() || a.bin == self.bin_filter)
            .filter(|a| q.is_empty() || a.name.to_lowercase().contains(&q))
            .map(|a| {
                let rt = self.assets_rt.get(&a.id);
                let (status, kind, progress) = match rt.map(|r| &r.status) {
                    Some(AssetStatus::Ready) => (t("media.status.ready"), 1, 1.0),
                    Some(AssetStatus::Working) => {
                        let p = rt.map_or(0.0, |r| r.progress);
                        (tf("media.status.analyzing", &[("pct", &format!("{:.0}", p * 100.0))]), 0, p)
                    }
                    Some(AssetStatus::Missing) => (t("media.status.offline"), 2, 0.0),
                    Some(AssetStatus::Error(e)) => (tf("media.status.error", &[("error", e)]), 2, 0.0),
                    Some(AssetStatus::Queued) | None => (t("media.status.queued"), 0, 0.0),
                };
                let info = &a.info;
                let codec = [
                    info.video.as_ref().filter(|_| info.kind != MediaKind::Audio).map(|v| v.codec.to_uppercase()),
                    info.audio.as_ref().map(|x| x.codec.to_uppercase()),
                ]
                .into_iter()
                .flatten()
                .collect::<Vec<_>>()
                .join(" / ");
                let vfr = info.video.as_ref().is_some_and(|v| v.variable_frame_rate);
                let thumb = rt.and_then(|r| r.thumb.clone());
                let audio = info.audio.as_ref().map(|x| {
                    let ch = match x.channels {
                        1 => t("media.mono"),
                        2 => t("media.stereo"),
                        n => tf("media.channels", &[("n", &n.to_string())]),
                    };
                    format!("{} kHz {ch}", x.sample_rate as f64 / 1000.0)
                });
                AssetView {
                    id: a.id.to_string().into(),
                    name: a.name.clone().into(),
                    kind: match a.kind() {
                        MediaKind::Video => 0,
                        MediaKind::Audio => 1,
                        MediaKind::Image => 2,
                    },
                    duration: if a.kind() == MediaKind::Image { t("media.still").into() } else { duration(a.duration().as_secs_f64()).into() },
                    resolution: info.resolution_label().into(),
                    fps: format!("{}{}", info.fps_label(), if vfr { " VFR" } else { "" }).into(),
                    codec: codec.into(),
                    audio: audio.unwrap_or_default().into(),
                    has_thumb: thumb.is_some(),
                    thumb: thumb.unwrap_or_default(),
                    status: status.into(),
                    status_kind: kind,
                    progress,
                    proxy: if a.kind() == MediaKind::Video && a.info.video.as_ref().is_some_and(|v| v.width > 1920) {
                        t("media.proxy_planned").into()
                    } else {
                        "".into()
                    },
                    selected: self.selected_asset == Some(a.id),
                    in_use: used.contains(&a.id),
                }
            })
            .collect();
        let total = self.project.assets.len();
        let mut bins = vec![BinView { id: "".into(), name: t("media.bin.all").into(), count: total as i32, selected: self.bin_filter.is_none(), renaming: false }];
        for b in &self.project.bins {
            let count = self.project.assets.iter().filter(|a| a.bin == Some(b.id)).count();
            bins.push(BinView {
                id: b.id.to_string().into(),
                name: b.name.clone().into(),
                count: count as i32,
                selected: self.bin_filter == Some(b.id),
                renaming: self.renaming_bin == Some(b.id),
            });
        }
        let dur: kadr_core::Time = self.project.assets.iter().map(|a| a.duration()).fold(kadr_core::Time::ZERO, |a, b| a + b);
        ui.set_media_summary(if total == 0 { "".into() } else { tn("media.summary", total as i64, &[("d", &duration(dur.as_secs_f64()))]).into() });
        ui.set_media_total(total as i32);
        ui.set_assets(ModelRc::new(VecModel::from(views)));
        ui.set_bins(ModelRc::new(VecModel::from(bins)));
    }

    pub fn media_search_changed(&mut self, s: &str) {
        self.media_search = s.to_string();
        self.refresh_library();
    }

    pub fn new_bin(&mut self) {
        let n = self.project.bins.len() + 1;
        let bin = Bin { id: BinId::new(), name: tf("media.bin.default_name", &[("n", &n.to_string())]), parent: None };
        self.bin_filter = Some(bin.id);
        self.renaming_bin = Some(bin.id);
        self.project.bins.push(bin);
        self.meta_dirty = true;
        self.refresh_library();
        self.refresh_status();
    }

    pub fn select_bin(&mut self, id: &str) {
        self.bin_filter = BinId::parse(id);
        self.refresh_library();
    }

    pub fn bin_action(&mut self, id: &str, action: &str) {
        let Some(bid) = BinId::parse(id) else { return };
        match action {
            "rename" => {
                self.renaming_bin = Some(bid);
                self.refresh_library();
            }
            "delete" => {
                let n = self.project.assets.iter().filter(|a| a.bin == Some(bid)).count();
                if n == 0 {
                    self.delete_bin_now(bid);
                } else {
                    let name = self.project.bins.iter().find(|b| b.id == bid).map(|b| b.name.clone()).unwrap_or_default();
                    self.ask(
                        &t("dlg.delete_bin.title"),
                        &tn("dlg.delete_bin.body", n as i64, &[("bin", &name)]),
                        &t("dlg.delete_bin.ok"),
                        "",
                        true,
                        Confirm::DeleteBin(bid),
                    );
                }
            }
            _ => {}
        }
    }

    /// Deleting a bin never deletes media: its items move back to "All".
    pub fn delete_bin_now(&mut self, id: BinId) {
        for a in self.project.assets.iter_mut().filter(|a| a.bin == Some(id)) {
            a.bin = None;
        }
        self.project.bins.retain(|b| b.id != id);
        if self.bin_filter == Some(id) {
            self.bin_filter = None;
        }
        self.meta_dirty = true;
        self.refresh_library();
        self.refresh_status();
    }

    pub fn rename_bin(&mut self, id: &str, name: &str) {
        self.renaming_bin = None;
        let name = name.trim();
        if let (Some(bid), false) = (BinId::parse(id), name.is_empty()) {
            if let Some(b) = self.project.bins.iter_mut().find(|b| b.id == bid) {
                if b.name != name {
                    b.name = name.chars().take(40).collect();
                    self.meta_dirty = true;
                }
            }
        }
        self.refresh_library();
        self.refresh_status();
    }

    pub fn asset_pressed(&mut self, id: &str, x: f32, y: f32) {
        let Some(id) = AssetId::parse(id) else { return };
        self.selected_asset = Some(id);
        self.library_drag = Some(LibDrag { asset: id, start: (x as f64, y as f64), active: false });
        self.refresh_library();
    }

    pub fn asset_moved(&mut self, _id: &str, x: f32, y: f32) {
        let (x, y) = (x as f64, y as f64);
        let Some(d) = self.library_drag.as_mut() else { return };
        if !d.active && ((x - d.start.0).abs() > 5.0 || (y - d.start.1).abs() > 5.0) {
            d.active = true;
        }
        if !d.active {
            return;
        }
        let asset = d.asset;
        let ui = self.ui();
        let name = self.project.asset(asset).map(|a| a.name.clone()).unwrap_or_default();
        ui.set_drag_active(true);
        ui.set_drag_x(x as f32);
        ui.set_drag_y(y as f32);
        match self.drop_target(x, y) {
            Some((t, row)) => {
                let seq = self.project.sequence();
                let t = seq.frame_rate.snap(t);
                let rs = rows(seq, self.tl.scroll_y);
                let dur = self.project.asset(asset).map(|a| a.duration().as_secs_f64()).unwrap_or(1.0);
                let r = rs.get(row).copied();
                let x0 = (t.as_secs_f64() - self.tl.scroll) * self.tl.pps;
                let (gy, gh) = r.map_or((0.0, VIDEO_H), |r| (r.y, if r.kind == TrackKind::Video { VIDEO_H } else { AUDIO_H }));
                self.tl.ghost = Some((x0, gy, dur * self.tl.pps, gh));
                ui.set_drag_label(format!("{name}  →  {}", kadr_core::Timecode::from_time(t, seq.frame_rate)).into());
            }
            None => {
                self.tl.ghost = None;
                ui.set_drag_label(tf("media.drag_hint", &[("name", &name)]).into());
            }
        }
        self.refresh_timeline();
    }

    pub fn asset_released(&mut self, _id: &str, x: f32, y: f32) {
        let ui = self.ui();
        ui.set_drag_active(false);
        self.tl.ghost = None;
        let Some(d) = self.library_drag.take() else { return };
        if d.active {
            if let Some((t, row)) = self.drop_target(x as f64, y as f64) {
                self.place_asset(d.asset, t, InsertMode::Overwrite, Some(row));
            }
        }
        self.refresh_timeline();
    }

    pub fn asset_activated(&mut self, id: &str) {
        if let Some(id) = AssetId::parse(id) {
            let end = self.project.sequence().duration();
            self.place_asset(id, end, InsertMode::Insert, None);
        }
    }

    pub fn insert_selected_asset(&mut self, mode: InsertMode) {
        match self.selected_asset {
            Some(a) => self.place_asset(a, self.playhead, mode, None),
            None => self.toast(t("toast.select_media_first")),
        }
    }

    pub fn asset_action(&mut self, id: &str, action: &str) {
        let Some(aid) = AssetId::parse(id) else { return };
        match action {
            "insert" => self.place_asset(aid, self.playhead, InsertMode::Insert, None),
            "overwrite" => self.place_asset(aid, self.playhead, InsertMode::Overwrite, None),
            "append" => {
                let end = self.project.sequence().duration();
                self.place_asset(aid, end, InsertMode::Insert, None);
            }
            "to-bin" => match self.bin_filter {
                Some(bin) => {
                    if let Some(a) = self.project.asset_mut(aid) {
                        a.bin = Some(bin);
                        self.meta_dirty = true;
                    }
                    self.refresh_library();
                }
                None => self.toast(t("toast.select_bin_first")),
            },
            "reanalyze" => {
                if let Some(a) = self.project.asset(aid) {
                    if let Ok(c) = self.cache.for_source(&a.path) {
                        let _ = std::fs::remove_file(c.dir().join("meta.json"));
                    }
                }
                self.process_asset(aid);
                self.refresh_library();
            }
            "reveal" => {
                if let Some(a) = self.project.asset(aid) {
                    crate::util::reveal(&a.path);
                }
            }
            "remove" => {
                let used = self.project.sequence().tracks.iter().flat_map(|t| t.clips.iter()).filter(|c| c.asset == aid).count();
                if used > 0 {
                    self.toast_warn(tn("toast.media_in_use", used as i64, &[]));
                } else {
                    let name = self.project.asset(aid).map(|a| a.name.clone()).unwrap_or_default();
                    self.ask(&t("dlg.remove_media.title"), &tf("dlg.remove_media.body", &[("name", &name)]), &t("dlg.remove_media.ok"), "", true, Confirm::RemoveAsset(aid));
                }
            }
            _ => {}
        }
    }

    pub fn remove_asset_now(&mut self, aid: AssetId) {
        self.project.assets.retain(|a| a.id != aid);
        self.project.analysis.retain(|a| a.asset != aid);
        if let Some(rt) = self.assets_rt.remove(&aid) {
            if let Some(j) = rt.job {
                self.jobs.cancel(j);
            }
        }
        if self.selected_asset == Some(aid) {
            self.selected_asset = None;
        }
        self.meta_dirty = true;
        self.refresh_library();
        self.refresh_status();
    }
}
