---
name: mur-workflow
description: "Run saved workflows — search and execute step-by-step task sequences with variables and tools"
---
# MUR Workflow Runner

When the user asks to perform a task, check if a saved workflow matches before starting from scratch.

## Finding Workflows

```bash
mur workflow search <query>    # Semantic search (LanceDB) with keyword fallback
mur workflow list              # List all available workflows
mur workflow show <name>       # Human-readable details
mur workflow show <name> --md  # Markdown output (use this for structured reading)
```

## Running a Workflow

1. Search for a matching workflow: `mur workflow search "<user's request>"`
2. If found, read full details: `mur workflow show <name> --md`
3. Check **Variables**:
   - Use default values unless the user specifies differently
   - Ask for `required` variables that have no defaults and aren't clear from context
   - `array` type = iterate the steps for each value
4. Follow **Steps** in order, using the listed **Tools**
5. Adapt steps as needed — workflows are guides, not rigid scripts

## When to Use Workflows

- User asks to do something that sounds like a repeatable task
- User explicitly says "run workflow" or "use the workflow for X"
- A previous session captured a workflow for this type of task

## When NOT to Use

- Simple one-off questions (no workflow needed)
- The user explicitly wants to do something differently
- No matching workflow found — just proceed normally

## After Completing a Workflow

If the session was recorded (`mur session start` was run), suggest:
```bash
mur session stop --analyze
```
This opens the session review where the user can refine or create a new workflow.

## Variable Types

- `string` — text value
- `url` — URL (validate format)
- `path` — file path (expand ~ and check existence)
- `number` — numeric value
- `bool` — true/false
- `array` — multiple values; repeat steps for each

## Example

User: "find AirPods Pro 3 prices on shopping sites"

```bash
mur workflow search "find prices"
# → find-prices (92% match, 8 steps) [agent-browser]

mur workflow show find-prices --md
# → Shows: variables (product_name, target_site), tools, steps
```

Then execute the steps with the detected variables:
- `product_name` = "AirPods Pro 3"
- `target_site` = from user context or default
