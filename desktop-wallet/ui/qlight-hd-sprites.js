/* QLight HD Classic v1 - cosmetic only, compatible 8x8 atlas; no chain state changes. */
(function (global) {
  'use strict';
  var base = global.QLSprites;
  if (!base || typeof base.atlas !== 'function') return;
  var originalAtlas = base.atlas();
  var TILE=32, COLS=8, ROWS=8, atlasCanvas=document.createElement('canvas');
  atlasCanvas.width=TILE*COLS; atlasCanvas.height=TILE*ROWS;
  var ctx=atlasCanvas.getContext('2d');
  ctx.imageSmoothingEnabled=false;
  // Preserve exact slot IDs and transparent pixels. Enrich edges with highlights/shadows.
  for(var id=0; id<22; id++) {
    var sx=(id%COLS)*16,sy=Math.floor(id/COLS)*16;
    var temp=document.createElement('canvas'); temp.width=temp.height=TILE;
    var t=temp.getContext('2d');t.imageSmoothingEnabled=false;
    t.drawImage(originalAtlas,sx,sy,16,16,0,0,TILE,TILE);
    var image=t.getImageData(0,0,TILE,TILE), data=image.data;
    var original=new Uint8ClampedArray(data);
    for(var y=0;y<TILE;y++)for(var x=0;x<TILE;x++){
      var i=(y*TILE+x)*4;if(original[i+3]<128)continue;
      var up=y>0?((y-1)*TILE+x)*4:i;
      var left=x>0?(y*TILE+x-1)*4:i;
      var down=y<TILE-1?((y+1)*TILE+x)*4:i;
      var light=(y>0&&original[up+3]<128)||(x>0&&original[left+3]<128);
      var shadow=(y<TILE-1&&original[down+3]<128);
      var grain=((x*37+y*59+id*113)^(x*y*17))&15;
      var amount=light?1.13:(shadow?0.84:(grain<3?1.10:grain>12?0.92:1));
      if(id===20||id===21)amount=light?1.08:amount;
      for(var c=0;c<3;c++) data[i+c]=Math.max(0,Math.min(255,Math.round(original[i+c]*amount)));
    }
    t.putImageData(image,0,0);ctx.drawImage(temp,(id%COLS)*TILE,Math.floor(id/COLS)*TILE);
  }
  function icon(idx,size) {
    size=size||32;var c=document.createElement('canvas');c.width=c.height=size;
    var g=c.getContext('2d');g.imageSmoothingEnabled=false;
    g.drawImage(atlasCanvas,(idx%8)*TILE,Math.floor(idx/8)*TILE,TILE,TILE,0,0,size,size);return c;
  }
  function avgColor(idx){var d=icon(idx,32).getContext('2d').getImageData(0,0,32,32).data,r=0,g=0,b=0,n=0;
    for(var i=0;i<d.length;i+=4){if(d[i+3]<128)continue;r+=d[i];g+=d[i+1];b+=d[i+2];n++;}return n?[Math.round(r/n),Math.round(g/n),Math.round(b/n)]:[200,200,200];}
  base.TILE=TILE;base.atlas=function(){return atlasCanvas;};base.icon=icon;
  base.dataUrl=function(idx,size){return icon(idx,size).toDataURL();};base.avgColor=avgColor;
  base.HD_PACK='QLight HD Classic v1';
})(window);
