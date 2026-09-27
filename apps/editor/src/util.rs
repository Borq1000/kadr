//! Small formatting / OS helpers shared by views.

use kadr_i18n::{t, tf, tn};
use slint::Color;

pub fn rgb(hex: &str) -> Color {
    let h = hex.trim_start_matches('#');
    let v = u32::from_str_radix(h, 16).unwrap_or(0xe5a50a);
    Color::from_rgb_u8((v >> 16) as u8, (v >> 8) as u8, v as u8)
}

/// Ruler label for `t` seconds, precision matching the major tick step.
pub fn fmt_ruler(t: f64, step: f64, fps: f64) -> String {
    let total = t.max(0.0);
    let h = (total / 3600.0).floor() as u64;
    let m = ((total / 60.0).floor() as u64) % 60;
    let s = (total.floor() as u64) % 60;
    if step < 1.0 {
        let f = ((total - total.floor()) * fps).round() as u64;
        format!("{m:02}:{s:02}:{f:02}")
    } else if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m:02}:{s:02}")
    }
}

/// Opens Explorer with the file selected.
pub fn reveal(path: &std::path::Path) {
    let _ = std::process::Command::new("explorer").arg(format!("/select,{}", path.display())).spawn();
}

pub fn open_folder(path: &std::path::Path) {
    let _ = std::fs::create_dir_all(path);
    let _ = std::process::Command::new("explorer").arg(path).spawn();
}

/// Opens a file with its default application.
pub fn open_file(path: &std::path::Path) {
    let _ = std::process::Command::new("cmd").args(["/C", "start", ""]).arg(path).spawn();
}

pub fn money(v: f64) -> String {
    if v == 0.0 {
        "$0.00".into()
    } else if v < 0.01 {
        format!("${v:.4}")
    } else {
        format!("${v:.2}")
    }
}

pub fn money_range(lo: f64, hi: f64) -> String {
    if (hi - lo).abs() < 1e-9 { money(hi) } else { format!("{}–{}", money(lo), money(hi)) }
}

pub fn fmt_tokens(n: u64) -> String {
    let v = if n >= 1000 { format!("{:.1}k", n as f64 / 1000.0) } else { n.to_string() };
    tf("ai.tokens", &[("n", &v)])
}

pub fn fmt_bytes(b: u64) -> String {
    let b = b as f64;
    if b >= 1e9 {
        tf("unit.gb", &[("v", &format!("{:.1}", b / 1e9))])
    } else if b >= 1e6 {
        tf("unit.mb", &[("v", &format!("{:.0}", b / 1e6))])
    } else {
        tf("unit.kb", &[("v", &format!("{:.0}", b / 1e3))])
    }
}

/// Local timezone offset in minutes (east positive).
fn local_offset_min() -> i64 {
    #[cfg(windows)]
    {
        #[repr(C)]
        struct SystemTime([u16; 8]);
        #[repr(C)]
        struct Tzi {
            bias: i32,
            standard_name: [u16; 32],
            standard_date: SystemTime,
            standard_bias: i32,
            daylight_name: [u16; 32],
            daylight_date: SystemTime,
            daylight_bias: i32,
        }
        #[link(name = "kernel32")]
        unsafe extern "system" {
            fn GetTimeZoneInformation(tzi: *mut Tzi) -> u32;
        }
        let mut tzi: Tzi = unsafe { std::mem::zeroed() };
        // SAFETY: tzi is a correctly sized TIME_ZONE_INFORMATION.
        let r = unsafe { GetTimeZoneInformation(&mut tzi) };
        let dst = if r == 2 { tzi.daylight_bias } else { tzi.standard_bias };
        -(tzi.bias + dst) as i64
    }
    #[cfg(not(windows))]
    {
        0
    }
}

/// `HH:MM` in local time.
pub fn clock_time(ms: i64) -> String {
    let local_min = ms.div_euclid(60_000) + local_offset_min();
    let m = local_min.rem_euclid(24 * 60);
    format!("{:02}:{:02}", m / 60, m % 60)
}

/// Puts `rows` into the `VecModel` behind `current` in place: changed rows
/// are replaced one by one, extra rows appended, surplus rows removed. A
/// Repeater then keeps its element instances (and handles held by UI
/// automation stay valid) instead of rebuilding every element on each
/// refresh. `Err(rows)` when `current` is not a `VecModel<T>` yet.
pub fn update_rows<T: Clone + PartialEq + 'static>(current: &slint::ModelRc<T>, rows: Vec<T>) -> Result<(), Vec<T>> {
    use slint::Model;
    let Some(model) = current.as_any().downcast_ref::<slint::VecModel<T>>() else { return Err(rows) };
    let keep = model.row_count().min(rows.len());
    while model.row_count() > rows.len() {
        model.remove(model.row_count() - 1);
    }
    for (i, row) in rows.into_iter().enumerate() {
        if i >= keep {
            model.push(row);
        } else if model.row_data(i).as_ref() != Some(&row) {
            model.set_row_data(i, row);
        }
    }
    Ok(())
}

/// [`update_rows`] for a model property: `set` installs a new model only
/// when the property has none to update. Positional: use it for lists
/// without identity (ruler ticks, lanes). Lists of things with an id use
/// [`sync_rows_by_key`].
pub fn sync_rows<T: Clone + PartialEq + 'static>(current: slint::ModelRc<T>, rows: Vec<T>, set: impl FnOnce(slint::ModelRc<T>)) {
    if let Err(rows) = update_rows(&current, rows) {
        set(slint::ModelRc::new(slint::VecModel::from(rows)));
    }
}

/// A nested list (a model inside a row) for a row being rebuilt: the row's
/// previous inner model, updated in place, when it has one. `ModelRc`
/// compares by pointer, so a fresh inner model would make every row look
/// changed and rebuild its nested elements on each refresh.
pub fn reuse_rows<T: Clone + PartialEq + 'static>(previous: Option<slint::ModelRc<T>>, rows: Vec<T>) -> slint::ModelRc<T> {
    let rows = match previous {
        Some(m) => match update_rows(&m, rows) {
            Ok(()) => return m,
            Err(rows) => rows,
        },
        None => rows,
    };
    slint::ModelRc::new(slint::VecModel::from(rows))
}

/// One step of turning a model's rows into new ones.
#[derive(Debug, PartialEq)]
pub enum RowOp<T> {
    Remove(usize),
    Insert(usize, T),
    Set(usize, T),
}

/// The steps that turn `old` into `new` keeping every surviving row (by
/// `key`) in its own element: deleting item k removes row k (its handle
/// dies) instead of shifting later items into earlier elements. Unchanged
/// rows get no step.
pub fn row_ops<T: PartialEq, K: PartialEq>(old: &[T], new: Vec<T>, key: impl Fn(&T) -> K) -> Vec<RowOp<T>> {
    let new_len = new.len();
    let new_keys: Vec<K> = new.iter().map(&key).collect();
    let mut ops = vec![];
    // Current rows: (key, index into `old`, or None once replaced/inserted).
    let mut cur: Vec<(K, Option<usize>)> = old.iter().enumerate().map(|(i, r)| (key(r), Some(i))).collect();
    for i in (0..cur.len()).rev() {
        if !new_keys.contains(&cur[i].0) {
            cur.remove(i);
            ops.push(RowOp::Remove(i));
        }
    }
    for (i, (row, k)) in new.into_iter().zip(new_keys).enumerate() {
        loop {
            if i < cur.len() && cur[i].0 == k {
                if cur[i].1.is_none_or(|o| old[o] != row) {
                    cur[i].1 = None;
                    ops.push(RowOp::Set(i, row));
                }
                break;
            }
            if cur[i.min(cur.len())..].iter().any(|(ck, _)| *ck == k) {
                // Out of order: drop the row in the way, it comes back later.
                cur.remove(i);
                ops.push(RowOp::Remove(i));
                continue;
            }
            cur.insert(i, (k, None));
            ops.push(RowOp::Insert(i, row));
            break;
        }
    }
    // Duplicate keys can leave surplus rows behind.
    for i in (new_len..cur.len()).rev() {
        ops.push(RowOp::Remove(i));
    }
    ops
}

/// [`update_rows`] keyed by `key`: rows keep their elements by identity.
pub fn update_rows_by_key<T: Clone + PartialEq + 'static, K: PartialEq>(current: &slint::ModelRc<T>, rows: Vec<T>, key: impl Fn(&T) -> K) -> Result<(), Vec<T>> {
    use slint::Model;
    let Some(model) = current.as_any().downcast_ref::<slint::VecModel<T>>() else { return Err(rows) };
    let old: Vec<T> = model.iter().collect();
    for op in row_ops(&old, rows, key) {
        match op {
            RowOp::Remove(i) => drop(model.remove(i)),
            RowOp::Insert(i, r) => model.insert(i, r),
            RowOp::Set(i, r) => model.set_row_data(i, r),
        }
    }
    Ok(())
}

/// [`sync_rows`] keyed by `key` (an id): a handle to a deleted item's
/// element dies instead of silently pointing at its neighbour.
pub fn sync_rows_by_key<T: Clone + PartialEq + 'static, K: PartialEq>(current: slint::ModelRc<T>, rows: Vec<T>, key: impl Fn(&T) -> K, set: impl FnOnce(slint::ModelRc<T>)) {
    if let Err(rows) = update_rows_by_key(&current, rows, key) {
        set(slint::ModelRc::new(slint::VecModel::from(rows)));
    }
}

/// "just now", "5 min ago", "yesterday", "12.09.2026".
pub fn relative_time(ms: i64) -> String {
    let now = kadr_project::now_ms();
    let d = (now - ms).max(0) / 1000;
    if d < 60 {
        t("time.just_now")
    } else if d < 3600 {
        tn("time.min_ago", d / 60, &[])
    } else if d < 86_400 {
        tn("time.hours_ago", d / 3600, &[])
    } else if d < 2 * 86_400 {
        t("time.yesterday")
    } else if d < 30 * 86_400 {
        tn("time.days_ago", d / 86_400, &[])
    } else {
        let days = (ms / 60_000 + local_offset_min()).div_euclid(1440);
        let (y, m, dd) = civil(days);
        format!("{dd:02}.{m:02}.{y}")
    }
}

/// Days since epoch → (year, month, day) (Howard Hinnant).
pub fn civil(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (yoe + era * 400 + if m <= 2 { 1 } else { 0 }, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ruler_labels() {
        assert_eq!(fmt_ruler(65.0, 5.0, 25.0), "01:05");
        assert_eq!(fmt_ruler(3725.0, 60.0, 25.0), "1:02:05");
        assert_eq!(fmt_ruler(1.4, 0.2, 25.0), "00:01:10");
        assert_eq!(money(0.00012), "$0.0001");
        assert_eq!(money_range(0.01, 0.05), "$0.01–$0.05");
    }

    #[test]
    fn rows_are_updated_in_the_same_model() {
        use slint::{Model, ModelRc, VecModel};
        let model = ModelRc::new(VecModel::from(vec![1, 2, 3]));
        let before = model.as_any().downcast_ref::<VecModel<i32>>().unwrap() as *const _;
        for rows in [vec![1, 5, 3, 4, 9], vec![7], vec![], vec![2, 2]] {
            update_rows(&model, rows.clone()).unwrap();
            assert_eq!(model.iter().collect::<Vec<_>>(), rows);
        }
        let after = model.as_any().downcast_ref::<VecModel<i32>>().unwrap() as *const _;
        assert_eq!(before, after, "the model is kept, so a Repeater keeps its element instances");
    }

    #[test]
    fn a_property_without_a_vec_model_gets_a_new_one() {
        use slint::{Model, ModelRc};
        assert_eq!(update_rows(&ModelRc::<i32>::default(), vec![4]), Err(vec![4]));
        let mut set = None;
        sync_rows(ModelRc::<i32>::default(), vec![4, 5], |m| set = Some(m));
        assert_eq!(set.unwrap().iter().collect::<Vec<_>>(), [4, 5]);
    }

    type Row = (&'static str, i32);

    fn ops(old: &[Row], new: &[Row]) -> Vec<RowOp<Row>> {
        row_ops(old, new.to_vec(), |r| r.0)
    }

    #[test]
    fn deleting_a_row_removes_that_row_not_the_last_one() {
        let old = [("a", 1), ("b", 1), ("c", 1)];
        assert_eq!(ops(&old, &[("a", 1), ("c", 1)]), [RowOp::Remove(1)]);
    }

    #[test]
    fn inserting_changing_and_keeping_rows() {
        let old = [("a", 1), ("c", 1)];
        assert_eq!(ops(&old, &[("a", 1), ("b", 1), ("c", 2)]), [RowOp::Insert(1, ("b", 1)), RowOp::Set(2, ("c", 2))]);
        assert_eq!(ops(&old, &old), [], "unchanged rows are left alone");
        assert_eq!(ops(&[], &[("x", 0)]), [RowOp::Insert(0, ("x", 0))]);
    }

    #[test]
    fn reordered_rows_end_in_the_new_order() {
        use slint::{Model, ModelRc, VecModel};
        for (old, new) in [
            (vec![("a", 1), ("b", 1), ("c", 1)], vec![("c", 1), ("a", 1), ("b", 1)]),
            (vec![("a", 1), ("b", 1), ("c", 1)], vec![("b", 2), ("d", 0), ("a", 1)]),
            (vec![("a", 1), ("a", 2)], vec![("a", 3)]),
        ] {
            let model = ModelRc::new(VecModel::from(old));
            update_rows_by_key(&model, new.clone(), |r| r.0).unwrap();
            assert_eq!(model.iter().collect::<Vec<_>>(), new);
        }
    }

    #[test]
    fn nested_lists_reuse_the_previous_model() {
        use slint::{Model, ModelRc, VecModel};
        let prev = ModelRc::new(VecModel::from(vec![1, 2]));
        let same = reuse_rows(Some(prev.clone()), vec![1, 2]);
        assert!(same == prev, "same model, so the outer row compares equal and is left alone");
        let changed = reuse_rows(Some(prev.clone()), vec![3]);
        assert!(changed == prev);
        assert_eq!(prev.iter().collect::<Vec<_>>(), [3]);
        let fresh = reuse_rows(None, vec![7]);
        assert_eq!(fresh.iter().collect::<Vec<_>>(), [7]);
    }

    #[test]
    fn civil_dates() {
        assert_eq!(civil(0), (1970, 1, 1));
        assert_eq!(civil(20_722), (2026, 9, 26));
    }
}
