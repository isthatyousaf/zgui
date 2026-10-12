// `area`: the filter's visible pixels (x0, y0, x1, y1). Like CSS backdrop
// filters, samples never leave it: content outside a frosted panel does not
// bleed into its edge, and changes there never make it re-blur.
// `shape`: corner radii (top-left, top-right, bottom-right, bottom-left); a
// rounded panel blurs only inside its curve, antialiased over one pixel.
struct Params { direction:vec4<f32>, bounds:vec4<f32>, effect:vec4<f32>, area:vec4<f32>, shape:vec4<f32> }
@group(0) @binding(0) var source:texture_2d<f32>;
@group(0) @binding(1) var original:texture_2d<f32>;
@group(0) @binding(2) var<uniform> params:Params;
@vertex fn vs(@builtin(vertex_index) i:u32)->@builtin(position) vec4<f32>{let p=vec2(f32((i<<1u)&2u),f32(i&2u))*2.-1.;return vec4(p,0.,1.);}
fn along(p:vec4<f32>)->vec4<f32>{
 let lo=vec2<i32>(params.area.xy);let hi=vec2<i32>(params.area.zw)-vec2(1);let pixel=vec2<i32>(p.xy);let sigma=max(params.direction.z,0.1);let radius=i32(ceil(sigma*3.));var sum=vec4(0.);var weight=0.;
 for(var i=-radius;i<=radius;i++){let w=exp(-f32(i*i)/(2.*sigma*sigma));let coord=clamp(pixel+vec2<i32>(params.direction.xy)*i,lo,hi);sum+=textureLoad(source,coord,0)*w;weight+=w;}
 return sum/weight;
}
fn coverage(p:vec2<f32>)->f32{
 let half=params.bounds.zw*.5;let q=p-params.bounds.xy-half;
 let r=select(select(params.shape.w,params.shape.z,q.x>0.),select(params.shape.x,params.shape.y,q.x>0.),q.y<0.);
 if r<=0.{return 1.;}
 let d=abs(q)-half+vec2(r);let sdf=length(max(d,vec2(0.)))+min(max(d.x,d.y),0.)-r;
 return clamp(.5-sdf,0.,1.);
}
// Vertical pass: blur the horizontal result and mix it over the original.
fn vertical(p:vec4<f32>)->vec4<f32>{
 let blurred=along(p);if params.direction.w<0.5{return blurred;}
 var amount=params.effect.x;if params.effect.y>0.{let d=min(p.xy-params.bounds.xy,params.bounds.xy+params.bounds.zw-p.xy);amount*=clamp(min(d.x,d.y)/params.effect.y,0.,1.);}
 amount*=coverage(p.xy);
 return mix(textureLoad(original,vec2<i32>(p.xy),0),blurred,amount);
}
@fragment fn fs(@builtin(position) p:vec4<f32>)->@location(0) vec4<f32>{return vertical(p);}
// Horizontal pass straight from the target: writes the horizontal blur and
// keeps the original pixels for the vertical mix, so no separate copy pass.
struct Split { @location(0) blurred:vec4<f32>, @location(1) original:vec4<f32> }
@fragment fn fs_split(@builtin(position) p:vec4<f32>)->Split{
 return Split(along(p),textureLoad(source,vec2<i32>(p.xy),0));
}
// Vertical pass drawn at presentation: the retained target and the drawable.
struct Both { @location(0) retained:vec4<f32>, @location(1) drawable:vec4<f32> }
@fragment fn fs_both(@builtin(position) p:vec4<f32>)->Both{
 let color=vertical(p);return Both(color,color);
}
