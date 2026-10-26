/* QLight 世界 · 程序化音效（WebAudio，零素材）
 *
 * 《我的世界》的沉浸感一半来自声音：脚踩草地、方块落地的闷响、拆掉的碎响。
 * 这里不打包任何音频文件，全部用振荡器/噪声当场合成，体积为零。
 * AudioContext 只在用户第一次操作时创建（浏览器自动播放策略）。
 */
(function (global) {
  'use strict';
  var ctx = null, noise = null, enabled = true;

  function ac() {
    if (!enabled) return null;
    if (!ctx) {
      var C = global.AudioContext || global.webkitAudioContext;
      if (!C) return null;
      ctx = new C();
    }
    if (ctx.state === 'suspended') ctx.resume();
    return ctx;
  }

  function noiseBuf(c) {
    if (noise && noise.sampleRate === c.sampleRate) return noise;
    var n = Math.floor(c.sampleRate * 0.4);
    var b = c.createBuffer(1, n, c.sampleRate);
    var d = b.getChannelData(0);
    for (var i = 0; i < n; i++) d[i] = Math.random() * 2 - 1;
    noise = b;
    return b;
  }

  function tone(f0, f1, dur, type, vol) {
    var c = ac();
    if (!c) return;
    var o = c.createOscillator(), g = c.createGain();
    o.type = type || 'triangle';
    o.frequency.setValueAtTime(f0, c.currentTime);
    o.frequency.exponentialRampToValueAtTime(Math.max(20, f1), c.currentTime + dur);
    g.gain.setValueAtTime(vol || 0.12, c.currentTime);
    g.gain.exponentialRampToValueAtTime(0.0001, c.currentTime + dur);
    o.connect(g); g.connect(c.destination);
    o.start(); o.stop(c.currentTime + dur + 0.02);
  }

  function burst(dur, freq, vol, type) {
    var c = ac();
    if (!c) return;
    var s = c.createBufferSource(), f = c.createBiquadFilter(), g = c.createGain();
    s.buffer = noiseBuf(c);
    f.type = type || 'bandpass';
    f.frequency.value = freq;
    f.Q.value = 0.8;
    g.gain.setValueAtTime(vol || 0.08, c.currentTime);
    g.gain.exponentialRampToValueAtTime(0.0001, c.currentTime + dur);
    s.connect(f); f.connect(g); g.connect(c.destination);
    s.start(); s.stop(c.currentTime + dur + 0.02);
  }

  global.QLSfx = {
    place: function () { tone(180, 70, 0.12, 'triangle', 0.16); burst(0.05, 300, 0.05, 'lowpass'); },
    break: function () { burst(0.18, 900, 0.11); tone(120, 55, 0.1, 'square', 0.05); },
    step: function () { burst(0.055, 500, 0.035, 'lowpass'); },
    chop: function () { tone(520, 260, 0.07, 'square', 0.05); },
    claim: function () { tone(660, 660, 0.09, 'sine', 0.09); setTimeout(function () { tone(990, 990, 0.16, 'sine', 0.08); }, 90); },
    deny: function () { tone(220, 140, 0.16, 'sawtooth', 0.07); },
    toggle: function () { enabled = !enabled; return enabled; },
    isOn: function () { return enabled; },
  };
})(window);
