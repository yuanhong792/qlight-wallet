//! 兑换池 `QL-POOL-v1`：QLX ↔ 光尘的恒定乘积做市。
//!
//! 为什么池子没有地址、没有私钥：账户制的链上，"替大家保管"意味着某个私钥能
//! 把池子里的钱转走。这里把两边储备直接记在链的账本状态里（`Ledger.pools`），
//! 出账只由这条曲线放行——没有任何一把钥匙能绕过它。
//!
//! 两边都是整数最小单位：QLX 是 10^-9，光尘是 1 ✦。中间量一律走 u128。
//!
//! 手续费 0.3% 不从储备里抽走，而是留在池子里：付进去的是全额，算出来的是
//! 扣费后的量，差额自动变成 LP 的收益。这也是 k 只增不减的来源。

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub const POOL_RULE_VERSION: &str = "QL-POOL-v1";
/// 目前只有一对。将来要开第二对，把 id 做成参数即可。
pub const POOL_ID: &str = "qlx/dust";
/// 0.30% —— 和主流 AMM 同档：套利者愿意来，做市商有得赚。
pub const FEE_BPS: u64 = 30;
const BPS: u64 = 10_000;
/// 首次做市永久锁死的最小份额：防止"1 wei 做市 + 捐赠"把后来者的份额算成 0。
pub const MIN_LIQUIDITY: u64 = 1_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dir {
    /// 拿 QLX 换光尘
    QlxIn,
    /// 拿光尘换 QLX
    DustIn,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Pool {
    /// QLX 储备（10^-9 QLX）
    pub qlx: u64,
    /// 光尘储备（1 ✦）
    pub dust: u64,
    /// LP 总份额（含锁死的那部分）
    pub lp_total: u64,
    pub lp: BTreeMap<String, u64>,
}

fn mul_div(a: u64, b: u64, c: u64) -> u64 {
    if c == 0 {
        return 0;
    }
    ((a as u128) * (b as u128) / (c as u128)) as u64
}

fn isqrt(n: u128) -> u128 {
    if n < 2 {
        return n;
    }
    let mut x = n;
    let mut y = (x + 1) / 2;
    while y < x {
        x = y;
        y = (x + n / x) / 2;
    }
    x
}

impl Pool {
    pub fn is_empty(&self) -> bool {
        self.lp_total == 0
    }

    pub fn ready(&self) -> bool {
        self.qlx > 0 && self.dust > 0
    }

    pub fn shares_of(&self, owner: &str) -> u64 {
        self.lp.get(owner).copied().unwrap_or(0)
    }

    /// 展示用：1 QLX 值多少光尘。QLX 按 10^9 最小单位换算。
    pub fn price_dust_per_qlx(&self) -> Option<f64> {
        if self.qlx == 0 {
            return None;
        }
        Some(self.dust as f64 * 1e9 / self.qlx as f64)
    }

    /// 只读报价：给我 `amount_in`，能拿到多少。
    pub fn quote(&self, dir: Dir, amount_in: u64) -> Result<u64, String> {
        if amount_in == 0 {
            return Err("数量必须大于 0".into());
        }
        if !self.ready() {
            return Err("池子还是空的——先有人做市才能换".into());
        }
        let (res_in, res_out) = match dir {
            Dir::QlxIn => (self.qlx, self.dust),
            Dir::DustIn => (self.dust, self.qlx),
        };
        // 手续费先扣掉，剩下的才参与曲线：差额留在池子里给 LP。
        let in_after = ((amount_in as u128) * ((BPS - FEE_BPS) as u128) / (BPS as u128)) as u64;
        if in_after == 0 {
            return Err("数量太小：扣完 0.3% 手续费什么都不剩".into());
        }
        let out = (res_out as u128) * (in_after as u128) / ((res_in as u128) + (in_after as u128));
        let out = out as u64;
        if out == 0 {
            return Err("换出来的数量按整数算成了 0——再加一点".into());
        }
        if out >= res_out {
            return Err("报价吃穿了储备，拒绝".into());
        }
        Ok(out)
    }

    /// 成交：先按同一份公式算，再要求不低于 `min_out`，最后才动储备。
    pub fn swap(&mut self, dir: Dir, amount_in: u64, min_out: u64) -> Result<u64, String> {
        let out = self.quote(dir, amount_in)?;
        if out < min_out {
            return Err(format!("滑点超出容忍：算出来 {out}，你要求至少 {min_out}"));
        }
        match dir {
            Dir::QlxIn => {
                self.qlx = self
                    .qlx
                    .checked_add(amount_in)
                    .ok_or("池子 QLX 储备溢出")?;
                self.dust = self.dust.checked_sub(out).ok_or("光尘储备不足")?;
            }
            Dir::DustIn => {
                self.dust = self
                    .dust
                    .checked_add(amount_in)
                    .ok_or("池子光尘储备溢出")?;
                self.qlx = self.qlx.checked_sub(out).ok_or("QLX 储备不足")?;
            }
        }
        Ok(out)
    }

    /// 做市：两边按现有比例存入，铸出 LP 份额。首次存入锁死 `MIN_LIQUIDITY`。
    pub fn add(&mut self, owner: &str, qlx_in: u64, dust_in: u64) -> Result<u64, String> {
        if qlx_in == 0 || dust_in == 0 {
            return Err("做市两边都要有：QLX 和光尘都得 > 0".into());
        }
        let minted;
        if self.lp_total == 0 {
            let total = isqrt((qlx_in as u128) * (dust_in as u128)) as u64;
            if total <= MIN_LIQUIDITY {
                return Err(format!(
                    "首次做市太小：份额算出来只有 {total}，至少要超过 {MIN_LIQUIDITY}（两边各加点）"
                ));
            }
            minted = total - MIN_LIQUIDITY;
            self.lp_total = total;
        } else {
            if !self.ready() {
                return Err("池子状态异常（储备为 0），拒绝做市".into());
            }
            let a = mul_div(qlx_in, self.lp_total, self.qlx);
            let b = mul_div(dust_in, self.lp_total, self.dust);
            let tol = 1 + a.max(b) / 1000;
            if a.abs_diff(b) > tol {
                return Err(format!(
                    "两边比例和池子不一致：按 QLX 算 {a} 份，按光尘算 {b} 份（允许差 {tol}）"
                ));
            }
            minted = a.min(b);
            if minted == 0 {
                return Err("存入太少，铸不出份额".into());
            }
            self.lp_total = self.lp_total.checked_add(minted).ok_or("LP 总额溢出")?;
        }
        self.qlx = self.qlx.checked_add(qlx_in).ok_or("QLX 储备溢出")?;
        self.dust = self.dust.checked_add(dust_in).ok_or("光尘储备溢出")?;
        *self.lp.entry(owner.to_string()).or_insert(0) += minted;
        Ok(minted)
    }

    /// 撤资：按份额占比取回两边。锁死的份额永远取不回来，所以池子不会被掏空。
    pub fn remove(&mut self, owner: &str, lp: u64) -> Result<(u64, u64), String> {
        if lp == 0 {
            return Err("撤资份额必须大于 0".into());
        }
        let held = self.shares_of(owner);
        if lp > held {
            return Err(format!("你的份额只有 {held}，撤不了 {lp}"));
        }
        let qlx_out = mul_div(self.qlx, lp, self.lp_total);
        let dust_out = mul_div(self.dust, lp, self.lp_total);
        if qlx_out == 0 || dust_out == 0 {
            return Err("撤出来的数量有一边是 0——再多留一会儿".into());
        }
        self.qlx -= qlx_out;
        self.dust -= dust_out;
        self.lp_total -= lp;
        let left = held - lp;
        if left == 0 {
            self.lp.remove(owner);
        } else {
            self.lp.insert(owner.to_string(), left);
        }
        Ok((qlx_out, dust_out))
    }

    /// k 不变量（只用于测试与自检）。
    pub fn k(&self) -> u128 {
        (self.qlx as u128) * (self.dust as u128)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seeded() -> Pool {
        let mut p = Pool::default();
        p.add("lp", 1_000_000_000_000_000, 1_000_000).unwrap(); // 1e6 QLX / 1e6 ✦
        p
    }

    #[test]
    fn swap_moves_the_price_the_right_way() {
        let mut p = seeded();
        let before = p.price_dust_per_qlx().unwrap();
        let out = p.swap(Dir::QlxIn, 1_000_000_000_000, 0).unwrap(); // 1000 QLX 换光尘
        assert!(out > 0);
        let after = p.price_dust_per_qlx().unwrap();
        assert!(after < before, "往池子里倒 QLX 应该让 QLX 变便宜");
        // 反方向同理
        let before = p.price_dust_per_qlx().unwrap();
        p.swap(Dir::DustIn, 10_000, 0).unwrap();
        assert!(p.price_dust_per_qlx().unwrap() > before);
    }

    #[test]
    fn k_never_shrinks() {
        let mut p = seeded();
        let k0 = p.k();
        p.swap(Dir::QlxIn, 3_000_000_000_000, 0).unwrap();
        assert!(p.k() >= k0);
        let k1 = p.k();
        p.swap(Dir::DustIn, 50_000, 0).unwrap();
        assert!(p.k() >= k1);
    }

    #[test]
    fn fee_is_left_in_the_pool() {
        let mut p = seeded();
        let res_in = p.qlx;
        let res_out = p.dust;
        let amount = 1_000_000_000_000u64;
        let out = p.swap(Dir::QlxIn, amount, 0).unwrap();
        // 无手续费时应该是 res_out*amount/(res_in+amount)
        let no_fee = ((res_out as u128) * (amount as u128) / ((res_in as u128) + amount as u128)) as u64;
        assert!(out < no_fee, "收的 0.3% 必须真的让用户少拿一点");
        assert_eq!(p.qlx, res_in + amount, "手续费留在池子里，储备收全额");
    }

    #[test]
    fn slippage_guard_refuses_a_bad_deal() {
        let mut p = seeded();
        let quoted = p.quote(Dir::QlxIn, 1_000_000_000_000).unwrap();
        assert!(p.swap(Dir::QlxIn, 1_000_000_000_000, quoted + 1).is_err());
        assert!(p.swap(Dir::QlxIn, 1_000_000_000_000, quoted).is_ok());
    }

    #[test]
    fn empty_pool_cannot_be_traded() {
        let mut p = Pool::default();
        assert!(p.quote(Dir::QlxIn, 1).is_err());
        assert!(p.swap(Dir::DustIn, 1, 0).is_err());
    }

    #[test]
    fn first_liquidity_locks_a_floor() {
        let mut p = Pool::default();
        let lp = p.add("a", 1_000_000_000_000_000, 1_000_000).unwrap();
        assert_eq!(p.lp_total, lp + MIN_LIQUIDITY);
        // 全部撤走也带不走锁死的那份，池子从此永远有储备
        p.remove("a", lp).unwrap();
        assert!(p.qlx > 0 && p.dust > 0, "锁死的份额保证池子不会被掏空");
        assert_eq!(p.lp_total, MIN_LIQUIDITY);
    }

    #[test]
    fn later_lp_must_match_the_ratio() {
        let mut p = seeded();
        assert!(p.add("b", 1_000_000_000_000, 1).is_err(), "比例差太远必须拒绝");
        let ok = p.add("b", 1_000_000_000_000, 1_000).unwrap();
        assert!(ok > 0);
    }

    #[test]
    fn remove_pays_back_proportionally() {
        let mut p = seeded();
        let half = p.shares_of("lp") / 2;
        let (q, d) = p.remove("lp", half).unwrap();
        // q 是 QLX 最小单位（10^-9），d 是 ✦：池子 1 QLX = 1 ✦，所以比值应贴近 1e9
        let ratio = q as f64 / d as f64;
        assert!((ratio / 1e9 - 1.0).abs() < 0.01, "撤资应按储备比例，实得比值 {ratio}");
        assert!(p.ready());
    }

    #[test]
    fn cannot_remove_more_than_you_hold() {
        let mut p = seeded();
        let held = p.shares_of("lp");
        assert!(p.remove("lp", held + 1).is_err());
    }

    #[test]
    fn dust_swap_cannot_drain_qlx() {
        let mut p = seeded();
        // 拿一笔相对储备很小的光尘，换出来的 QLX 必须严格小于储备
        let out = p.swap(Dir::DustIn, 1, 0).unwrap_or(0);
        assert!(out < p.qlx);
        // 大额也不行
        let before = p.qlx;
        let _ = p.swap(Dir::DustIn, 100_000_000, 0);
        assert!(p.qlx <= before);
    }

    #[test]
    fn quote_and_swap_agree() {
        let mut p = seeded();
        let q = p.quote(Dir::QlxIn, 7_000_000_000_000).unwrap();
        let got = p.swap(Dir::QlxIn, 7_000_000_000_000, 0).unwrap();
        assert_eq!(q, got);
    }
}
