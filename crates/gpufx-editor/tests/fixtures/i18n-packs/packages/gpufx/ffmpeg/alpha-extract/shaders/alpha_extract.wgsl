@compute @workgroup_size(8, 8)
fn main(@builtin(global_invocation_id) id: vec3u) {
    if (!gpufx_in_bounds(id)) { return; }
    // Same size as the output: read the texel directly, exact on every GPU.
    let a = textureLoad(src, vec2i(id.xy), 0).a;
    let g = select(a, 1.0 - a, params.invert != 0u);
    gpufx_write(id, vec4f(vec3f(g), 1.0));
}
