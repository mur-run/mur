---
name: mur-workflow
description: "Run saved workflows with mur. Trigger: /mur-run, /mur-workflow, 'mur run', 'run workflow', or 'use the workflow'. Searches and executes step-by-step task sequences with variables and tools."
---
# MUR Workflow Runner

## Triggers

This skill activates when:
- User types `/mur-run <query>` or `/mur-workflow <query>`
- User says "run workflow", "use the workflow for X", "mur run X"
- User's task clearly matches a known workflow from `mur inject` output

## Quick Run

```bash
mur run <name-or-query>    # Best method — finds and outputs workflow as executable prompt
```

This command:
1. Tries exact name match first
2. Falls back to semantic search (LanceDB)
3. Falls back to keyword search
4. Outputs the workflow with variables, tools, and steps ready to execute

## Manual Lookup

```bash
mur workflow search <query>    # Semantic search — find matching workflows
mur workflow list              # List all available workflows
mur workflow show <name> --md  # Full markdown details
```

## Executing a Workflow

After `mur run` outputs the workflow:

1. Read the **Variables** section
   - Use default values unless the user specifies differently
   - Ask for `required` variables without defaults
   - `array` type = iterate steps for each value
2. Follow **Steps** in order using the listed **Tools**
3. Adapt steps as needed — workflows are guides, not rigid scripts

## After Completion

If recording was active, suggest:
```bash
mur session stop --analyze
```

## Variable Types

| Type | Description |
|------|-------------|
| `string` | Text value |
| `url` | URL (validate format) |
| `path` | File path (expand ~, check existence) |
| `number` | Numeric value |
| `bool` | true/false |
| `array` | Multiple values — repeat steps for each |

## Example

User: `/mur-run find prices for AirPods`

```bash
mur run "find prices"
```

Output:
```
# Workflow: find-prices
Search and compare product prices using agent-browser.

## Variables
- `product_name` (string, required): Target product — default: `AirPods Pro 3`
- `target_site` (string, required): Website to search — default: `pchome`

## Tools
- agent-browser

## Steps
1. Check agent-browser available commands
2. Navigate to target_site and search for product_name
...
```

Then execute each step with the variables substituted.
