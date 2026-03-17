# MUR System Prompts Externalization Integration

## Overview
This document describes the integration of external system prompt templates with the MUR core system.

## Template System Architecture

### Location
- Templates: `~/.mur/templates/`
- Configuration: `~/.mur/templates/config/`
- Processor: `~/.mur/templates/process-template.sh`

### Integration Points

1. **mur-out skill**: Modified to use external templates for LLM analysis instructions
2. **mur session stop --analyze**: Now processes sessions using configurable templates
3. **mur sync**: Updated to handle template-generated patterns

### Template Types

1. **session-analysis.md**: For analyzing recorded sessions
2. **pattern-extraction.md**: For extracting reusable patterns
3. **workflow-execution.md**: For executing workflows with variables

### Configuration

Edit `~/.mur/templates/config/template-config.yaml` to:
- Change template engine
- Adjust variable handling
- Configure output formatting
- Set logging levels

## Usage Examples

### Process a session with external template
```bash
# Create variables file
echo '{"project_context": "mur-commander", "extraction_focus": "patterns"}' > /tmp/session-vars.json

# Process session analysis template
~/.mur/templates/process-template.sh -t session-analysis -v /tmp/session-vars.json -o /tmp/analysis-prompt.md
```

### Custom template modification
```bash
# Edit session analysis template
code ~/.mur/templates/prompts/session-analysis.md

# Test template processing
~/.mur/templates/process-template.sh -t session-analysis -v /tmp/test-vars.json
```

## Next Steps

1. Implement proper Handlebars/Jinja2 processing in the processor script
2. Integrate template system with Go backend
3. Add template validation and caching
4. Create web interface for template editing
5. Add template versioning and rollback capabilities
