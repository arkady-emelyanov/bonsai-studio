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

/// The sampling half of a workload profile.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Sampling {
    pub temp: f32,
    pub top_p: f32,
    pub top_k: u32,
    pub min_p: f32,
    pub presence_penalty: f32,
}

/// Whether the chat template is asked to produce a thinking block.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Reasoning {
    /// Let the template decide, which is what the model was tuned for.
    Auto,
    On,
    /// Skip thinking entirely. Much faster, and much worse on anything that
    /// needs several steps.
    Off,
}

/// How the model should think and sample for a kind of work.
///
/// Sampling and reasoning are one decision, not two. Chat wants a high
/// temperature and an unbounded chain; a structured single-action request wants
/// the opposite on both counts, and setting one without the other produces the
/// worst of each -- a long deliberation sampled too loosely to commit, or a
/// sharp distribution with nothing behind it. They travel together for the same
/// reason `Profile` bundles context with cache type: the combinations that are
/// wrong are reachable only by moving one without the other.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Workload {
    pub sampling: Sampling,
    pub reasoning: Reasoning,
    /// Token ceiling for the thinking block; -1 leaves it unrestricted.
    ///
    /// This is the only setting that bounds deliberation directly. Temperature
    /// and top-p shape what gets produced; the budget bounds how much.
    pub reasoning_budget: i32,
    /// Injected before the end-of-thinking tag when the budget runs out. Empty
    /// means the flag is left off, and the chain is simply cut.
    pub reasoning_budget_message: String,
}

/// The card's thinking-mode values, with deliberation left unbounded. Reasoning
/// chains need the exploration a high temperature buys; sharpening the
/// distribution collapses them early.
pub fn workload_chat() -> Workload {
    Workload {
        sampling: Sampling {
            temp: 1.0,
            top_p: 0.95,
            top_k: 20,
            min_p: 0.0,
            presence_penalty: 0.0,
        },
        reasoning: Reasoning::Auto,
        reasoning_budget: -1,
        reasoning_budget_message: String::new(),
    }
}

/// The card's instruct values. Thinking is off rather than merely discouraged:
/// these numbers are the ones the card gives for non-thinking use, so leaving
/// the chain on would pair them with the thing they were not measured with.
pub fn workload_instruct() -> Workload {
    Workload {
        sampling: Sampling {
            temp: 0.7,
            top_p: 0.80,
            top_k: 20,
            min_p: 0.0,
            presence_penalty: 1.5,
        },
        reasoning: Reasoning::Off,
        reasoning_budget: -1,
        reasoning_budget_message: String::new(),
    }
}

/// Many short, structured, independent decisions -- tool calls, agent turns, a
/// schema-constrained answer per request.
///
/// Thinking stays on, because it is what makes the decisions good, but it is
/// bounded: unbounded chat defaults spend thousands of tokens deliberating
/// before a forty-character answer, and the deliberation is then discarded. The
/// budget is the lever that bounds it.
///
/// The presence penalty is deliberately 0.0 rather than the instruct set's 1.5.
/// A schema-constrained reply must repeat tokens -- field names, braces, quotes
/// -- and a presence penalty pushes against exactly that, which shows up as
/// rare malformed JSON rather than as an obvious failure.
///
/// The numbers are a starting point. 512 is a first guess at where deliberation
/// stops paying; if answers degrade, the question is where the knee is, not
/// whether bounding works.
pub fn workload_agent() -> Workload {
    Workload {
        sampling: Sampling {
            temp: 0.6,
            top_p: 0.85,
            top_k: 20,
            min_p: 0.0,
            presence_penalty: 0.0,
        },
        reasoning: Reasoning::Auto,
        reasoning_budget: 512,
        // Being cut off mid-thought can leave the model unable to close the
        // object it was asked for, so the budget ends with an instruction to
        // land rather than with silence.
        reasoning_budget_message: "Budget reached. State your final answer now.".into(),
    }
}

/// A workload the user named and saved, shown alongside the built-ins.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct WorkloadPreset {
    /// Stable key the picker stores in `workload_profile`.
    pub id: String,
    pub name: String,
    pub values: Workload,
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

    // --- workload ---
    /// Which entry the picker is on: "chat", "instruct", "agent", "custom", or
    /// the id of one of `workload_presets`.
    pub workload_profile: String,
    /// The values actually passed to llama-server. A named profile pushes its
    /// numbers in here, so the flag builder reads one source of truth -- the
    /// same arrangement as `profile` and `ctx`/`kv_type`.
    pub workload: Workload,
    /// Workloads the user saved. Built-ins are code, not data, so they cannot
    /// be deleted or drift from the card.
    pub workload_presets: Vec<WorkloadPreset>,

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
            // Chat is what an app with a chat button opens on.
            workload_profile: "chat".into(),
            workload: workload_chat(),
            workload_presets: Vec::new(),
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
        self.apply_workload_profile();
    }

    /// The workload counterpart: a named profile pushes its numbers into
    /// `workload`, "custom" leaves whatever is there alone.
    ///
    /// An id that matches nothing -- a preset deleted on another machine, or a
    /// settings file from a newer build -- falls back to leaving the values
    /// untouched rather than resetting them, so the numbers the user last ran
    /// survive an unknown label.
    pub fn apply_workload_profile(&mut self) {
        let found = match self.workload_profile.as_str() {
            "chat" => Some(workload_chat()),
            "instruct" => Some(workload_instruct()),
            "agent" => Some(workload_agent()),
            "custom" => None,
            id => self
                .workload_presets
                .iter()
                .find(|p| p.id == id)
                .map(|p| p.values.clone()),
        };
        if let Some(values) = found {
            self.workload = values;
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
            self.workload.sampling.temp.to_string(),
            "--top-p".into(),
            self.workload.sampling.top_p.to_string(),
            "--top-k".into(),
            self.workload.sampling.top_k.to_string(),
            "--min-p".into(),
            self.workload.sampling.min_p.to_string(),
            "--presence-penalty".into(),
            self.workload.sampling.presence_penalty.to_string(),
            "--reasoning".into(),
            match self.workload.reasoning {
                Reasoning::Auto => "auto".into(),
                Reasoning::On => "on".into(),
                Reasoning::Off => "off".into(),
            },
        ];

        // -1 is the server's own default; passing it changes nothing, so only
        // a real ceiling is worth a flag.
        if self.workload.reasoning_budget >= 0 {
            args.push("--reasoning-budget".into());
            args.push(self.workload.reasoning_budget.to_string());

            // Only meaningful alongside a budget: with none, it is never shown.
            let message = self.workload.reasoning_budget_message.trim();
            if !message.is_empty() {
                args.push("--reasoning-budget-message".into());
                args.push(message.into());
            }
        }

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

#[cfg(test)]
mod tests {
    use super::*;

    /// A Settings pointing at two real files, so `to_args` gets past validate().
    fn settings_with_model(tag: &str) -> Settings {
        let dir = std::env::temp_dir().join(format!("bonsai-cfg-{tag}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let weights = dir.join("w.gguf");
        let mmproj = dir.join("m.gguf");
        std::fs::write(&weights, b"x").unwrap();
        std::fs::write(&mmproj, b"x").unwrap();
        Settings {
            model: Some(ModelFiles {
                weights: weights.to_string_lossy().into_owned(),
                mmproj: mmproj.to_string_lossy().into_owned(),
            }),
            ..Default::default()
        }
    }

    fn flag<'a>(args: &'a [String], name: &str) -> Option<&'a str> {
        let i = args.iter().position(|a| a == name)?;
        args.get(i + 1).map(|s| s.as_str())
    }

    #[test]
    fn default_workload_is_chat() {
        let s = Settings::default();
        assert_eq!(s.workload_profile, "chat");
        assert_eq!(s.workload, workload_chat());
        assert_eq!(s.workload.sampling.temp, 1.0);
        assert_eq!(s.workload.reasoning_budget, -1);
    }

    #[test]
    fn named_profile_pushes_its_values() {
        let mut s = Settings::default();
        s.workload_profile = "instruct".into();
        s.apply_profile();
        assert_eq!(s.workload, workload_instruct());
        assert_eq!(s.workload.sampling.presence_penalty, 1.5);
    }

    /// The point of bundling: picking a workload moves sampling and reasoning
    /// together, so neither can be left behind on the other's values.
    #[test]
    fn workload_carries_sampling_and_reasoning_together() {
        let mut s = Settings::default();
        s.workload_profile = "agent".into();
        s.apply_profile();
        assert_eq!(s.workload.sampling.temp, 0.6);
        assert_eq!(s.workload.reasoning, Reasoning::Auto);
        assert_eq!(s.workload.reasoning_budget, 512);
    }

    /// A schema-constrained answer has to repeat field names and punctuation,
    /// which a presence penalty pushes against.
    #[test]
    fn agent_does_not_inherit_instructs_presence_penalty() {
        assert_eq!(workload_agent().sampling.presence_penalty, 0.0);
        assert_eq!(workload_instruct().sampling.presence_penalty, 1.5);
    }

    /// The card's non-thinking numbers belong with thinking actually off.
    #[test]
    fn instruct_turns_thinking_off() {
        assert_eq!(workload_instruct().reasoning, Reasoning::Off);
        assert_eq!(workload_chat().reasoning, Reasoning::Auto);
    }

    #[test]
    fn custom_keeps_whatever_is_there() {
        let mut s = Settings::default();
        s.workload_profile = "custom".into();
        s.workload.sampling.temp = 0.33;
        s.workload.reasoning_budget = 77;
        s.apply_profile();
        assert_eq!(s.workload.sampling.temp, 0.33);
        assert_eq!(s.workload.reasoning_budget, 77);
    }

    /// A saved preset carries the reasoning half too, so a tuned agent setup
    /// survives being named.
    #[test]
    fn saved_preset_round_trips_reasoning() {
        let mut s = Settings::default();
        let mut values = workload_agent();
        values.reasoning_budget = 1024;
        s.workload_presets.push(WorkloadPreset {
            id: "mine".into(),
            name: "Mine".into(),
            values: values.clone(),
        });
        s.workload_profile = "mine".into();
        s.apply_profile();
        assert_eq!(s.workload, values);
        assert_eq!(s.workload.reasoning_budget, 1024);
    }

    /// A preset deleted elsewhere must not silently reset the numbers the user
    /// is running.
    #[test]
    fn unknown_profile_leaves_values_alone() {
        let mut s = Settings::default();
        s.workload_profile = "gone".into();
        s.workload.sampling.temp = 0.42;
        s.apply_profile();
        assert_eq!(s.workload.sampling.temp, 0.42);
    }

    #[test]
    fn args_carry_sampling_and_reasoning() {
        let mut s = settings_with_model("args");
        s.workload_profile = "instruct".into();
        s.apply_profile();
        let args = s.to_args().unwrap();
        assert_eq!(flag(&args, "--temp"), Some("0.7"));
        assert_eq!(flag(&args, "--top-p"), Some("0.8"));
        assert_eq!(flag(&args, "--presence-penalty"), Some("1.5"));
        assert_eq!(flag(&args, "--reasoning"), Some("off"));
    }

    #[test]
    fn agent_args_bound_the_thinking() {
        let mut s = settings_with_model("agent");
        s.workload_profile = "agent".into();
        s.apply_profile();
        let args = s.to_args().unwrap();
        assert_eq!(flag(&args, "--temp"), Some("0.6"));
        assert_eq!(flag(&args, "--reasoning"), Some("auto"));
        assert_eq!(flag(&args, "--reasoning-budget"), Some("512"));
        assert_eq!(
            flag(&args, "--reasoning-budget-message"),
            Some("Budget reached. State your final answer now.")
        );
    }

    /// -1 is the server's own default, so it is left off the command line --
    /// and the message that only makes sense with a budget goes with it.
    #[test]
    fn reasoning_budget_only_when_set() {
        let mut s = settings_with_model("budget");
        let args = s.to_args().unwrap();
        assert!(!args.iter().any(|a| a == "--reasoning-budget"));
        assert!(!args.iter().any(|a| a == "--reasoning-budget-message"));

        s.workload_profile = "custom".into();
        s.workload.reasoning_budget = 512;
        s.workload.reasoning_budget_message = String::new();
        let args = s.to_args().unwrap();
        assert_eq!(flag(&args, "--reasoning-budget"), Some("512"));
        assert!(!args.iter().any(|a| a == "--reasoning-budget-message"));
    }
}
