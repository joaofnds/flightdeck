use metal::{Buffer, DeviceRef, RenderPipelineState, Texture};

use super::routes;

pub(super) struct Resources {
    pub queue: metal::CommandQueue,
    pub rotor_pipeline: RenderPipelineState,
    pub particle_pipeline: RenderPipelineState,
    pub texture: Texture,
    pub routes: Buffer,
    pub particles: Buffer,
    pub extent: [f32; 4],
    pub lengths: [f32; 24],
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct RoutePoint {
    x: f32,
    y: f32,
    distance: f32,
    padding: f32,
}

#[repr(C)]
struct Route {
    points: [RoutePoint; 162],
    length: f32,
    padding: [f32; 3],
}

#[repr(C)]
struct Particle {
    lane: u32,
    phase: f32,
    radius: f32,
    opacity: f32,
}

struct PipelineStages<'a> {
    vertex: &'a str,
    fragment: &'a str,
}

const _: () = {
    assert!(size_of::<RoutePoint>() == 16);
    assert!(size_of::<Route>() == 2608);
    assert!(size_of::<Particle>() == 16);
};

pub(super) fn create(device: &DeviceRef) -> Result<Resources, String> {
    let options = metal::CompileOptions::new();
    options.set_fast_math_enabled(false);
    let library =
        device.new_library_with_source(include_str!("hardware_animation.metal"), &options)?;
    let routes = create_routes();
    let particles = create_particles();

    Ok(Resources {
        queue: device.new_command_queue_with_max_command_buffer_count(2),
        rotor_pipeline: create_pipeline(
            device,
            &library,
            &PipelineStages {
                vertex: "rotor_vertex",
                fragment: "rotor_fragment",
            },
        )?,
        particle_pipeline: create_pipeline(
            device,
            &library,
            &PipelineStages {
                vertex: "particle_vertex",
                fragment: "particle_fragment",
            },
        )?,
        texture: create_texture(device)?,
        routes: create_buffer(device, &routes)?,
        particles: create_buffer(device, &particles)?,
        extent: create_extent(&routes),
        lengths: std::array::from_fn(|index| routes[index].length),
    })
}

fn create_pipeline(
    device: &DeviceRef,
    library: &metal::LibraryRef,
    stages: &PipelineStages<'_>,
) -> Result<RenderPipelineState, String> {
    let vertex = library.get_function(stages.vertex, None)?;
    let fragment = library.get_function(stages.fragment, None)?;
    let descriptor = metal::RenderPipelineDescriptor::new();
    descriptor.set_vertex_function(Some(&vertex));
    descriptor.set_fragment_function(Some(&fragment));
    let attachment = descriptor
        .color_attachments()
        .object_at(0)
        .ok_or("Missing Metal pipeline color attachment")?;
    attachment.set_pixel_format(metal::MTLPixelFormat::BGRA8Unorm);
    attachment.set_blending_enabled(true);
    attachment.set_rgb_blend_operation(metal::MTLBlendOperation::Add);
    attachment.set_alpha_blend_operation(metal::MTLBlendOperation::Add);
    attachment.set_source_rgb_blend_factor(metal::MTLBlendFactor::One);
    attachment.set_source_alpha_blend_factor(metal::MTLBlendFactor::One);
    attachment.set_destination_rgb_blend_factor(metal::MTLBlendFactor::OneMinusSourceAlpha);
    attachment.set_destination_alpha_blend_factor(metal::MTLBlendFactor::OneMinusSourceAlpha);

    device.new_render_pipeline_state(&descriptor)
}

fn create_texture(device: &DeviceRef) -> Result<Texture, String> {
    let mut image = image::load_from_memory(include_bytes!("../resources/schematic_rotor.png"))
        .map_err(|error| format!("Cannot decode GPU rotor: {error}"))?
        .into_rgba8();
    if image.dimensions() != (312, 312) {
        return Err("GPU rotor must be the original 312 by 312 RGBA image".into());
    }

    for pixel in image.pixels_mut() {
        for channel in 0..3 {
            // Add half the divisor, rounded down, to round to the nearest integer.
            let premultiplied = (u16::from(pixel[channel]) * u16::from(pixel[3]) + 127) / 255;
            pixel[channel] = u8::try_from(premultiplied)
                .map_err(|error| format!("Rotor premultiplication: {error}"))?;
        }
    }

    let descriptor = metal::TextureDescriptor::new();
    descriptor.set_texture_type(metal::MTLTextureType::D2);
    descriptor.set_pixel_format(metal::MTLPixelFormat::RGBA8Unorm);
    descriptor.set_width(312);
    descriptor.set_height(312);
    descriptor.set_depth(1);
    descriptor.set_mipmap_level_count(1);
    descriptor.set_sample_count(1);
    descriptor.set_array_length(1);
    descriptor.set_storage_mode(metal::MTLStorageMode::Shared);
    descriptor.set_usage(metal::MTLTextureUsage::ShaderRead);
    let texture = device.new_texture(&descriptor);
    texture.replace_region(
        metal::MTLRegion::new_2d(0, 0, 312, 312),
        0,
        image.as_raw().as_ptr().cast(),
        312 * 4,
    );

    Ok(texture)
}

fn create_routes() -> Vec<Route> {
    let source = routes::create();
    assert_eq!(source.len(), 24);
    let mut routes = Vec::with_capacity(24);
    create_routes_initialize(&mut routes);
    for (uploaded, route) in routes.iter_mut().zip(&source) {
        assert_eq!(route.points.len(), 162);
        uploaded.length = route.length;
        let mut distance = 0.0;
        for (index, point) in route.points.iter().enumerate() {
            assert!(point.x.is_finite());
            assert!(point.y.is_finite());
            if index > 0 {
                let previous = route.points[index - 1];
                let segment = (point.x - previous.x).hypot(point.y - previous.y);
                assert!(segment > 0.0);
                distance += segment;
            }
            uploaded.points[index] = RoutePoint {
                x: point.x,
                y: point.y,
                distance,
                padding: 0.0,
            };
        }
        assert!(route.length > 0.0);
        assert!(route.length.is_finite());
        assert!((distance - route.length).abs() < 0.01);
    }

    routes
}

fn create_routes_initialize(routes: &mut Vec<Route>) {
    assert!(routes.is_empty());
    assert!(routes.capacity() >= 24);
    // Every Route field is f32, so zero initializes valid values and padding directly in the
    // startup allocation. No Route-sized value passes through a constructor or Vec::push.
    unsafe {
        routes.as_mut_ptr().write_bytes(0, 24);
        routes.set_len(24);
    }
}

fn create_particles() -> Vec<Particle> {
    let mut particles = Vec::with_capacity(96);
    for lane in 0..24_u16 {
        for phase in 0..4_u16 {
            let offset = (f32::from(phase) + (f32::from(lane) * 0.618_034).fract()) / 4.0;
            let variation = (f32::from(lane) * 0.618_034 + f32::from(phase) * 0.381_966).fract();
            particles.push(Particle {
                lane: u32::from(lane),
                phase: offset,
                radius: 0.65 + variation * 0.45,
                opacity: 0.36 + variation * 0.24,
            });
        }
    }
    assert_eq!(particles.len(), 96);

    particles
}

fn create_buffer<T>(device: &DeviceRef, values: &[T]) -> Result<Buffer, String> {
    let length = u64::try_from(size_of_val(values)).map_err(|error| error.to_string())?;
    assert!(length > 0);

    Ok(device.new_buffer_with_data(
        values.as_ptr().cast(),
        length,
        metal::MTLResourceOptions::StorageModeShared,
    ))
}

fn create_extent(routes: &[Route]) -> [f32; 4] {
    assert_eq!(routes.len(), 24);
    // The swept square must include its corners at every rotation, not just its diameter.
    let swept_radius = 52.0 * std::f32::consts::SQRT_2;
    let mut left = 183.0 - swept_radius;
    let mut right = 183.0 + swept_radius;
    let mut top = 221.0 - swept_radius;
    let mut bottom = 221.0 + swept_radius;
    for route in routes {
        for point in &route.points {
            left = left.min(point.x);
            right = right.max(point.x);
            top = top.min(point.y);
            bottom = bottom.max(point.y);
        }
    }
    let mirrored_left = left.min(840.0 - right) - 3.0;
    let mirrored_right = right.max(840.0 - left) + 3.0;

    [
        mirrored_left,
        top - 3.0,
        mirrored_right - mirrored_left,
        bottom - top + 6.0,
    ]
}
