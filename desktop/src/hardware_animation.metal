#include <metal_stdlib>
using namespace metal;

struct Side {
    float angle;
    float active;
    float wash_opacity;
    float padding;
    float color[4];
    float distances[24];
};

struct Leader {
    float points[6];
    float padding[2];
    float color[4];
};

struct Frame {
    float scale;
    float width;
    float height;
    float backing_scale;
    float stage_x;
    float stage_y;
    float padding[2];
    Side sides[2];
    Leader leaders[6];
};

struct RoutePoint {
    float x;
    float y;
    float distance;
    float padding;
};

struct Route {
    RoutePoint points[162];
    float length;
    float padding[3];
};

struct Particle {
    uint lane;
    float phase;
    float radius;
    float opacity;
};

struct Vertex {
    float4 position [[position]];
    float2 local;
    float opacity;
    float4 color;
};

constant float2 corners[4] = {
    float2(-1.0, -1.0), float2(1.0, -1.0),
    float2(-1.0, 1.0), float2(1.0, 1.0)
};

float4 stage_position(float2 point, uint side, constant Frame &frame) {
    if (side == 1) {
        point.x = 840.0 - point.x;
    }
    float2 pixel = float2(frame.stage_x, frame.stage_y) + point * frame.scale;
    return float4(pixel.x * 2.0 / frame.width - 1.0,
        1.0 - pixel.y * 2.0 / frame.height, 0.0, 1.0);
}

vertex Vertex rotor_vertex(uint vertex_id [[vertex_id]], uint side [[instance_id]],
    constant Frame &frame [[buffer(0)]]) {
    float2 corner = corners[vertex_id];
    float angle = frame.sides[side].angle;
    float cosine = cos(angle);
    float sine = sin(angle);
    float2 rotated = float2(cosine * corner.x - sine * corner.y,
        sine * corner.x + cosine * corner.y) * 52.0;
    Vertex result;
    // Mirroring the whole side after rotation preserves the original reflected highlight.
    result.position = stage_position(float2(183.0, 221.0) + rotated, side, frame);
    result.local = (corner + 1.0) * 0.5;
    result.opacity = frame.sides[side].wash_opacity;
    result.color = float4(frame.sides[side].color[0], frame.sides[side].color[1],
        frame.sides[side].color[2], 1.0);
    return result;
}

float segment_distance(float2 point, float2 start, float2 end) {
    float2 direction = end - start;
    float projection = clamp(dot(point - start, direction) / max(dot(direction, direction),
        0.000001), 0.0, 1.0);
    return length(point - (start + direction * projection));
}

float4 callout_foreground(float2 point, constant Frame &frame) {
    // Match hardware.rs: 1-point leaders at 0.65 opacity, and opaque radius-2 anchor circles.
    float edge = 0.5 / frame.backing_scale;
    float4 foreground = float4(0.0);
    for (uint index = 0; index < 6; ++index) {
        constant Leader &leader = frame.leaders[index];
        float2 anchor = float2(leader.points[0], leader.points[1]);
        float2 elbow = float2(leader.points[2], leader.points[3]);
        float2 port = float2(leader.points[4], leader.points[5]);
        float distance = min(segment_distance(point, anchor, elbow),
            segment_distance(point, elbow, port));
        float line = (1.0 - smoothstep(0.5 - edge, 0.5 + edge, distance)) * 0.65;
        float dot = 1.0 - smoothstep(2.0 - edge, 2.0 + edge, length(point - anchor));
        float alpha = dot + line * (1.0 - dot);
        float3 color = float3(leader.color[0], leader.color[1], leader.color[2]);
        foreground = float4(color * alpha, alpha) + foreground * (1.0 - alpha);
    }
    return foreground;
}

float2 logical_position(float2 physical, constant Frame &frame) {
    float2 logical = float2(frame.width, frame.height);
    return physical * logical / ceil(logical * frame.backing_scale);
}

float4 preserve_callouts(float4 sprite, float2 physical, constant Frame &frame) {
    float4 foreground = callout_foreground(logical_position(physical, frame), frame);
    // The GPUI surface already contains the leader over the chassis. Keep sprite alpha intact
    // so compositing reconstructs leader-over-sprite-over-chassis without holes or doubled lines.
    return float4(sprite.rgb * (1.0 - foreground.a) + foreground.rgb * sprite.a, sprite.a);
}

fragment float4 rotor_fragment(Vertex input [[stage_in]],
    texture2d<float> image [[texture(0)]], constant Frame &frame [[buffer(0)]]) {
    constexpr sampler filtering(coord::normalized, address::clamp_to_edge, filter::linear);
    float4 sprite = image.sample(filtering, input.local);
    float2 point = (logical_position(input.position.xy, frame)
        - float2(frame.stage_x, frame.stage_y)) / frame.scale;
    if (point.x > 420.0) { point.x = 840.0 - point.x; }
    point -= float2(183.0, 221.0);
    // The GPUI wash has reached its constant stop here, outside its 32-sided hub opening.
    float sector = M_PI_F / 16.0;
    float angle = atan2(point.y, point.x);
    float normal = floor(angle / sector) * sector + sector * 0.5;
    float distance = dot(point, float2(cos(normal), sin(normal))) - 29.0 * cos(sector * 0.5);
    float edge = 0.5 / (frame.scale * frame.backing_scale);
    float wash = smoothstep(-edge, edge, distance) * input.opacity;
    sprite.rgb = sprite.rgb * (1.0 - wash) + input.color.rgb * wash * sprite.a;
    return preserve_callouts(sprite, input.position.xy, frame);
}

vertex Vertex particle_vertex(uint vertex_id [[vertex_id]], uint instance [[instance_id]],
    constant Frame &frame [[buffer(0)]], device const Route *routes [[buffer(1)]],
    device const Particle *particles [[buffer(2)]]) {
    uint side = instance / 96;
    Particle particle = particles[instance % 96];
    device const Route &route = routes[particle.lane];
    float progress = fract(frame.sides[side].distances[particle.lane] / route.length
        + particle.phase);
    float distance = progress * route.length;
    uint lower = 0;
    uint upper = 161;
    for (uint step = 0; step < 9; ++step) {
        if (lower + 1 < upper) {
            uint middle = (lower + upper) / 2;
            if (route.points[middle].distance < distance) {
                lower = middle;
            } else {
                upper = middle;
            }
        }
    }
    RoutePoint first = route.points[lower];
    RoutePoint second = route.points[upper];
    float fraction = (distance - first.distance) / (second.distance - first.distance);
    float2 center = mix(float2(first.x, first.y), float2(second.x, second.y), fraction);
    float radius = particle.radius + 1.0 / (frame.scale * frame.backing_scale);
    Vertex result;
    result.position = stage_position(center + corners[vertex_id] * radius, side, frame);
    result.local = corners[vertex_id] * radius / particle.radius;
    result.opacity = clamp(min(progress / 0.06, (1.0 - progress) / 0.08), 0.0, 1.0)
        * particle.opacity * frame.sides[side].active;
    result.color = float4(frame.sides[side].color[0], frame.sides[side].color[1],
        frame.sides[side].color[2], frame.sides[side].color[3]);
    return result;
}

fragment float4 particle_fragment(Vertex input [[stage_in]], constant Frame &frame [[buffer(0)]]) {
    float distance = length(input.local);
    float edge = max(fwidth(distance), 0.0001);
    float alpha = (1.0 - smoothstep(1.0 - edge * 0.5, 1.0 + edge * 0.5, distance))
        * input.opacity;
    return preserve_callouts(float4(input.color.rgb * alpha, alpha), input.position.xy, frame);
}
