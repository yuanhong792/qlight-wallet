//! 外壳偏好：欢迎页是否还要弹、引导走到哪一步。
//!
//! 为什么写成文件而不是 `localStorage`：这台机器上同机多开的每个实例都有自己的
//! 数据目录与自己的账本（`node-1`、`node-2`……），而 webview 的存储是按应用共享的。
//! 进度属于"哪本账玩到哪"，不属于"哪个渲染进程"——所以跟着账本放在一起。
//!
//! 读到一个坏文件绝不能把程序锁死在启动画面：与实体/光尘账本同一纪律
//! （§9.18），坏的东西只影响它自己那一格，回落到默认值并把话说出来。

use serde::{Deserialize, Serialize};
use std::path::Path;

/// 只有布尔位：前端能置的就是这几个，别的形状写不进来
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(default)]
pub struct Prefs {
    /// 欢迎页已经看过并且不再需要（勾选「以后不再显示」）
    pub welcome_done: bool,
    /// 进过第一人称世界 —— 引导第 1 步。这是**导航事实**，不是账本事实，界面上要这么标
    pub opened_world: bool,
    /// 看过自己的活动/记录 —— 引导第 4 步，同样是导航事实
    pub opened_history: bool,
    /// 玩家自己把引导清单收起来了（做完与否都允许收）
    pub guide_done: bool,
}

/// 白名单写入。未知键返回错误而不是静默忽略：静默忽略会让"我以为存下来了"
/// 变成下一次重启时的意外。
pub fn apply(p: &mut Prefs, key: &str, value: bool) -> Result<(), String> {
    match key {
        "welcome_done" => p.welcome_done = value,
        "opened_world" => p.opened_world = value,
        "opened_history" => p.opened_history = value,
        "guide_done" => p.guide_done = value,
        other => return Err(format!("不认识的偏好键：{other}")),
    }
    Ok(())
}

/// 读出偏好。返回 `(偏好, 告警)`：告警只在文件确实坏了的时候是 `Some`，
/// 文件不存在（首次运行）不是坏，是常态。
pub fn load(path: &Path) -> (Prefs, Option<String>) {
    let raw = match std::fs::read(path) {
        Ok(r) => r,
        Err(_) => return (Prefs::default(), None),
    };
    match serde_json::from_slice::<Prefs>(&raw) {
        Ok(p) => (p, None),
        Err(e) => (
            Prefs::default(),
            Some(format!("偏好文件读不出来（{e}），按首次运行处理")),
        ),
    }
}

pub fn save(path: &Path, p: &Prefs) -> Result<(), String> {
    let out = serde_json::to_vec_pretty(p).map_err(|e| e.to_string())?;
    std::fs::write(path, out).map_err(|e| format!("写入偏好失败：{e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_path(tag: &str) -> std::path::PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        std::env::temp_dir().join(format!("qlight-prefs-{tag}-{}-{nanos}.json", std::process::id()))
    }

    #[test]
    fn a_missing_file_is_a_first_run_not_a_failure() {
        let (p, warn) = load(&tmp_path("absent"));
        assert_eq!(p, Prefs::default());
        assert!(warn.is_none(), "首次运行不该向玩家报错");
    }

    /// 坏偏好不许把程序锁死在启动画面：回落默认值 + 说清楚发生了什么
    #[test]
    fn a_corrupt_prefs_file_falls_back_without_bringing_the_app_down() {
        let path = tmp_path("corrupt");
        std::fs::write(&path, b"{ this is not json").unwrap();
        let (p, warn) = load(&path);
        assert_eq!(p, Prefs::default());
        assert!(warn.is_some());
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn written_prefs_read_back() {
        let path = tmp_path("roundtrip");
        let mut p = Prefs::default();
        p.opened_world = true;
        p.welcome_done = true;
        save(&path, &p).unwrap();
        let (again, warn) = load(&path);
        assert_eq!(again, p);
        assert!(warn.is_none());
        std::fs::remove_file(&path).ok();
    }

    /// 未知键必须失败：静默收下会让前端以为已经存下来了
    #[test]
    fn an_unknown_key_is_refused() {
        let mut p = Prefs::default();
        assert!(apply(&mut p, "cheat_mode", true).is_err());
        assert_eq!(p, Prefs::default(), "被拒的写入不许留下任何痕迹");
        assert!(apply(&mut p, "opened_world", true).is_ok());
        assert!(p.opened_world);
    }

    /// 引导收起状态必须真的能存下来：每次重启都弹一遍清单，玩家会直接关掉整个界面
    #[test]
    fn the_collapsed_guide_stays_collapsed() {
        let path = tmp_path("guide");
        let mut p = Prefs::default();
        apply(&mut p, "guide_done", true).unwrap();
        save(&path, &p).unwrap();
        let (again, _) = load(&path);
        assert!(again.guide_done);
        std::fs::remove_file(&path).ok();
    }
}
