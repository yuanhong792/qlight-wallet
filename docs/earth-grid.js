// 星球网格口径：2048 × 1024 等经纬高程网格 ↔ 米，纯整数/纯算术，零依赖。
//
// 为什么从 planet-view.js 搬出来单独成文件：planet-view 是**星球渲染层**，顶部就
// `import * as THREE`。但这份字节口径不只画球要用——整颗地球的 2D 地图（earth-map.js）、
// 席位地块生成（earth-local.js）、官网那页都要用。让它们陪着球去依赖 three.js，
// 等于"看一张地图要先加载一个 3D 引擎"；官网那种页面更是白背一份 vendor。
// 口径本身仍然是**一份**：谁抄一份网格，谁就会在极地和海岸各说各话。
//
// 口径真身：tools/world/fetch_earth_planet.py；Rust 侧 world/earth_local.rs 另有一份，
// 必须算出同一个格——对拍见 dev/seat-crosscheck.html 与 tools/world/seat_crosscheck.py。
export const GW = 2048, GH = 1024;      // 等经纬高程网格：1 格 ≈ 19.6 公里
export const SEA_CODES = 128;           // <128 海，>=128 陆，0 = 无数据
export const OCEAN_STEP = 48.0;
export const LAND_BUCKETS = [[0, 4, 32], [128, 8, 16], [256, 16, 16], [512, 32, 16],
                             [1024, 64, 16], [2048, 128, 16], [4096, 256, 16]];

/** 星球层 1 字节 → 米。0 是"这里没有数据"，返回 NaN：补 0 会把缺数据烤成一片海，
 *  而一片海看上去并不荒谬，所以错得很安静。 */
export function unquantize(b) {
  if (b === 0) return NaN;
  if (b < SEA_CODES) return -((SEA_CODES - 1 - b) + 0.5) * OCEAN_STEP;
  let base = SEA_CODES;
  for (const [start, step, count] of LAND_BUCKETS) {
    if (b < base + count) return start + (b - base + 0.5) * step;
    base += count;
  }
  return 8192;
}

/** 经纬度 → 网格索引：等经纬就是两次乘加，没有墨卡托在极地的面积爆炸。 */
export function cellOf(lat, lon) {
  return [Math.min(GW - 1, Math.max(0, Math.floor((lon + 180) / 360 * GW))),
          Math.min(GH - 1, Math.max(0, Math.floor((90 - lat) / 180 * GH)))];
}

export function heightAt(H, lat, lon) {
  const [i, j] = cellOf(lat, lon);
  return unquantize(H[j * GW + i]);
}
