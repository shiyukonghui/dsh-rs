//! 布局三函数（core.js 40–93 行逐段移植：columnsForWidth / layoutGrid /
//! layoutMeasured）。测试断言逐条移植自 core.test.mjs（D-184/D-209 契约）。

use serde_json::Value;

pub const GRID_COL: i64 = 260;
pub const GRID_ROW: i64 = 100;
pub const GRID_GAP: i64 = 10;

/// 列数 = floor((宽+gap)/(格宽+gap))，窄屏保底 1。
pub fn columns_for_width(width_px: f64) -> usize {
    let c = ((width_px + GRID_GAP as f64) / (GRID_COL + GRID_GAP) as f64).floor();
    (c as i64).max(1) as usize
}

#[derive(Debug, Clone, PartialEq)]
pub struct Placement {
    pub key: String,
    pub col: usize,
    pub row: i64,
    pub w: usize,
    pub h: i64,
}

/// 瀑布流 first-fit（D-184）：w=min(w,C)；卡顶=跨列当前高最大值；平手最左。
pub fn layout_grid(cards: &[Value], columns: usize) -> (Vec<Placement>, i64) {
    let c_n = columns.max(1);
    let mut heights = vec![0i64; c_n];
    let mut out: Vec<Placement> = Vec::new();
    for c in cards {
        let key = c.get("key").and_then(Value::as_str).unwrap_or_default().to_string();
        let raw_w = c.get("w").and_then(Value::as_i64).unwrap_or(1);
        let h = c.get("h").and_then(Value::as_i64).unwrap_or(1);
        let w = raw_w.max(1).min(c_n as i64) as usize;
        let mut best_col = 0usize;
        let mut best_top = i64::MAX;
        for s in 0..=(c_n - w) {
            let top = heights[s..s + w].iter().copied().max().unwrap_or(0);
            if top < best_top {
                best_top = top;
                best_col = s;
            }
        }
        for cell in &mut heights[best_col..best_col + w] {
            *cell = best_top + h;
        }
        out.push(Placement { key, col: best_col, row: best_top, w, h });
    }
    let total = heights.iter().copied().max().unwrap_or(0);
    (out, total)
}

#[derive(Debug, Clone, PartialEq)]
pub struct MeasuredPlacement {
    pub key: String,
    pub col: usize,
    pub y_px: i64,
    pub w: usize,
    pub h_px: i64,
}

/// D-209 实测装箱：真实 hPx 入位，矩形互不重叠。
pub fn layout_measured(cards: &[Value], columns: usize) -> (Vec<MeasuredPlacement>, i64) {
    layout_measured_avoid(cards, columns, &[])
}

/// D-215 避障实测装箱：obstacles=(x,y,w,h) 像素矩形（钉卡区）。
/// 候选位下沉直到与一切障碍无交集——钉卡怎么放，装箱卡都绕着走。
pub fn layout_measured_avoid(
    cards: &[Value],
    columns: usize,
    obstacles: &[(f64, f64, f64, f64)],
) -> (Vec<MeasuredPlacement>, i64) {
    let c_n = columns.max(1);
    let mut col_y = vec![0i64; c_n];
    let mut out: Vec<MeasuredPlacement> = Vec::new();
    for c in cards {
        let key = match c.get("key").and_then(Value::as_str) {
            Some(k) => k.to_string(),
            None => continue, // 无 key 条目跳过（junk 守卫）
        };
        let raw_w = c.get("w").and_then(Value::as_i64).unwrap_or(1);
        let w = raw_w.max(1).min(c_n as i64) as usize;
        let h_px = c.get("hPx").and_then(Value::as_i64).unwrap_or(1).max(1);
        let mut best = 0usize;
        let mut best_y = i64::MAX;
        for s in 0..=(c_n - w) {
            let mut y = col_y[s..s + w].iter().copied().max().unwrap_or(0);
            // 下沉直到避开一切障碍（障碍有限 ⇒ 必终止）。
            let mut guard = 0;
            while guard < 64 {
                let hit = obstacles.iter().find(|(ox, oy, ow, oh)| {
                    let ax = (s as i64 * (GRID_COL + GRID_GAP)) as f64;
                    let aw = (w as i64 * GRID_COL + (w as i64 - 1) * GRID_GAP) as f64;
                    ax < *ox + *ow && *ox < ax + aw && (y as f64) < *oy + *oh && *oy < y as f64 + h_px as f64
                });
                match hit {
                    Some(ob) => y = (ob.1 + ob.3 + GRID_GAP as f64).ceil() as i64,
                    None => break,
                }
                guard += 1;
            }
            if y < best_y {
                best_y = y;
                best = s;
            }
        }
        out.push(MeasuredPlacement { key, col: best, y_px: best_y, w, h_px });
        for cell in &mut col_y[best..best + w] {
            *cell = best_y + h_px + GRID_GAP;
        }
    }
    let total_h = if out.is_empty() {
        0
    } else {
        col_y.iter().copied().max().unwrap_or(0) - GRID_GAP
    };
    (out, total_h)
}

/// 矩形求交判重叠（测试用谓词，JS 侧同款数学）。
#[cfg(test)]
fn overlap(a: &MeasuredPlacement, b: &MeasuredPlacement) -> bool {
    let ax = a.col as i64 * (GRID_COL + GRID_GAP);
    let bx = b.col as i64 * (GRID_COL + GRID_GAP);
    let aw = a.w as i64 * GRID_COL + (a.w as i64 - 1) * GRID_GAP;
    let bw = b.w as i64 * GRID_COL + (b.w as i64 - 1) * GRID_GAP;
    !(ax + aw <= bx || bx + bw <= ax || a.y_px + a.h_px <= b.y_px || b.y_px + b.h_px <= a.y_px)
}

#[cfg(test)]
fn card(key: &str, w: i64, h: i64, measured: bool) -> Value {
    let k = if measured { "hPx" } else { "h" };
    serde_json::json!({ "key": key, "w": w, k: h })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn columns_for_width_floors_and_clamps() {
        assert_eq!(columns_for_width(270.0), 1);
        assert_eq!(columns_for_width(540.0), 2);
        assert_eq!(columns_for_width(1600.0), 5);
        assert_eq!(columns_for_width(100.0), 1, "窄屏保底 1 列");
    }

    #[test]
    fn layout_grid_no_overlap_clamps_and_covers() {
        let cards = vec![card("a", 2, 3, false), card("b", 2, 1, false), card("c", 9, 2, false)];
        let (pos, total) = layout_grid(&cards, 4);
        assert_eq!(pos.len(), 3);
        assert_eq!(pos.iter().map(|p| p.key.as_str()).collect::<Vec<_>>(), vec!["a", "b", "c"]);
        assert_eq!(pos[2].w, 4, "超宽钳到列数");
        assert!(total >= 3, "totalRows 覆盖");
        // 声明格坐标的相交判定
        let rects: Vec<(i64, i64, i64, i64)> = pos
            .iter()
            .map(|p| {
                (
                    p.col as i64 * (GRID_COL + GRID_GAP),
                    p.row * (GRID_ROW + GRID_GAP),
                    p.w as i64 * GRID_COL + (p.w as i64 - 1) * GRID_GAP,
                    p.h * GRID_ROW + (p.h - 1) * GRID_GAP,
                )
            })
            .collect();
        for i in 0..rects.len() {
            for j in i + 1..rects.len() {
                let (ax, ay, aw, ah) = rects[i];
                let (bx, by, bw, bh) = rects[j];
                let disj = ax + aw <= bx || bx + bw <= ax || ay + ah <= by || by + bh <= ay;
                assert!(disj, "重叠 {}×{}: {:?} {:?}", pos[i].key, pos[j].key, rects[i], rects[j]);
            }
        }
    }

    #[test]
    fn layout_measured_never_overlaps_and_clamps() {
        let cards = vec![
            card("a", 2, 300, true),
            card("b", 2, 100, true),
            card("c", 1, 500, true),
            card("d", 9, 50, true),
        ];
        let (pos, total) = layout_measured(&cards, 4);
        assert_eq!(pos.len(), 4);
        for i in 0..pos.len() {
            for j in i + 1..pos.len() {
                assert!(!overlap(&pos[i], &pos[j]), "重叠 {}×{}", pos[i].key, pos[j].key);
            }
        }
        assert_eq!(pos.iter().map(|p| p.key.as_str()).collect::<Vec<_>>(), vec!["a", "b", "c", "d"], "顺序保持");
        assert_eq!(pos[3].w, 4, "超宽 span 钳到列数");
        assert!(total >= 500, "总高覆盖内容");
        let (e, t) = layout_measured(&[], 4);
        assert!(e.is_empty());
        assert_eq!(t, 0);
    }

    #[test]
    fn layout_measured_skips_keyless_entries() {
        let cards = vec![serde_json::json!({ "w": 1, "hPx": 10 }), card("ok", 1, 10, true)];
        let (pos, _) = layout_measured(&cards, 2);
        assert_eq!(pos.len(), 1);
        assert_eq!(pos[0].key, "ok");
    }

    /// D-215：无障碍=原行为；有障碍=候选下沉绕行，永不与障碍相交。
    #[test]
    fn layout_measured_avoid_routes_around_obstacles() {
        let cards = vec![card("a", 1, 100, true), card("b", 1, 100, true), card("c", 2, 60, true)];
        let (plain, _) = layout_measured(&cards, 3);
        let (same, _) = layout_measured_avoid(&cards, 3, &[]);
        assert_eq!(plain, same, "无障碍=原样");
        // 障碍竖占第 1 列 0..250（像素 x=270..530）
        let obstacles = vec![(270.0f64, 0.0, 260.0, 250.0)];
        let (pos, _total) = layout_measured_avoid(&cards, 3, &obstacles);
        for p in &pos {
            let ax = (p.col as i64 * (GRID_COL + GRID_GAP)) as f64;
            let aw = (p.w as i64 * GRID_COL + (p.w as i64 - 1) * GRID_GAP) as f64;
            let (ox, oy, ow, oh) = obstacles[0];
            let hit = ax < ox + ow && ox < ax + aw && (p.y_px as f64) < oy + oh && oy < p.y_px as f64 + p.h_px as f64;
            assert!(!hit, "装箱卡 {} 撞上障碍 {:?}", p.key, p);
        }
        // 互不重叠不变式在有障碍时同样成立
        for i in 0..pos.len() {
            for j in i + 1..pos.len() {
                assert!(!overlap(&pos[i], &pos[j]), "重叠 {}×{}", pos[i].key, pos[j].key);
            }
        }
    }
}
