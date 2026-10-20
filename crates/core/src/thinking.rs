//! Per-model reasoning modes.
//!
//! Every model advertises the reasoning levels it really supports, with the
//! provider's own names. Local llama.cpp models are read from their chat
//! template (the only reliable source); hosted models come from the
//! provider's model list when it says, otherwise from a table of known
//! families.

use tessera_proto::{ProviderKind, ThinkingLevel, ThinkingOption};

fn opt(level: ThinkingLevel, label: &str, hint: &str) -> ThinkingOption {
    ThinkingOption {
        level,
        label: label.into(),
        hint: hint.into(),
    }
}

fn effort_option(level: ThinkingLevel) -> ThinkingOption {
    use ThinkingLevel::*;
    match level {
        Auto => opt(
            Auto,
            "Server default",
            "Use whatever the server is configured for",
        ),
        Off => opt(Off, "Off", "Answer directly, no reasoning; fastest"),
        Minimal => opt(Minimal, "Minimal", "A very short think before answering"),
        Low => opt(Low, "Low", "Quick, light reasoning"),
        Medium => opt(Medium, "Medium", "Balanced speed and depth"),
        High => opt(High, "High", "Thorough reasoning for hard problems"),
        XHigh => opt(
            XHigh,
            "Extra high",
            "Very thorough; best for complex coding and agentic work",
        ),
        Max => opt(Max, "Max", "Maximum depth; slowest and most expensive"),
    }
}

fn list(levels: &[ThinkingLevel]) -> Vec<ThinkingOption> {
    levels.iter().map(|l| effort_option(*l)).collect()
}

/// What a local chat template accepts (parsed from `/props`).
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct TemplateCaps {
    pub enable_thinking: bool,
    /// Accepted `reasoning_effort` values, in the template's own words.
    pub efforts: Vec<String>,
    pub default_effort: Option<String>,
    /// Values the template renames before checking them (`high` → `xhigh`).
    #[serde(default)]
    pub aliases: Vec<(String, String)>,
    /// The model family the template's own markers show (see
    /// [`template_family`]), for models whose id doesn't say.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub family: Option<String>,
}

/// The family a chat template belongs to, from markers only its family
/// uses: Mistral's `[TOOL_CALLS]` (which wants 9-character call ids),
/// gpt-oss's harmony channels, Gemma's turns, Llama 3's headers.
pub fn template_family(t: &str) -> Option<&'static str> {
    if t.contains("[TOOL_CALLS]") || t.contains("[AVAILABLE_TOOLS]") {
        Some("mistral")
    } else if t.contains("<|channel|>") {
        Some("gpt-oss")
    } else if t.contains("<start_of_turn>") {
        Some("gemma")
    } else if t.contains("<|start_header_id|>") {
        Some("llama")
    } else {
        None
    }
}

/// Parse a Jinja chat template for thinking controls.
pub fn parse_template(t: &str) -> TemplateCaps {
    let mut caps = TemplateCaps {
        enable_thinking: t.contains("enable_thinking"),
        family: template_family(t).map(str::to_string),
        ..Default::default()
    };
    if t.contains("reasoning_effort") {
        // `reasoning_effort|default('xhigh')`
        if let Some(i) = t.find("reasoning_effort|default(") {
            let rest = &t[i + "reasoning_effort|default(".len()..];
            caps.default_effort = rest
                .trim_start_matches(['\'', '"'])
                .split(['\'', '"'])
                .next()
                .map(str::to_string);
        }
        // `{%- if effort == 'high' %}{%- set effort = 'xhigh' %}`
        let alias = regex::Regex::new(
            r#"reasoning_effort\s*==\s*['"]([a-z]+)['"]\s*-?%\}\s*\{%-?\s*set\s+[a-z_]*reasoning_effort\s*=\s*['"]([a-z]+)['"]"#,
        )
        .unwrap();
        caps.aliases = alias
            .captures_iter(t)
            .map(|c| (c[1].to_string(), c[2].to_string()))
            .filter(|(a, b)| a != b)
            .collect();
        // `not in ('xhigh', 'medium', 'low')` or `in ["low", "medium", "high"]`
        let re = regex::Regex::new(
            r#"reasoning_effort\s+(?:not\s+)?in\s*[\(\[]\s*((?:['"][a-z]+['"]\s*,?\s*)+)[\)\]]"#,
        )
        .unwrap();
        if let Some(c) = re.captures(t) {
            caps.efforts = c[1]
                .split(',')
                .map(|s| s.trim().trim_matches(['\'', '"']).to_string())
                .filter(|s| !s.is_empty())
                .collect();
        }
        if caps.efforts.is_empty() {
            // Comparisons like `reasoning_effort == 'low'` (renamed values aren't levels).
            let eq = regex::Regex::new(r#"reasoning_effort\s*==\s*['"]([a-z]+)['"]"#).unwrap();
            for c in eq.captures_iter(t) {
                if !caps.efforts.contains(&c[1].to_string())
                    && !caps.aliases.iter().any(|(a, _)| *a == c[1])
                {
                    caps.efforts.push(c[1].to_string());
                }
            }
        }
        if caps.efforts.is_empty() {
            caps.efforts = vec!["low".into(), "medium".into(), "high".into()];
        }
    }
    caps
}

pub fn level_from_effort(s: &str) -> Option<ThinkingLevel> {
    Some(match s {
        "none" => ThinkingLevel::Off,
        "minimal" => ThinkingLevel::Minimal,
        "low" => ThinkingLevel::Low,
        "medium" => ThinkingLevel::Medium,
        "high" => ThinkingLevel::High,
        "xhigh" => ThinkingLevel::XHigh,
        "max" => ThinkingLevel::Max,
        _ => return None,
    })
}

pub fn effort_word(l: ThinkingLevel) -> &'static str {
    match l {
        ThinkingLevel::Auto => "auto",
        ThinkingLevel::Off => "none",
        ThinkingLevel::Minimal => "minimal",
        ThinkingLevel::Low => "low",
        ThinkingLevel::Medium => "medium",
        ThinkingLevel::High => "high",
        ThinkingLevel::XHigh => "xhigh",
        ThinkingLevel::Max => "max",
    }
}

/// How an OpenAI-compatible endpoint takes reasoning settings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChatStyle {
    /// Send nothing.
    None,
    /// llama.cpp-style chat template kwargs (`enable_thinking`) and/or `reasoning_effort`.
    Template { toggle: bool, efforts: bool },
    /// OpenRouter's unified `reasoning` object.
    OpenRouter,
    /// Plain `reasoning_effort`.
    Effort,
}

pub fn chat_style(
    base_url: &str,
    model: &str,
    reasoning: bool,
    caps: Option<&TemplateCaps>,
) -> ChatStyle {
    let m = model.to_ascii_lowercase();
    if base_url.contains("openrouter.ai") {
        return if reasoning {
            ChatStyle::OpenRouter
        } else {
            ChatStyle::None
        };
    }
    if base_url.contains("api.openai.com") || m.contains("gpt-oss") {
        return if reasoning {
            ChatStyle::Effort
        } else {
            ChatStyle::None
        };
    }
    if let Some(c) = caps {
        return ChatStyle::Template {
            toggle: c.enable_thinking,
            efforts: !c.efforts.is_empty(),
        };
    }
    if reasoning {
        ChatStyle::Template {
            toggle: true,
            efforts: false,
        }
    } else {
        ChatStyle::None
    }
}

/// What a request that sets nothing gets: the server's own default effort
/// (`--reasoning-effort`; `none` when it turns thinking off) as the template
/// reads it, else the template's default.
pub fn server_default(caps: &TemplateCaps, server: Option<&str>) -> Option<ThinkingLevel> {
    if server == Some("none") {
        return Some(ThinkingLevel::Off);
    }
    if caps.efforts.is_empty() {
        return caps.enable_thinking.then_some(ThinkingLevel::High);
    }
    let e = server.or(caps.default_effort.as_deref())?;
    let e = caps
        .aliases
        .iter()
        .find(|(a, _)| a == e)
        .map_or(e, |(_, b)| b.as_str());
    level_from_effort(e)
}

/// Options for a local model from its template; `server` as for [`server_default`].
pub fn from_template(
    caps: &TemplateCaps,
    server: Option<&str>,
) -> (Vec<ThinkingOption>, ThinkingLevel) {
    let mut out = vec![effort_option(ThinkingLevel::Auto)];
    if caps.enable_thinking {
        out.push(opt(ThinkingLevel::Off, "Off", "Thinking disabled; fastest"));
    }
    let mut levels: Vec<ThinkingLevel> = caps
        .efforts
        .iter()
        .filter_map(|e| level_from_effort(e))
        .filter(|l| *l != ThinkingLevel::Off)
        .collect();
    levels.sort();
    levels.dedup();
    if levels.is_empty() && caps.enable_thinking {
        out.push(opt(ThinkingLevel::High, "On", "Think before answering"));
    }
    out.extend(levels.into_iter().map(effort_option));
    // "Server default" says what it amounts to.
    if let Some(label) = server_default(caps, server)
        .and_then(|l| out.iter().find(|o| o.level == l))
        .map(|o| o.label.clone())
    {
        out[0].hint = format!("{} ({label})", out[0].hint);
    }
    (out, ThinkingLevel::Auto)
}

/// What to send a template model asked for `want`: the level the template
/// renames it to (`high` → `xhigh`), else the nearest one it offers (a chat
/// may hold a level from before the template was read).
pub fn for_template(
    want: ThinkingLevel,
    caps: &TemplateCaps,
    options: &[ThinkingOption],
    default: ThinkingLevel,
) -> ThinkingLevel {
    let word = effort_word(want);
    if matches!(want, ThinkingLevel::Auto | ThinkingLevel::Off)
        || caps.efforts.is_empty()
        || caps.efforts.iter().any(|e| e == word)
    {
        return want;
    }
    caps.aliases
        .iter()
        .find(|(a, _)| a == word)
        .and_then(|(_, b)| level_from_effort(b))
        .unwrap_or_else(|| clamp(want, options, default))
}

/// Marks a local model's modes as a guess until its chat template has been
/// read (llama.cpp shows it only while the model is loaded).
pub fn provisional(opts: &mut [ThinkingOption], server: Option<&str>) {
    if let Some(o) = opts.iter_mut().find(|o| o.level == ThinkingLevel::Auto) {
        let now = server
            .and_then(level_from_effort)
            .map(|l| format!(" ({})", effort_option(l).label))
            .unwrap_or_default();
        o.hint = format!(
            "Use whatever the server is configured for{now}; the exact modes show once this model has run"
        );
    }
}

/// Open models that always reason (R1, QwQ, `-thinking` builds).
fn always_thinks(m: &str) -> bool {
    m.contains("qwq") || m.contains("thinking") || m.contains("-r1") || m.starts_with("r1")
}

/// Known families when the provider doesn't describe the model.
pub fn known(
    kind: ProviderKind,
    base_url: &str,
    model: &str,
    reasoning: bool,
) -> (Vec<ThinkingOption>, ThinkingLevel) {
    use ThinkingLevel::*;
    let m = model.to_ascii_lowercase();
    match kind {
        ProviderKind::Anthropic => anthropic(&m),
        ProviderKind::ClaudeCode => crate::claude_code::thinking(&m),
        ProviderKind::OpenaiCodex | ProviderKind::OpenaiResponses => openai(&m, reasoning),
        ProviderKind::OpenaiChat => {
            if base_url.contains("openrouter.ai") {
                if reasoning {
                    (list(&[Auto, Off, Low, Medium, High]), Auto)
                } else {
                    (vec![], Auto)
                }
            } else if base_url.contains("api.openai.com") {
                openai(&m, reasoning)
            } else if m.contains("gpt-oss") {
                (list(&[Auto, Low, Medium, High]), Auto)
            } else if always_thinks(&m) || m.contains("instruct") {
                // Thinking can't be turned off (or on, for `-instruct` builds).
                (vec![], Auto)
            } else if reasoning {
                // Hybrid-thinking open models (Qwen3, GLM, DeepSeek V3.x…): on/off.
                (
                    vec![
                        effort_option(Auto),
                        opt(Off, "Off", "Thinking disabled; fastest"),
                        opt(High, "On", "Think before answering"),
                    ],
                    Auto,
                )
            } else {
                (vec![], Auto)
            }
        }
    }
}

fn anthropic(m: &str) -> (Vec<ThinkingOption>, ThinkingLevel) {
    use ThinkingLevel::*;
    let has = |s: &str| m.contains(s);
    if has("fable") || has("mythos") {
        (list(&[Low, Medium, High, XHigh, Max]), High)
    } else if has("opus-5-5") {
        (list(&[Low, Medium, High, XHigh, Max]), Medium)
    } else if has("sonnet-5-5") {
        (list(&[Off, Low, Medium, High, XHigh, Max]), High)
    } else if has("opus-5") || has("sonnet-5") {
        (list(&[Low, Medium, High, XHigh, Max]), High)
    } else if has("opus-4-8") || has("opus-4-7") {
        (list(&[Off, Low, Medium, High, XHigh, Max]), High)
    } else if has("opus-4-6") || has("sonnet-4-6") {
        (list(&[Off, Low, Medium, High, Max]), High)
    } else {
        // Haiku 4.5 and older: thinking budgets.
        (
            vec![
                opt(Off, "Off", "No extended thinking"),
                opt(Low, "Low", "Short thinking budget (~4k tokens)"),
                opt(Medium, "Medium", "Moderate thinking budget (~10k tokens)"),
                opt(High, "High", "Large thinking budget (~24k tokens)"),
            ],
            Off,
        )
    }
}

fn openai(m: &str, reasoning: bool) -> (Vec<ThinkingOption>, ThinkingLevel) {
    use ThinkingLevel::*;
    if m.starts_with("gpt-6") || m.starts_with("gpt-5.6") {
        (list(&[Off, Low, Medium, High, XHigh, Max]), Medium)
    } else if m.starts_with("gpt-5.") {
        (list(&[Off, Low, Medium, High, XHigh]), Medium)
    } else if m.starts_with("gpt-5") || m.starts_with("codex") {
        (list(&[Minimal, Low, Medium, High]), Medium)
    } else if reasoning || m.starts_with("o1") || m.starts_with("o3") || m.starts_with("o4") {
        (list(&[Low, Medium, High]), Medium)
    } else {
        (vec![], Auto)
    }
}

/// The supported level closest to `want` (ties go down: cheaper).
pub fn clamp(
    want: ThinkingLevel,
    options: &[ThinkingOption],
    default: ThinkingLevel,
) -> ThinkingLevel {
    if options.is_empty() {
        return ThinkingLevel::Auto;
    }
    if options.iter().any(|o| o.level == want) {
        return want;
    }
    if want == ThinkingLevel::Auto {
        return if options.iter().any(|o| o.level == default) {
            default
        } else {
            options[0].level
        };
    }
    let rank = |l: ThinkingLevel| l as i32;
    options
        .iter()
        // Never silently turn reasoning off when some was asked for.
        .filter(|o| o.level != ThinkingLevel::Auto && o.level != ThinkingLevel::Off)
        .min_by_key(|o| {
            (
                (rank(o.level) - rank(want)).abs() * 2,
                rank(o.level) > rank(want),
            )
        })
        .map(|o| o.level)
        .unwrap_or(options[0].level)
}

#[cfg(test)]
mod tests {
    use super::*;

    const QWEN: &str = "{%- if enable_thinking is undefined or enable_thinking is true %} {%- set resolved_reasoning_effort = reasoning_effort|default('xhigh') %} {%- if resolved_reasoning_effort not in ('xhigh', 'medium', 'low') %}{{ raise_exception('x') }}";

    #[test]
    fn reads_qwen_template() {
        let caps = parse_template(QWEN);
        assert!(caps.enable_thinking);
        assert_eq!(caps.efforts, vec!["xhigh", "medium", "low"]);
        assert_eq!(caps.default_effort.as_deref(), Some("xhigh"));
        let (opts, def) = from_template(&caps, None);
        let levels: Vec<ThinkingLevel> = opts.iter().map(|o| o.level).collect();
        assert_eq!(
            levels,
            vec![
                ThinkingLevel::Auto,
                ThinkingLevel::Off,
                ThinkingLevel::Low,
                ThinkingLevel::Medium,
                ThinkingLevel::XHigh
            ]
        );
        assert_eq!(def, ThinkingLevel::Auto);
        assert_eq!(opts.last().unwrap().label, "Extra high");
        assert!(opts[0].hint.ends_with("(Extra high)"), "{}", opts[0].hint);
    }

    /// The template llama.cpp serves for Qwen 3.8 27B (`/props`, saved as is).
    fn qwen38() -> TemplateCaps {
        let props: serde_json::Value = serde_json::from_str(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/llama-props-qwen3.8-27b.json"
        )))
        .unwrap();
        parse_template(props["chat_template"].as_str().unwrap())
    }

    #[test]
    fn reads_the_real_qwen38_template() {
        let caps = qwen38();
        assert!(caps.enable_thinking);
        assert_eq!(caps.efforts, vec!["xhigh", "medium", "low"]);
        assert_eq!(caps.default_effort.as_deref(), Some("xhigh"));
        assert_eq!(
            caps.aliases,
            vec![("high".to_string(), "xhigh".to_string())]
        );
        // The router starts it with `--reasoning-effort high`, which the template reads as xhigh.
        let (opts, def) = from_template(&caps, Some("high"));
        let labels: Vec<&str> = opts.iter().map(|o| o.label.as_str()).collect();
        assert_eq!(
            labels,
            ["Server default", "Off", "Low", "Medium", "Extra high"]
        );
        assert_eq!(def, ThinkingLevel::Auto);
        assert!(opts[0].hint.ends_with("(Extra high)"), "{}", opts[0].hint);
        assert_eq!(
            server_default(&caps, Some("none")),
            Some(ThinkingLevel::Off)
        );
        assert_eq!(server_default(&caps, Some("low")), Some(ThinkingLevel::Low));
        // A chat still on a level the template lacks sends what it means.
        let send = |l| for_template(l, &caps, &opts, def);
        assert_eq!(send(ThinkingLevel::High), ThinkingLevel::XHigh);
        assert_eq!(send(ThinkingLevel::Max), ThinkingLevel::XHigh);
        assert_eq!(send(ThinkingLevel::Minimal), ThinkingLevel::Low);
        assert_eq!(send(ThinkingLevel::Off), ThinkingLevel::Off);
        assert_eq!(send(ThinkingLevel::Medium), ThinkingLevel::Medium);
        // Sent as the template wants it.
        assert_eq!(
            chat_style("http://127.0.0.1:1234/v1", "qwen3.8-27b", true, Some(&caps)),
            ChatStyle::Template {
                toggle: true,
                efforts: true
            }
        );
        assert_eq!(effort_word(ThinkingLevel::XHigh), "xhigh");
    }

    #[test]
    fn reads_the_uncensored_variant_without_the_high_rename() {
        // From the GGUF of the user's second model: same levels, but `high`
        // is refused rather than read as xhigh.
        let caps = parse_template(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/llama-template-qwen3.8-27b-uncensored.jinja"
        )));
        assert!(caps.enable_thinking);
        assert_eq!(caps.efforts, vec!["xhigh", "medium", "low"]);
        assert!(caps.aliases.is_empty());
        let (opts, def) = from_template(&caps, Some("xhigh"));
        assert_eq!(
            opts.iter().map(|o| o.level).collect::<Vec<_>>(),
            [
                ThinkingLevel::Auto,
                ThinkingLevel::Off,
                ThinkingLevel::Low,
                ThinkingLevel::Medium,
                ThinkingLevel::XHigh
            ]
        );
        assert!(opts[0].hint.ends_with("(Extra high)"));
        // Never sent as `high` (the template would refuse it).
        assert_eq!(
            for_template(ThinkingLevel::High, &caps, &opts, def),
            ThinkingLevel::Medium
        );
    }

    #[test]
    fn local_families_before_the_template_is_known() {
        let levels = |id: &str| {
            let reasoning = crate::providers::default_info(
                &crate::config::ProviderConfig {
                    id: "l".into(),
                    name: String::new(),
                    kind: ProviderKind::OpenaiChat,
                    base_url: "http://127.0.0.1:1234/v1".into(),
                    discover: true,
                    models: vec![],
                    headers: Default::default(),
                    reasoning_replay: Default::default(),
                    parallel: None,
                },
                id,
                None,
            )
            .reasoning;
            known(
                ProviderKind::OpenaiChat,
                "http://127.0.0.1:1234/v1",
                id,
                reasoning,
            )
            .0
            .iter()
            .map(|o| o.level)
            .collect::<Vec<_>>()
        };
        use ThinkingLevel::*;
        assert_eq!(levels("qwen3.8-27b-uncensored"), [Auto, Off, High]);
        assert_eq!(levels("gpt-oss-120b"), [Auto, Low, Medium, High]);
        for id in [
            "DeepSeek-R1-Distill-Qwen-32B",
            "qwq-32b",
            "Qwen3-30B-A3B-Thinking-2507",
            "Qwen3-Coder-30B-A3B-Instruct",
            "gemma-3-27b-it",
        ] {
            assert!(levels(id).is_empty(), "{id}");
        }
        let mut o = known(ProviderKind::OpenaiChat, "", "qwen3-8b", true).0;
        provisional(&mut o, Some("xhigh"));
        assert!(
            o[0].hint.contains("(Extra high)") && o[0].hint.contains("once this model has run")
        );
    }

    #[test]
    fn known_families_and_clamping() {
        let (o, d) = known(ProviderKind::OpenaiCodex, "", "gpt-6-astra", true);
        assert!(o.iter().any(|x| x.level == ThinkingLevel::Max) && d == ThinkingLevel::Medium);
        let (o, _) = known(ProviderKind::Anthropic, "", "claude-opus-5-5", true);
        assert!(
            !o.iter().any(|x| x.level == ThinkingLevel::Off),
            "Opus 5.5 cannot disable thinking"
        );
        let (o, _) = known(ProviderKind::Anthropic, "", "claude-sonnet-5-5", true);
        assert!(o.iter().any(|x| x.level == ThinkingLevel::Off));
        // Qwen (low/medium/xhigh): "high" snaps to xhigh? no — nearest is medium or xhigh; ties go cheaper.
        let (q, _) = from_template(&parse_template(QWEN), None);
        assert_eq!(
            clamp(ThinkingLevel::High, &q, ThinkingLevel::Auto),
            ThinkingLevel::Medium
        );
        assert_eq!(
            clamp(ThinkingLevel::Max, &q, ThinkingLevel::Auto),
            ThinkingLevel::XHigh
        );
        assert_eq!(
            clamp(ThinkingLevel::Minimal, &q, ThinkingLevel::Auto),
            ThinkingLevel::Low
        );
        assert_eq!(
            clamp(ThinkingLevel::Medium, &[], ThinkingLevel::Auto),
            ThinkingLevel::Auto
        );
    }
}
