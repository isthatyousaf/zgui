struct Vertex { @location(0) rect:vec4<f32>, @location(1) uv:vec4<f32>, @location(2) color:vec4<f32>, @location(3) fade:vec4<f32>, @location(4) options:vec4<f32>, @location(5) shape:vec4<f32>, @location(6) border:vec4<f32>, @location(7) mask:vec4<f32> }
// Only `uv` and `point` vary across a quad; the rest are per-quad constants.
// Flat, they cost nothing per pixel (a software rasterizer interpolates every
// smooth varying for every pixel it shades).
struct Out { @builtin(position) position:vec4<f32>, @location(0) uv:vec2<f32>, @location(1) @interpolate(flat) color:vec4<f32>, @location(2) point:vec2<f32>, @location(3) @interpolate(flat) fade:vec4<f32>, @location(4) @interpolate(flat) options:vec4<f32>, @location(5) @interpolate(flat) shape:vec4<f32>, @location(6) @interpolate(flat) border:vec4<f32>, @location(7) @interpolate(flat) mask:vec4<f32> }
// Group 1: one immutable buffer per viewport size, so layers never rewrite it.
@group(1) @binding(0) var<uniform> viewport:vec4<f32>;
@group(0) @binding(1) var atlas:texture_2d<f32>;
@group(0) @binding(2) var tex_sampler:sampler;
@vertex fn vs(v:Vertex,@builtin(vertex_index) i:u32)->Out {
 // A quad is a four-vertex strip, (0,0) (1,0) (0,1) (1,1): its two
 // triangles with two fewer vertex invocations than a list. Corners come
 // from the index's bits: a runtime index into a local array makes Metal
 // reserve per-thread scratch memory for every thread the GPU can run.
 let corner=vec2(f32(i&1u),f32(i>>1u));
 let p=v.rect.xy+corner*v.rect.zw;
 var position=p;
 if v.options.z>0.5 {position=vec2(dot(v.shape.xy,p)+v.border.x,dot(v.shape.zw,p)+v.border.y);}
 var o:Out; o.position=vec4(position.x/viewport.x*2.-1.,1.-position.y/viewport.y*2.,0.,1.);o.uv=v.uv.xy+corner*v.uv.zw;o.color=v.color;o.point=p;o.fade=v.fade;o.options=v.options;o.shape=v.shape;o.border=v.border;o.mask=v.mask;return o;
}
fn shade(input:Out)->vec4<f32> {
 // Where the pixel is, in logical units: from its own position, so a quad
 // drawn in pieces (`push_panel`) shades every pixel as the whole would. A
 // transformed quad keeps its interpolated, untransformed position.
 var v=input;
 if v.options.z<0.5 {v.point=v.position.xy/viewport.z;}
 var color=v.color;
 if v.options.z<0.5 && (v.shape.w>0.5 || v.shape.z>0.) {
   var corner=v.shape.x;
   if v.options.w>0.5 {
     let right=v.point.x>v.fade.x+v.fade.z*0.5; let bottom=v.point.y>v.fade.y+v.fade.w*0.5;
     corner=select(select(v.border.x,v.border.y,right),select(v.border.w,v.border.z,right),bottom);
   }
   let radius=clamp(corner,0.,min(v.fade.z,v.fade.w)*0.5);
   let q=abs(v.point-v.fade.xy-v.fade.zw*0.5)-(v.fade.zw*0.5-vec2(radius));
   let distance=length(max(q,vec2(0.)))+min(max(q.x,q.y),0.)-radius;
   if v.shape.z>0. {color.a*=exp(-0.5*pow(max(distance,0.)/v.shape.z,2.));}
   else {let aa=max(fwidth(distance),0.5);let outer=clamp(0.5-distance/aa,0.,1.);let inner=clamp(0.5-(distance+v.shape.y)/aa,0.,1.);let alpha=color.a*inner+v.border.a*(outer-inner);let rgb=select(vec3(0.),(color.rgb*color.a*inner+v.border.rgb*v.border.a*(outer-inner))/max(alpha,0.00001),alpha>0.);color=vec4(rgb,alpha);}
 }

 // Premultiplied texels carry their alpha already: the colour stays
 // premultiplied from here (`basic`'s product), scaled by whatever later
 // scales alpha, rather than divided by it and multiplied back.
 var premultiplied=false; var rgb=vec3(0.); var scale=1.;
 if v.options.y>0.5 {
   let tex=textureSample(atlas,tex_sampler,v.uv);
   if v.options.y>1.5 {premultiplied=true; rgb=color.rgb*tex.rgb*color.a; color.a=color.a*tex.a;}
   else {color=vec4(color.rgb*tex.rgb,color.a*tex.a);}
 }
 if v.options.x>0. {let distance=min(v.point-v.fade.xy,v.fade.xy+v.fade.zw-v.point); let f=clamp(min(distance.x,distance.y)/v.options.x,0.,1.); color.a*=f; scale*=f;}
 // A `fade_edges` ancestor: a quadratic ramp over each band, as Zeron fades
 // transcript edges.
 if v.mask.z>0. {let t=clamp((v.point.y-v.mask.x)/v.mask.z,0.,1.); color.a*=t*t; scale*=t*t;}
 if v.mask.w>0. {let b=clamp((v.mask.y-v.point.y)/v.mask.w,0.,1.); color.a*=b*b; scale*=b*b;}
 return select(vec4(color.rgb*color.a,color.a),vec4(rgb*scale,color.a),premultiplied);
}
@fragment fn fs(v:Out)->@location(0) vec4<f32> { return shade(v); }
// Single-pass presentation: the same colour to the retained target and the drawable.
struct Both { @location(0) retained:vec4<f32>, @location(1) drawable:vec4<f32> }
@fragment fn fs_both(v:Out)->Both { let c=shade(v); return Both(c,c); }
// Plain quads (no corners, borders, shadows, fades, masks or transforms):
// most text and backgrounds. A software rasterizer runs a fragment shader on
// the CPU for every pixel, and every branch of `shade` with it, so these use
// this one where drawing is split (see `push_draw`). Same results as `shade`.
// `texture`: 0 for a flat colour, 1 for straight-alpha samples (glyphs), 2
// for premultiplied ones (icons, images), as `options.y`.
struct Basic { @builtin(position) position:vec4<f32>, @location(0) uv:vec2<f32>, @location(1) @interpolate(flat) color:vec4<f32>, @location(2) @interpolate(flat) texture:f32 }
@vertex fn vs_basic(@location(0) rect:vec4<f32>,@location(1) uv:vec4<f32>,@location(2) color:vec4<f32>,@location(4) options:vec4<f32>,@builtin(vertex_index) i:u32)->Basic {
 let corner=vec2(f32(i&1u),f32(i>>1u));
 let p=rect.xy+corner*rect.zw;
 var o:Basic; o.position=vec4(p.x/viewport.x*2.-1.,1.-p.y/viewport.y*2.,0.,1.);o.uv=uv.xy+corner*uv.zw;o.color=color;o.texture=options.y;return o;
}
// Straight samples: colour.rgb * tex.rgb * (colour.a * tex.a). Premultiplied
// ones already carry tex.a in tex.rgb: colour.rgb * tex.rgb * colour.a.
fn basic(v:Basic)->vec4<f32> {
 if v.texture>0.5 {let tex=textureSample(atlas,tex_sampler,v.uv); let a=v.color.a*tex.a; return vec4(v.color.rgb*tex.rgb*select(a,v.color.a,v.texture>1.5),a);}
 return vec4(v.color.rgb*v.color.a,v.color.a);
}
@fragment fn fs_basic(v:Basic)->@location(0) vec4<f32> { return basic(v); }
@fragment fn fs_basic_both(v:Basic)->Both { let c=basic(v); return Both(c,c); }
