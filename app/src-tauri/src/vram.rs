//! Pre-flight VRAM estimate.
//!
//! This exists to turn a confusing CUDA OOM halfway through model loading into a
//! warning shown before anything starts. It is deliberately a rough model, not a
//! simulation -- the numbers come from the published measurements for Ternary
//! Bonsai 27B: ~8.4 GB peak at 4K context with an f16 cache, ~8.7 GB at 10K and
//! ~14.7 GB at 100K, which works out to roughly 62 MiB per additional 1K tokens.

use serde::Serialize;

const BASE_MIB: u64 = 8000;
const PER_1K_MIB: u64 = 62;
const BASE_CTX: u32 = 4096;
const MMPROJ_MIB: u64 = 600;

#[derive(Debug, Clone, Serialize)]
pub struct VramEstimate {
    /// None when there is no NVIDIA GPU to ask.
    pub free_mib: Option<u64>,
    pub needed_mib: u64,
    pub warning: Option<String>,
}

fn kv_divisor(kv_type: &str) -> u64 {
    match kv_type {
        "q8_0" => 2,
        "q4_0" | "q4_1" => 4,
        _ => 1,
    }
}

/// Free VRAM on device 0, via nvidia-smi. None if it is absent or unhappy --
/// plenty of valid setups (Vulkan on AMD, CPU-only, macOS) have no nvidia-smi,
/// and none of them should produce a scary warning.
pub fn free_mib() -> Option<u64> {
    let out = std::process::Command::new("nvidia-smi")
        .args(["--query-gpu=memory.free", "--format=csv,noheader,nounits"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    String::from_utf8_lossy(&out.stdout).lines().next()?.trim().parse().ok()
}

pub fn estimate(ctx: u32, kv_type: &str, ngl: u32, mmproj_cpu: bool, kv_offload: bool) -> VramEstimate {
    // Nothing on the GPU means nothing to run out of.
    if ngl == 0 {
        return VramEstimate { free_mib: free_mib(), needed_mib: 0, warning: None };
    }

    let needed_mib = if !kv_offload {
        // The cache lives in system RAM; only the weights sit on the card.
        BASE_MIB
    } else {
        let extra_ctx = ctx.saturating_sub(BASE_CTX) as u64;
        let cache = extra_ctx * PER_1K_MIB / 1000 / kv_divisor(kv_type);
        BASE_MIB + cache + if mmproj_cpu { 0 } else { MMPROJ_MIB }
    };

    let free = free_mib();
    let warning = match free {
        Some(free) if needed_mib > free => Some(format!(
            "Estimated peak is about {needed_mib} MiB of VRAM, but only {free} MiB is free. \
             Try a 4-bit KV cache, a smaller context, keeping the vision tower in system RAM, \
             or moving the KV cache to system RAM."
        )),
        _ => None,
    };

    VramEstimate { free_mib: free, needed_mib, warning }
}
