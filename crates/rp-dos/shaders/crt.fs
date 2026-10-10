#version 330

// CRT post-process for the DOS screen. GLSL port of
// an earlier Bevy project's crt.wgsl for raylib's default vertex
// shader: draw the DOS screen texture over the tube rect with this shader
// active. Mirrored in Rust by src/crt.rs; keep the two in sync.

in vec2 fragTexCoord;
in vec4 fragColor;

uniform sampler2D texture0;
uniform vec4 colDiffuse;

// DOS screen size in pixels (200x320 or 320x200).
uniform vec2 resolution;
uniform float curvature;
uniform float scanlineIntensity;
uniform float time;
uniform float flicker;
// Per-channel tint; white leaves CGA colors unchanged.
uniform vec3 phosphor;

out vec4 finalColor;

const float PI = 3.14159265;

// Barrel-distort a UV in [0,1]. Each axis is pushed out by the squared
// distance along the other axis, which rounds the corners like a real tube.
// Results outside [0,1] are bezel.
vec2 curve_uv(vec2 uv, float k) {
    vec2 cuv = uv * 2.0 - 1.0;
    vec2 offset = abs(cuv.yx) * k;
    cuv = cuv + cuv * offset * offset;
    return cuv * 0.5 + 0.5;
}

void main() {
    vec2 curved = curve_uv(fragTexCoord, curvature);

    // Black bezel where the tube curves out of view.
    if (curved.x < 0.0 || curved.x > 1.0 || curved.y < 0.0 || curved.y > 1.0) {
        finalColor = vec4(0.0, 0.0, 0.0, 1.0);
        return;
    }

    vec3 color = texture(texture0, curved).rgb * phosphor;

    // One scanline per DOS screen row: full brightness at the row center,
    // darkest on the boundary between rows.
    float line = cos(2.0 * PI * curved.y * resolution.y);
    color *= 1.0 - scanlineIntensity * (0.5 + 0.5 * line);

    // Subtle mains-hum flicker.
    color *= 1.0 - flicker * (0.5 + 0.5 * sin(time * 100.0));

    // Vignette toward the curved edges.
    vec2 edge = curved * (1.0 - curved.yx);
    color *= clamp(pow(edge.x * edge.y * 40.0, 0.25), 0.0, 1.0);

    finalColor = vec4(color, 1.0);
}
