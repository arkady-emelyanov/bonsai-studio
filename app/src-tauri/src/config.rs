//! Settings model, persistence, and the mapping from settings to llama-server flags.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Default API port. Deliberately not llama.cpp's 8080 (contested) and not
/// Ollama's 11434 (collides). 11435 is Ollama+1: memorable and adjacent, at the
/// cost of overlapping the conventional port for a second Ollama instance --
/// which is why `Supervisor::start` names that case explicitly when the bind
/// fails.
pub const DEFAULT_PORT: u16 = 11435;

/// Files that make up a model. Both are required: the 27B takes image input, and
/// the projector is not optional the way the FP16 reference weights are.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ModelFiles {
    pub weights: String,
    pub mmproj: String,
}

/// A profile bundles the three settings that have to move together. Splitting
/// them into independent controls invites combinations that load and then OOM
/// partway through a long prompt.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Profile {
    /// 16K, f16 cache, projector on the GPU. ~9 GB peak.
    Default,
    /// 64K, 4-bit cache, projector in system RAM. ~9.4 GB peak.
    Ctx64k,
    /// 100K, 4-bit cache, projector in system RAM. ~10.1 GB peak.
    Ctx100k,
    /// The full 262144-token window at 4-bit. ~12.8 GB peak; wants a 16 GiB card.
    Max,
    /// Whatever ctx/kv/projector values are currently in Settings.
    Custom,
}

impl Profile {
    /// (context, kv cache type, keep projector off the GPU)
    pub fn params(self) -> Option<(u32, &'static str, bool)> {
        match self {
            Profile::Default => Some((16384, "f16", false)),
            Profile::Ctx64k => Some((65536, "q4_0", true)),
            Profile::Ctx100k => Some((102400, "q4_0", true)),
            Profile::Max => Some((262144, "q4_0", true)),
            Profile::Custom => None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Settings {
    // --- model ---
    pub model_dir: String,
    pub model: Option<ModelFiles>,
    pub profile: Profile,
    pub ctx: u32,
    pub kv_type: String,
    pub ngl: u32,
    pub mmproj_cpu: bool,
    pub kv_offload: bool,

    // --- api ---
    pub port: u16,
    /// Bind 0.0.0.0 instead of 127.0.0.1. Loopback already reaches every app on
    /// this machine, so this is only for other devices on the network.
    pub bind_lan: bool,
    pub cors_origins: String,
    pub api_key: String,

    // --- sampling ---
    pub temp: f32,
    pub top_p: f32,
    pub top_k: u32,
    pub min_p: f32,

    /// Override for the llama-server binary. Empty means "resolve it".
    pub server_path: String,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            model_dir: default_model_dir().to_string_lossy().into_owned(),
            model: None,
            profile: Profile::Ctx64k,
            ctx: 65536,
            kv_type: "q4_0".into(),
            ngl: 99,
            mmproj_cpu: true,
            kv_offload: true,
            port: DEFAULT_PORT,
            bind_lan: false,
            // Permissive on loopback: this is a local development tool, and the
            // browser-facing chat UI is served from the same origin anyway.
            // Flipping bind_lan is where the stakes change -- see validate().
            cors_origins: "*".into(),
            api_key: String::new(),
            temp: 0.5,
            top_p: 0.85,
            top_k: 20,
            min_p: 0.0,
            server_path: String::new(),
        }
    }
}

impl Settings {
    /// Push profile values into the flat fields, so the UI and the flag builder
    /// only ever read one source of truth.
    pub fn apply_profile(&mut self) {
        if let Some((ctx, kv, mmproj_cpu)) = self.profile.params() {
            self.ctx = ctx;
            self.kv_type = kv.to_string();
            self.mmproj_cpu = mmproj_cpu;
        }
    }

    pub fn host(&self) -> &'static str {
        if self.bind_lan {
            "0.0.0.0"
        } else {
            "127.0.0.1"
        }
    }

    /// Reasons the settings cannot be launched as-is.
    pub fn validate(&self) -> Result<&ModelFiles, String> {
        let model = self
            .model
            .as_ref()
            .ok_or("No model selected. Download one or point the app at an existing GGUF file.")?;

        for path in [&model.weights, &model.mmproj] {
            if !Path::new(path).is_file() {
                return Err(format!("Missing model file: {path}"));
            }
        }

        // Unauthenticated on loopback is defensible; unauthenticated on 0.0.0.0
        // hands an unthrottled model to the whole network.
        if self.bind_lan && self.api_key.trim().is_empty() {
            return Err(
                "Network access is on but no API key is set. Generate a key, or switch \
                 the bind scope back to This PC."
                    .into(),
            );
        }

        if self.ctx == 0 {
            return Err("Context size must be greater than zero.".into());
        }

        Ok(model)
    }

    /// The full llama-server argument list.
    pub fn to_args(&self) -> Result<Vec<String>, String> {
        let model = self.validate()?;
        let mut args: Vec<String> = vec![
            "-m".into(),
            model.weights.clone(),
            "--mmproj".into(),
            model.mmproj.clone(),
            "-c".into(),
            self.ctx.to_string(),
            "-ngl".into(),
            self.ngl.to_string(),
            "--cache-type-k".into(),
            self.kv_type.clone(),
            "--cache-type-v".into(),
            self.kv_type.clone(),
            // A quantized V cache requires flash attention.
            "-fa".into(),
            if self.kv_type == "f16" { "auto".into() } else { "on".into() },
            // Native OpenAI-style tool_calls.
            "--jinja".into(),
            "--host".into(),
            self.host().into(),
            "--port".into(),
            self.port.to_string(),
            "--alias".into(),
            "bonsai-27b".into(),
            "--temp".into(),
            self.temp.to_string(),
            "--top-p".into(),
            self.top_p.to_string(),
            "--top-k".into(),
            self.top_k.to_string(),
            "--min-p".into(),
            self.min_p.to_string(),
        ];

        if self.mmproj_cpu {
            args.push("--no-mmproj-offload".into());
        }
        if !self.kv_offload {
            args.push("--no-kv-offload".into());
        }
        if !self.cors_origins.trim().is_empty() {
            args.push("--cors-origins".into());
            args.push(self.cors_origins.trim().into());
        }
        if !self.api_key.trim().is_empty() {
            args.push("--api-key".into());
            args.push(self.api_key.trim().into());
        }

        Ok(args)
    }
}

// --- paths -------------------------------------------------------------------

fn home() -> PathBuf {
    std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."))
}

/// XDG data dir: models are large data, not configuration.
pub fn default_model_dir() -> PathBuf {
    std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home().join(".local/share"))
        .join("bonsai-studio/models")
}

pub fn config_path() -> PathBuf {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home().join(".config"))
        .join("bonsai-studio/settings.json")
}

pub fn load() -> Settings {
    let path = config_path();
    let Ok(text) = std::fs::read_to_string(&path) else {
        return Settings::default();
    };
    // A settings file from a newer version, or a hand-edit with a typo, should
    // not brick the app -- fall back rather than refusing to start.
    serde_json::from_str(&text).unwrap_or_else(|e| {
        eprintln!("settings: ignoring {}: {e}", path.display());
        Settings::default()
    })
}

pub fn save(settings: &Settings) -> Result<(), String> {
    let path = config_path();
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    let text = serde_json::to_string_pretty(settings).map_err(|e| e.to_string())?;
    std::fs::write(&path, text).map_err(|e| format!("{}: {e}", path.display()))
}
