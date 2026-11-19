//! 席位导出（`QL-SEAT-EXPORT-v1`）——「走进我的席位」。
//!
//! 把席位中心换算成一个 ~1 公里见方的 bbox，交给随包的 arnis 无头 CLI
//! （Apache-2.0，github.com/louis-e/arnis）生成一张 Minecraft 存档。
//!
//! 边界（与席位/实测窗口分离同一条原则）：存档是本地"明信片"——不写链、
//! 不参与共识、不进 `entity_root`。矿脉铁律一寸不动：这里只是把真实世界的
//! 建筑与道路渲染进另一个游戏，和 DEM 烘焙一样属于"外部数据源"，但比 DEM
//! 更弱——它连共识侧都不进，纯本地产物。
//!
//! 生成需要联网（arnis 现拉 OpenStreetMap 与高程瓦片），UI 文案必须说清。

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

/// 默认窗口边长（米）。席位一格约 19.6 公里，arnis 吃不下整格；
/// 1 公里是实测过内存与耗费的尺度（崂山 1 km² <2 分钟、青岛老城 600 m² 热缓存 7 秒）。
pub const WINDOW_M: f64 = 1000.0;
/// 每度纬度的地面米数（球形近似，与 arnis 的局部投影同量级）。
const M_PER_DEG: f64 = 111_320.0;

/// 一把全局锁：同一时刻只许一份生成在跑。第二次点击当场报错，不排队。
static BUSY: AtomicBool = AtomicBool::new(false);

/// RAII 释放 BUSY——中途任何 `?` 提前返回都不会把锁留下。
#[derive(Debug)]
struct BusyGuard;
impl BusyGuard {
    fn acquire() -> Result<Self, String> {
        BUSY
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .map(|_| BusyGuard)
            .map_err(|_| "正在生成中，请等上一份完成".to_string())
    }
}
impl Drop for BusyGuard {
    fn drop(&mut self) {
        BUSY.store(false, Ordering::SeqCst);
    }
}

/// 席位中心 → arnis `--bbox` 的四个数（min_lat, min_lon, max_lat, max_lon）。
/// 经度跨度按 cos(lat) 放宽，保证东西向也是 ~1 公里而不是随纬度缩水。
pub fn seat_bbox(lat: f64, lon: f64) -> (f64, f64, f64, f64) {
    let dlat = (WINDOW_M / 2.0) / M_PER_DEG;
    let dlon = (WINDOW_M / 2.0) / (M_PER_DEG * lat.to_radians().cos().max(0.05));
    (lat - dlat, lon - dlon, lat + dlat, lon + dlon)
}

pub fn bbox_arg(b: (f64, f64, f64, f64)) -> String {
    format!("{:.6},{:.6},{:.6},{:.6}", b.0, b.1, b.2, b.3)
}

/// 随包 arnis 的位置：钱包 exe 同目录。开发期（target/debug 或 release）
/// 旁边没有它，报错文案直接说清缺什么，不偷偷去别处找。
pub fn arnis_path() -> Result<PathBuf, String> {
    let exe = std::env::current_exe().map_err(|e| format!("读不到自身路径：{e}"))?;
    let dir = exe
        .parent()
        .ok_or_else(|| "自身路径没有父目录".to_string())?;
    Ok(dir.join("arnis.exe"))
}

/// 存档落点：优先 Minecraft 自己的 saves 目录（生成完直接在游戏里看见），
/// 没有 Minecraft 就落文档下的 QLight-Worlds，不引对话框插件。
pub fn default_out_root() -> PathBuf {
    #[cfg(target_os = "windows")]
    {
        if let Some(appdata) = std::env::var_os("APPDATA") {
            let mc = PathBuf::from(appdata).join(".minecraft").join("saves");
            if mc.is_dir() {
                return mc;
            }
        }
        if let Some(home) = std::env::var_os("USERPROFILE") {
            return PathBuf::from(home).join("Documents").join("QLight-Worlds");
        }
    }
    #[cfg(target_os = "macos")]
    {
        if let Some(home) = std::env::var_os("HOME") {
            let mc = PathBuf::from(&home)
                .join("Library/Application Support/minecraft/saves");
            if mc.is_dir() {
                return mc;
            }
            return PathBuf::from(home).join("Documents").join("QLight-Worlds");
        }
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        if let Some(home) = std::env::var_os("HOME") {
            let mc = PathBuf::from(&home).join(".minecraft").join("saves");
            if mc.is_dir() {
                return mc;
            }
            return PathBuf::from(home).join("QLight-Worlds");
        }
    }
    PathBuf::from("QLight-Worlds")
}

/// 跑一次生成。`progress` 收 (百分比, 消息)；返回新存档目录。
/// 不依赖 Tauri——command 只是把回调接成事件，单测与无头 e2e 直接调这里。
pub fn run_export<F>(
    arnis: &Path,
    lat: f64,
    lon: f64,
    out_root: &Path,
    mut progress: F,
) -> Result<PathBuf, String>
where
    F: FnMut(u8, String),
{
    let _guard = BusyGuard::acquire()?;
    if !arnis.is_file() {
        return Err(format!(
            "缺随包组件 arnis.exe（应在钱包 exe 同目录：{}）。发行包自带；开发期请手动 copy 一份过去。",
            arnis.display()
        ));
    }
    std::fs::create_dir_all(out_root).map_err(|e| format!("建输出目录失败：{e}"))?;
    let started = std::time::SystemTime::now();
    let bbox = seat_bbox(lat, lon);
    let mut child = Command::new(arnis)
        .arg(format!("--bbox={}", bbox_arg(bbox)))
        .arg(format!("--output-dir={}", out_root.display()))
        .arg("--progress")
        .arg("json")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("启动 arnis 失败：{e}"))?;

    // stderr 单独一条线程收干，避免管道写满把子进程堵死。
    let stderr_buf = Arc::new(Mutex::new(String::new()));
    let stderr_thread = child.stderr.take().map(|se| {
        let buf = stderr_buf.clone();
        std::thread::spawn(move || {
            let mut acc = String::new();
            for line in BufReader::new(se).lines().map_while(|l| l.ok()) {
                acc.push_str(&line);
                acc.push('\n');
            }
            *buf.lock().unwrap() = acc;
        })
    });

    let mut last_err = String::new();
    if let Some(so) = child.stdout.take() {
        for line in BufReader::new(so).lines().map_while(|l| l.ok()) {
            let Ok(v) = serde_json::from_str::<serde_json::Value>(&line) else {
                continue;
            };
            match v.get("type").and_then(|t| t.as_str()) {
                Some("progress") => {
                    let pct = v
                        .get("progress")
                        .and_then(|p| p.as_f64())
                        .map(|p| (p * 100.0).clamp(0.0, 100.0) as u8)
                        .unwrap_or(0);
                    let msg = v
                        .get("message")
                        .and_then(|m| m.as_str())
                        .unwrap_or("")
                        .to_string();
                    progress(pct, msg);
                }
                Some("error") => {
                    last_err = v
                        .get("message")
                        .and_then(|m| m.as_str())
                        .unwrap_or("unknown")
                        .to_string();
                }
                _ => {}
            }
        }
    }
    let status = child.wait().map_err(|e| format!("等 arnis 退出失败：{e}"))?;
    if let Some(t) = stderr_thread {
        let _ = t.join();
    }
    if !status.success() {
        let se = stderr_buf.lock().unwrap().clone();
        let tail: String = se.lines().rev().take(3).collect::<Vec<_>>().join(" / ");
        let err_part = if last_err.is_empty() {
            String::new()
        } else {
            format!("：{last_err}")
        };
        let se_part = if tail.is_empty() {
            String::new()
        } else {
            format!("；stderr：{tail}")
        };
        return Err(format!(
            "生成失败（arnis 退出码 {:?}）{}{}",
            status.code(),
            err_part,
            se_part
        ));
    }
    // 只认本次开工之后落盘的存档：out_root 里可能躺着玩家自己的旧世界。
    let mut best: Option<(std::time::SystemTime, PathBuf)> = None;
    for entry in std::fs::read_dir(out_root).map_err(|e| format!("读输出目录失败：{e}"))? {
        let Ok(entry) = entry else { continue };
        let dir = entry.path();
        if !dir.join("level.dat").is_file() {
            continue;
        }
        let Ok(md) = entry.metadata().and_then(|m| m.modified()) else {
            continue;
        };
        if md < started {
            continue;
        }
        if best.as_ref().map(|(t, _)| md > *t).unwrap_or(true) {
            best = Some((md, dir));
        }
    }
    best.map(|(_, p)| p)
        .ok_or_else(|| "arnis 报成功但输出目录里没有新存档（level.dat 缺失）".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bbox_is_one_km_both_ways() {
        for lat in [0.0_f64, 36.1527, 60.0, -33.0] {
            let (a, b, c, d) = seat_bbox(lat, 120.6652);
            let ns = (c - a) * M_PER_DEG;
            let ew = (d - b) * M_PER_DEG * lat.to_radians().cos();
            assert!((ns - WINDOW_M).abs() < 1.0, "lat {lat} ns {ns}");
            assert!((ew - WINDOW_M).abs() < 1.0, "lat {lat} ew {ew}");
            assert!(((a + c) / 2.0 - lat).abs() < 1e-12);
            assert!(((b + d) / 2.0 - 120.6652).abs() < 1e-12);
        }
    }

    #[test]
    fn bbox_arg_order_is_arnis_order() {
        let s = bbox_arg(seat_bbox(36.1527, 120.6652));
        let v: Vec<f64> = s.split(',').map(|x| x.parse().unwrap()).collect();
        assert_eq!(v.len(), 4);
        assert!(v[0] < v[2] && v[1] < v[3]);
        assert!(v[0] < 36.1527 && v[2] > 36.1527);
    }

    #[test]
    fn busy_lock_refuses_second_run() {
        let _g = BusyGuard::acquire().unwrap();
        let err = BusyGuard::acquire().unwrap_err();
        assert!(err.contains("正在生成"));
    }

    #[test]
    fn missing_arnis_is_a_clear_chinese_error() {
        let out = std::env::temp_dir().join("qlight-seat-export-test-missing");
        let err = run_export(
            &out.join("definitely-not-arnis.exe"),
            36.1527,
            120.6652,
            &out,
            |_, _| {},
        )
        .unwrap_err();
        assert!(err.contains("缺随包组件"), "{err}");
        // 锁必须已释放：紧接着再调一次仍报缺组件而不是报忙。
        let err2 = run_export(
            &out.join("definitely-not-arnis.exe"),
            36.1527,
            120.6652,
            &out,
            |_, _| {},
        )
        .unwrap_err();
        assert!(err2.contains("缺随包组件"), "{err2}");
    }

    /// 无头端到端：真 arnis + 真网络。默认不跑（CI 没有 arnis 也不该拉网），
    /// 手动：ARNIS_EXE=D:\arnis-spike\target\release\arnis.exe cargo test --lib seat_export -- --ignored
    #[test]
    #[ignore]
    fn e2e_real_arnis_headless() {
        let arnis = std::env::var("ARNIS_EXE").expect("设 ARNIS_EXE 指向 arnis 无头 exe");
        let out = std::env::temp_dir().join("qlight-seat-export-e2e");
        let mut seen = 0_u32;
        let world = run_export(
            Path::new(&arnis),
            36.1527,
            120.6652,
            &out,
            |pct, msg| {
                seen += 1;
                eprintln!("[{pct}%] {msg}");
            },
        )
        .expect("生成应成功");
        assert!(world.join("level.dat").is_file());
        assert!(world.join("region").is_dir());
        assert!(seen > 0, "应收到进度事件");
        eprintln!("world: {}", world.display());
    }
}