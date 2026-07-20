//! `.muragent` writer — build a signed agent package tarball.

use crate::agent::AgentProfile;
use crate::identity::AgentIdentity;
use crate::muragent::MuragentError;
use crate::muragent::dsse;
use crate::muragent::jcs_canonical;
use crate::muragent::manifest::MuragentManifest;
use crate::muragent::statement::{self, InTotoStatement};
use flate2::Compression;
use flate2::write::GzEncoder;
use std::fs;
use std::path::{Path, PathBuf};
use tar::Builder;

pub struct MuragentWriter {
    manifest: MuragentManifest,
    profile_yaml: String,
    identity: AgentIdentity,
    icon_files: Vec<(String, Vec<u8>)>,
    voice_yaml: Option<String>,
    sys_prompt_md: Option<String>,
    skill_files: Vec<(String, Vec<u8>)>,
    commander_assets: Vec<(String, Vec<u8>)>,
    hub_assets: Vec<(String, Vec<u8>)>,
}

impl MuragentWriter {
    pub fn new(manifest: MuragentManifest, profile_yaml: String, identity: AgentIdentity) -> Self {
        Self {
            manifest,
            profile_yaml,
            identity,
            icon_files: Vec::new(),
            voice_yaml: None,
            sys_prompt_md: None,
            skill_files: Vec::new(),
            commander_assets: Vec::new(),
            hub_assets: Vec::new(),
        }
    }

    pub fn add_icon(&mut self, name: &str, data: Vec<u8>) {
        self.icon_files.push((format!("icon/{name}"), data));
    }

    pub fn set_voice_yaml(&mut self, yaml: String) {
        self.voice_yaml = Some(yaml);
    }

    /// Bundle the agent's system prompt so the recipient runs with the same
    /// persona/instructions (without this the loaded agent has no prompt).
    pub fn set_sys_prompt(&mut self, md: String) {
        self.sys_prompt_md = Some(md);
    }

    /// Bundle a skill markdown file under `skills/<name>` so skill
    /// registrations in the profile keep their backing file after load.
    pub fn add_skill(&mut self, name: &str, data: Vec<u8>) {
        self.skill_files.push((format!("skills/{name}"), data));
    }

    pub fn add_commander_asset(&mut self, path: &str, data: Vec<u8>) {
        self.commander_assets
            .push((format!("assets/commander/{path}"), data));
    }

    pub fn add_hub_asset(&mut self, path: &str, data: Vec<u8>) {
        self.hub_assets.push((format!("assets/{path}"), data));
    }

    /// Write the `.muragent` tar.gz to `out_path`.
    pub fn write(&self, out_path: &Path) -> Result<(), MuragentError> {
        let manifest_yaml = serde_yaml_ng::to_string(&self.manifest)
            .map_err(|e| MuragentError::ManifestParse(e.to_string()))?;

        let signed_json_bytes = jcs_canonical::derive_signed_json(&manifest_yaml)?;

        let all_files = self.collect_all_files(&manifest_yaml, &signed_json_bytes);
        let statement: InTotoStatement = statement::build_statement(&signed_json_bytes, &all_files);

        let statement_value = serde_json::to_value(&statement)
            .map_err(|e| MuragentError::Other(format!("statement serialize: {e}")))?;
        let statement_canonical_bytes = crate::jcs::to_jcs(&statement_value);
        let statement_canonical = String::from_utf8(statement_canonical_bytes)
            .map_err(|e| MuragentError::Other(format!("jcs utf-8: {e}")))?;

        let envelope = dsse::sign(
            "application/vnd.in-toto+json",
            &statement_canonical,
            &self.identity,
        )?;
        let signatures_json = serde_json::to_string_pretty(&envelope)
            .map_err(|e| MuragentError::Other(format!("signatures serialize: {e}")))?;

        let file = fs::File::create(out_path).map_err(MuragentError::Io)?;
        let gz = GzEncoder::new(file, Compression::default());
        let mut tar = Builder::new(gz);

        add_blob(&mut tar, "manifest.yaml", manifest_yaml.as_bytes())?;
        add_blob(&mut tar, "manifest.signed.json", &signed_json_bytes)?;
        add_blob(&mut tar, "signatures.json", signatures_json.as_bytes())?;
        add_blob(&mut tar, "profile.yaml", self.profile_yaml.as_bytes())?;

        for (name, data) in &self.icon_files {
            add_blob(&mut tar, name, data)?;
        }

        if let Some(ref voice_yaml) = self.voice_yaml {
            add_blob(&mut tar, "voice/voice.yaml", voice_yaml.as_bytes())?;
        }

        if let Some(ref sys_prompt) = self.sys_prompt_md {
            add_blob(&mut tar, "sys_prompt.md", sys_prompt.as_bytes())?;
        }

        for (name, data) in &self.skill_files {
            add_blob(&mut tar, name, data)?;
        }

        for (name, data) in &self.commander_assets {
            add_blob(&mut tar, name, data)?;
        }

        for (name, data) in &self.hub_assets {
            add_blob(&mut tar, name, data)?;
        }

        tar.into_inner()
            .map_err(|e| MuragentError::Other(format!("close tar: {e}")))?
            .finish()
            .map_err(|e| MuragentError::Other(format!("flush gzip: {e}")))?;

        Ok(())
    }

    fn collect_all_files(
        &self,
        manifest_yaml: &str,
        signed_json_bytes: &[u8],
    ) -> Vec<(String, Vec<u8>)> {
        // These three are excluded from the Statement subject list anyway
        let mut files: Vec<(String, Vec<u8>)> = vec![
            ("manifest.yaml".into(), manifest_yaml.as_bytes().to_vec()),
            ("manifest.signed.json".into(), signed_json_bytes.to_vec()),
            ("signatures.json".into(), b"placeholder".to_vec()),
            ("profile.yaml".into(), self.profile_yaml.as_bytes().to_vec()),
        ];

        for (name, data) in &self.icon_files {
            files.push((name.clone(), data.clone()));
        }

        if let Some(ref voice) = self.voice_yaml {
            files.push(("voice/voice.yaml".into(), voice.as_bytes().to_vec()));
        }

        if let Some(ref sys_prompt) = self.sys_prompt_md {
            files.push(("sys_prompt.md".into(), sys_prompt.as_bytes().to_vec()));
        }

        for (name, data) in &self.skill_files {
            files.push((name.clone(), data.clone()));
        }

        for (name, data) in &self.commander_assets {
            files.push((name.clone(), data.clone()));
        }

        for (name, data) in &self.hub_assets {
            files.push((name.clone(), data.clone()));
        }

        files
    }
}

fn add_blob<W: std::io::Write>(
    tar: &mut Builder<W>,
    name: &str,
    bytes: &[u8],
) -> Result<(), MuragentError> {
    let mut header = tar::Header::new_gnu();
    header.set_size(bytes.len() as u64);
    header.set_mode(0o644);
    header.set_cksum();
    tar.append_data(&mut header, name, bytes)
        .map_err(|e| MuragentError::Other(format!("tar append {name}: {e}")))?;
    Ok(())
}

/// Build a `MuragentManifest` from an `AgentProfile`.
pub fn build_manifest_from_profile(profile: &AgentProfile, mur_version: &str) -> MuragentManifest {
    use crate::muragent::manifest::*;

    let behavior_preset = match profile.appearance.behavior_preset {
        crate::BehaviorPreset::Quiet => "quiet",
        crate::BehaviorPreset::Normal => "normal",
        crate::BehaviorPreset::Lively => "lively",
    }
    .to_string();

    MuragentManifest {
        schema: "mur-agent/2".into(),
        exported_at: chrono::Utc::now().to_rfc3339(),
        exporter: ExporterInfo {
            mur_version: mur_version.to_string(),
            tool: "mur".into(),
            min_hub_version: Some(mur_version.to_string()),
            min_commander_version: None,
        },
        agent: AgentRef {
            slug: profile.name.clone(),
            display_name: profile.display_name.clone(),
            bundle_id: format!("run.mur.agent.{}", profile.name),
            url_scheme: format!("muragent-{}", profile.name),
            original_uuid: profile.id.clone(),
        },
        required_surfaces: vec![Surface::Hub],
        optional_capabilities: profile.capabilities.clone(),
        mcp_servers: profile
            .mcp_servers
            .iter()
            .map(|s| McpServerRef {
                name: s.name.clone(),
                command_basename: PathBuf::from(&s.command)
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or(&s.command)
                    .to_string(),
            })
            .collect(),
        icon: IconHashes {
            formats: vec![],
            hash: IconHashMap::default(),
        },
        sanitized: SanitizedReport {
            removed_fields: vec!["identity.private_key".into()],
        },
        hub: Some(HubBlock {
            appearance: HubAppearance {
                style_preset: profile.appearance.style_preset.clone(),
                behavior_preset,
            },
            voice: if profile.voice.enabled {
                Some(HubVoice { enabled: true })
            } else {
                None
            },
            pet: Some(HubPet { enabled: true }),
            url_scheme_overrides: vec![],
        }),
        commander: None,
        deployment: None,
        assignment: None,
        model_hint: Some(crate::muragent::model_class::classify(
            &profile.model.provider,
            &profile.model.name,
        )),
        distribution: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::AgentProfile;
    use tempfile::TempDir;

    #[test]
    fn writer_produces_nonempty_tarball() {
        let tmp = TempDir::new().unwrap();
        let out = tmp.path().join("test.muragent");

        let profile = AgentProfile::default_for_tests();
        let identity = AgentIdentity::generate();
        let manifest = build_manifest_from_profile(&profile, "2.13.0");

        let profile_yaml = serde_yaml_ng::to_string(&profile).unwrap();
        let mut writer = MuragentWriter::new(manifest, profile_yaml, identity);
        writer.add_icon("icon-512.png", b"fake-png".to_vec());
        writer.write(&out).unwrap();

        assert!(out.exists());
        assert!(out.metadata().unwrap().len() > 0);
    }

    #[test]
    fn manifest_carries_model_hint_from_inline_binding() {
        let mut profile = AgentProfile::default_for_tests();
        profile.model = crate::agent::ModelConfig {
            provider: "ollama".into(),
            name: "llama3.2:3b".into(),
            params: Default::default(),
        };
        let m = build_manifest_from_profile(&profile, "1.0.0");
        let hint = m.model_hint.expect("model_hint populated");
        assert_eq!(hint.tier, crate::muragent::manifest::ModelTier::Small);
        assert!(hint.local_capable);
    }
}
