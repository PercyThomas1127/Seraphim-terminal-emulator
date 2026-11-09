//! Choosing a GPU and a graphics API.
//!
//! * **API:** Vulkan (Metal, DX12 elsewhere) is tried first; OpenGL is
//!   started only when no native adapter fits. Starting OpenGL as well
//!   costs about 40 ms at startup even when it isn't used. `WGPU_BACKEND`
//!   (wgpu's own variable, e.g. `gl`) overrides this.
//! * **GPU:** machines with more than one (a laptop's integrated and
//!   discrete GPUs, or two cards in a desktop) pick one with
//!   `SERAPHIM_GPU`, any part of its name: `2060`, `radeon`, `intel`; case
//!   does not matter. `seraphim bench --list-gpus` lists them. Without it,
//!   wgpu picks (the low-power adapter).

/// Environment variable naming the GPU to use.
pub const ENV: &str = "SERAPHIM_GPU";

/// `SERAPHIM_GPU`, if set.
pub fn from_env() -> Option<String> {
    std::env::var(ENV).ok().filter(|v| !v.trim().is_empty())
}

/// Graphics APIs to try, in order.
pub fn tiers() -> Vec<wgpu::Backends> {
    match wgpu::Backends::from_env() {
        Some(b) => vec![b],
        None => vec![wgpu::Backends::PRIMARY, wgpu::Backends::GL],
    }
}

/// Run `attempt` with each tier of [`tiers`] until one succeeds; the last
/// error otherwise.
pub fn first_tier<R>(
    mut attempt: impl FnMut(wgpu::Backends) -> Result<R, String>,
) -> Result<R, String> {
    let mut last = String::from("no graphics API is enabled (check WGPU_BACKEND)");
    for b in tiers() {
        match attempt(b) {
            Ok(r) => return Ok(r),
            Err(e) => last = e,
        }
    }
    Err(last)
}

fn type_rank(t: wgpu::DeviceType) -> u8 {
    match t {
        wgpu::DeviceType::DiscreteGpu => 0,
        wgpu::DeviceType::IntegratedGpu => 1,
        wgpu::DeviceType::VirtualGpu => 2,
        wgpu::DeviceType::Other => 3,
        wgpu::DeviceType::Cpu => 4,
    }
}

/// The adapters `instance` offers, hardware before software.
pub fn adapters(instance: &wgpu::Instance) -> Vec<wgpu::Adapter> {
    let mut all = pollster::block_on(instance.enumerate_adapters(wgpu::Backends::all()));
    all.sort_by_key(|a| {
        let i = a.get_info();
        (type_rank(i.device_type), i.name.clone())
    });
    all
}

/// One line describing an adapter.
pub fn describe(info: &wgpu::AdapterInfo) -> String {
    let driver = [info.driver.as_str(), info.driver_info.as_str()]
        .iter()
        .filter(|s| !s.is_empty())
        .copied()
        .collect::<Vec<_>>()
        .join(" ");
    format!("{} ({:?}, {:?}, driver {})", info.name, info.backend, info.device_type, driver)
}

/// The adapter whose name contains `wanted` (case-insensitive), among those
/// that can draw to `surface` if given. `None` (or an empty string) lets
/// wgpu choose.
pub fn choose(
    instance: &wgpu::Instance,
    surface: Option<&wgpu::Surface<'_>>,
    wanted: Option<&str>,
) -> Result<wgpu::Adapter, String> {
    if let Some(w) = wanted.map(str::trim).filter(|w| !w.is_empty()) {
        let needle = w.to_lowercase();
        let found = adapters(instance)
            .into_iter()
            .find(|a| a.get_info().name.to_lowercase().contains(&needle));
        return match found {
            Some(a) if surface.is_none_or(|s| a.is_surface_supported(s)) => Ok(a),
            Some(a) => Err(format!("{ENV}={w}: {} cannot draw to this window", a.get_info().name)),
            None => {
                Err(format!("{ENV}={w}: no such GPU (`seraphim bench --list-gpus` lists them)"))
            }
        };
    }
    pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::LowPower,
        force_fallback_adapter: false,
        compatible_surface: surface,
        ..Default::default()
    }))
    .map_err(|e| format!("no usable GPU adapter: {e}"))
}

/// A windowless instance for `backends`.
pub fn headless_instance(backends: wgpu::Backends) -> wgpu::Instance {
    let mut desc = wgpu::InstanceDescriptor::new_without_display_handle_from_env();
    desc.backends = backends;
    wgpu::Instance::new(desc)
}

/// `wanted` (see [`choose`]) through the API tiers, without a window.
pub fn choose_headless(wanted: Option<&str>) -> Result<(wgpu::Instance, wgpu::Adapter), String> {
    first_tier(|backends| {
        let instance = headless_instance(backends);
        let adapter = choose(&instance, None, wanted)?;
        Ok((instance, adapter))
    })
}

/// Every GPU through every API, for `--list-gpus`.
pub fn list() -> Vec<wgpu::AdapterInfo> {
    let mut out: Vec<wgpu::AdapterInfo> = Vec::new();
    for b in tiers() {
        out.extend(adapters(&headless_instance(b)).iter().map(|a| a.get_info()));
    }
    out
}
