struct VIn {
    @location(0) xy: vec2<f32>,
    @location(1) uv: vec2<f32>,
    @location(2) tint: vec4<f32>,
};
struct VOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) tint: vec4<f32>,
};
@group(0) @binding(0) var image: texture_2d<f32>;
@group(0) @binding(1) var source_sampler: sampler;

@vertex
fn vs_main(v: VIn) -> VOut {
    var out: VOut;
    out.clip = vec4<f32>(v.xy, 0.0, 1.0);
    out.uv = v.uv;
    out.tint = v.tint;
    return out;
}
@fragment
fn fs_main(v: VOut) -> @location(0) vec4<f32> {
    return textureSample(image, source_sampler, v.uv) * v.tint;
}
