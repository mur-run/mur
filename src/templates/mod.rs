//! Template processing for external system prompts

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use handlebars::Handlebars;

#[derive(Debug, Deserialize, Serialize)]
pub struct TemplateMetadata {
    pub name: String,
    pub version: String,
    pub description: String,
    pub variables: Vec<VariableDefinition>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct VariableDefinition {
    pub name: String,
    #[serde(rename = "type")]
    pub var_type: String,
    pub required: Option<bool>,
    pub default: Option<serde_json::Value>,
    pub description: Option<String>,
}

pub struct TemplateProcessor {
    templates_dir: PathBuf,
    handlebars: Handlebars<'static>,
}

impl TemplateProcessor {
    pub fn new<P: AsRef<Path>>(templates_dir: P) -> Result<Self> {
        let mut handlebars = Handlebars::new();
        
        // 註冊自定義助手函數
        handlebars.register_helper("fragment", Box::new(fragment_helper));
        
        Ok(Self {
            templates_dir: templates_dir.as_ref().to_path_buf(),
            handlebars,
        })
    }

    pub fn load_template(&mut self, name: &str) -> Result<(TemplateMetadata, String)> {
        let template_path = self.templates_dir.join("prompts").join(format!("{}.md", name));
        let content = fs::read_to_string(&template_path)
            .with_context(|| format!("Failed to read template: {}", template_path.display()))?;

        // 解析 YAML frontmatter
        let parts: Vec<&str> = content.splitn(3, "---").collect();
        if parts.len() < 3 {
            anyhow::bail!("Template {} missing YAML frontmatter", name);
        }

        let metadata: TemplateMetadata = serde_yaml::from_str(parts[1])
            .with_context(|| "Failed to parse template metadata")?;

        let template_content = parts[2].trim().to_string();

        // 註冊模板到 handlebars
        self.handlebars.register_template_string(name, &template_content)
            .with_context(|| "Failed to register template")?;

        Ok((metadata, template_content))
    }

    pub fn process_template(
        &mut self,
        name: &str,
        variables: &HashMap<String, serde_json::Value>,
    ) -> Result<String> {
        // 載入模板（如果還未載入）
        let (metadata, _) = self.load_template(name)?;

        // 驗證變數
        self.validate_variables(&metadata, variables)?;

        // 處理模板
        let result = self.handlebars.render(name, variables)
            .with_context(|| format!("Failed to render template: {}", name))?;

        Ok(result)
    }

    fn validate_variables(
        &self,
        metadata: &TemplateMetadata,
        variables: &HashMap<String, serde_json::Value>,
    ) -> Result<()> {
        for var_def in &metadata.variables {
            let required = var_def.required.unwrap_or(false);
            
            if required && !variables.contains_key(&var_def.name) {
                anyhow::bail!("Required variable '{}' is missing", var_def.name);
            }
        }
        
        Ok(())
    }

    /// 為 mur-out 提供的主要函數
    pub fn process_session_analysis(
        &mut self,
        session_data: &str,
        project_context: &str,
        extraction_focus: &str,
    ) -> Result<String> {
        let mut variables = HashMap::new();
        variables.insert("session_data".to_string(), serde_json::Value::String(session_data.to_string()));
        variables.insert("project_context".to_string(), serde_json::Value::String(project_context.to_string()));
        variables.insert("extraction_focus".to_string(), serde_json::Value::String(extraction_focus.to_string()));

        self.process_template("session-analysis", &variables)
    }
}

// Fragment 助手函數 - 載入模板片段
fn fragment_helper(
    h: &handlebars::Helper,
    _: &handlebars::Handlebars,
    _: &handlebars::Context,
    _: &mut handlebars::RenderContext,
    out: &mut dyn handlebars::Output,
) -> handlebars::HelperResult {
    let fragment_name = h.param(0)
        .ok_or_else(|| handlebars::RenderError::new("fragment helper requires a parameter"))?
        .value()
        .as_str()
        .ok_or_else(|| handlebars::RenderError::new("fragment name must be a string"))?;

    // 構建片段路徑（假設從環境變數或配置中獲取）
    let home_dir = std::env::var("HOME").unwrap_or_default();
    let fragment_path = format!("{}/.mur/templates/fragments/{}.md", home_dir, fragment_name);

    match fs::read_to_string(&fragment_path) {
        Ok(content) => {
            out.write(&content)?;
            Ok(())
        },
        Err(_) => {
            out.write(&format!("<!-- Fragment '{}' not found -->", fragment_name))?;
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::env;

    #[test]
    fn test_template_processing() {
        let home_dir = env::var("HOME").unwrap();
        let templates_dir = format!("{}/.mur/templates", home_dir);
        
        let mut processor = TemplateProcessor::new(&templates_dir).unwrap();
        
        let mut variables = HashMap::new();
        variables.insert("session_data".to_string(), serde_json::Value::String("test session".to_string()));
        variables.insert("project_context".to_string(), serde_json::Value::String("test project".to_string()));
        variables.insert("extraction_focus".to_string(), serde_json::Value::String("patterns".to_string()));

        let result = processor.process_template("session-analysis", &variables);
        assert!(result.is_ok());
    }
}
