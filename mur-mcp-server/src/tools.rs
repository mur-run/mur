// mur-mcp-server/src/tools.rs
use std::collections::BTreeMap;

use serde::Serialize;
use serde_json::{Value, json};

use mur_compress::{AutoCfg, CompressConfig, CompressEngine, RetrieveResult};
use mur_core::cmd::notes_cmd;

/// Build a per-call compression engine rooted at <mur_home>/compress.
fn compress_engine() -> Result<CompressEngine, String> {
    let home = resolve_mur_home().map_err(|e| format!("compress engine unavailable: {e}"))?;
    let cfg = CompressConfig::load(&home);
    CompressEngine::new(home.join("compress"), cfg)
        .map_err(|e| format!("compress engine unavailable: {e}"))
}

/// JSON Schema for a tool parameter (MCP uses JSON Schema subset).
#[derive(Debug, Clone, Serialize)]
pub struct ToolParam {
    #[serde(rename = "type")]
    pub param_type: String,
    pub description: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default: Option<Value>,
}

/// MCP tool definition returned by tools/list.
#[derive(Debug, Clone, Serialize)]
pub struct Tool {
    pub name: String,
    pub description: String,
    #[serde(rename = "inputSchema")]
    pub input_schema: ToolInputSchema,
}

#[derive(Debug, Clone, Serialize)]
pub struct ToolInputSchema {
    #[serde(rename = "type")]
    pub schema_type: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub properties: Option<BTreeMap<String, ToolParam>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub required: Option<Vec<String>>,
}

/// Return all registered tools.
pub fn all_tools() -> Vec<Tool> {
    vec![
        // ── notes tools ──
        Tool {
            name: "mur_notes_search".into(),
            description: "Search MUR notes and patterns by keyword query. Returns ranked results with name, description, maturity, and relevance score.".into(),
            input_schema: ToolInputSchema {
                schema_type: "object".into(),
                properties: Some(BTreeMap::from([
                    ("query".into(), ToolParam {
                        param_type: "string".into(),
                        description: "Search query".into(),
                        default: None,
                    }),
                    ("limit".into(), ToolParam {
                        param_type: "integer".into(),
                        description: "Max results, 1-10 (default: 5)".into(),
                        default: Some(json!(5)),
                    }),
                ])),
                required: Some(vec!["query".into()]),
            },
        },
        Tool {
            name: "mur_notes_show".into(),
            description: "Load a specific note or pattern by name. Returns full body, metadata, maturity, and tags.".into(),
            input_schema: ToolInputSchema {
                schema_type: "object".into(),
                properties: Some(BTreeMap::from([
                    ("name".into(), ToolParam {
                        param_type: "string".into(),
                        description: "Note name (exact match)".into(),
                        default: None,
                    }),
                ])),
                required: Some(vec!["name".into()]),
            },
        },
        // ── project tools ──
        Tool {
            name: "mur_project_search".into(),
            description: "Search indexed project source code using hybrid vector+BM25. Returns code snippets with file paths, line numbers, and relevance scores. Only works after 'mur project index' has been run for the project.".into(),
            input_schema: ToolInputSchema {
                schema_type: "object".into(),
                properties: Some(BTreeMap::from([
                    ("query".into(), ToolParam {
                        param_type: "string".into(),
                        description: "Search query".into(),
                        default: None,
                    }),
                    ("project".into(), ToolParam {
                        param_type: "string".into(),
                        description: "Project name to search. Defaults to the current working directory's project.".into(),
                        default: None,
                    }),
                    ("limit".into(), ToolParam {
                        param_type: "integer".into(),
                        description: "Max results, 1-10 (default: 5)".into(),
                        default: Some(json!(5)),
                    }),
                    ("all".into(), ToolParam {
                        param_type: "boolean".into(),
                        description: "Search across ALL indexed projects instead of just the current one (default: false).".into(),
                        default: Some(json!(false)),
                    }),
                ])),
                required: Some(vec!["query".into()]),
            },
        },
        Tool {
            name: "mur_project_status".into(),
            description: "Show which projects are indexed and their indexing status (chunk count, last indexed, freshness, in-progress indexing). Use before project search to check if a project is indexed.".into(),
            input_schema: ToolInputSchema {
                schema_type: "object".into(),
                properties: None,
                required: None,
            },
        },
        // ── agent tools ──
        Tool {
            name: "mur_agent_status".into(),
            description: "List configured MUR agents with their running state, health, transport, and tool counts. Use to check if agents are online before sending A2A messages. Pass a name to get detail for one agent; omit to list all.".into(),
            input_schema: ToolInputSchema {
                schema_type: "object".into(),
                properties: Some(BTreeMap::from([
                    ("name".into(), ToolParam {
                        param_type: "string".into(),
                        description: "Optional agent name. Shows detail for one agent; lists all if omitted.".into(),
                        default: None,
                    }),
                ])),
                required: None,
            },
        },
        // ── context tools ──
        Tool {
            name: "mur_hook_context".into(),
            description: "Get the patterns that MUR would inject for the current project context. Returns top-ranked patterns within a token budget. Use at session start or when switching project contexts.".into(),
            input_schema: ToolInputSchema {
                schema_type: "object".into(),
                properties: Some(BTreeMap::from([
                    ("query".into(), ToolParam {
                        param_type: "string".into(),
                        description: "Override auto-detected context query".into(),
                        default: None,
                    }),
                    ("compact".into(), ToolParam {
                        param_type: "boolean".into(),
                        description: "Return fewer patterns in shorter format (default: false)".into(),
                        default: Some(json!(false)),
                    }),
                    ("budget".into(), ToolParam {
                        param_type: "integer".into(),
                        description: "Token budget for returned content (default: 2000)".into(),
                        default: Some(json!(2000)),
                    }),
                ])),
                required: None,
            },
        },
        // ── media tools ──
        Tool {
            name: "vlc_open".into(),
            description: "Open a local video file path or a URL (e.g. a YouTube link) in VLC and start playing. Returns playback status.".into(),
            input_schema: ToolInputSchema {
                schema_type: "object".into(),
                properties: Some(BTreeMap::from([
                    ("source".into(), ToolParam {
                        param_type: "string".into(),
                        description: "Local file path or video URL (YouTube supported)".into(),
                        default: None,
                    }),
                ])),
                required: Some(vec!["source".into()]),
            },
        },
        Tool {
            name: "vlc_playback".into(),
            description: "Control VLC playback. action ∈ play|pause|toggle|stop|seek|volume. For seek, value=seconds; for volume, value=0-512 (256=100%).".into(),
            input_schema: ToolInputSchema {
                schema_type: "object".into(),
                properties: Some(BTreeMap::from([
                    ("action".into(), ToolParam {
                        param_type: "string".into(),
                        description: "play|pause|toggle|stop|seek|volume".into(),
                        default: None,
                    }),
                    ("value".into(), ToolParam {
                        param_type: "number".into(),
                        description: "Seconds (seek) or volume level (volume)".into(),
                        default: None,
                    }),
                ])),
                required: Some(vec!["action".into()]),
            },
        },
        Tool {
            name: "vlc_status".into(),
            description: "Get current VLC playback status (state, time, length, volume). Use before narrating so the explanation matches the current frame.".into(),
            input_schema: ToolInputSchema {
                schema_type: "object".into(),
                properties: None,
                required: None,
            },
        },
        Tool {
            name: "scene_explain".into(),
            description: "Capture the current VLC frame and explain what is on screen using the local multimodal model (offline, private). Optionally pass a specific question.".into(),
            input_schema: ToolInputSchema {
                schema_type: "object".into(),
                properties: Some(BTreeMap::from([
                    ("prompt".into(), ToolParam {
                        param_type: "string".into(),
                        description: "Optional question about the frame; defaults to a general description".into(),
                        default: None,
                    }),
                ])),
                required: None,
            },
        },
        Tool {
            name: "video_analyze".into(),
            description: "Analyze a whole video (YouTube link or local file) and return a structured zh-TW summary or conclusions with clickable timestamps. Uses captions + the local model. Omit 'source' to analyze the currently open video.".into(),
            input_schema: ToolInputSchema {
                schema_type: "object".into(),
                properties: Some(BTreeMap::from([
                    ("source".into(), ToolParam {
                        param_type: "string".into(),
                        description: "Video URL or local path; omit to use the currently open video".into(),
                        default: None,
                    }),
                    ("mode".into(), ToolParam {
                        param_type: "string".into(),
                        description: "summary (default) | conclusions | qa".into(),
                        default: None,
                    }),
                    ("focus".into(), ToolParam {
                        param_type: "string".into(),
                        description: "For qa mode: the question to answer".into(),
                        default: None,
                    }),
                ])),
                required: None,
            },
        },
        Tool {
            name: "watch_start".into(),
            description: "Begin a proactive co-watching session: MuR may briefly comment on big scene changes (runtime-only; consent-gated; say \"噓\" to mute).".into(),
            input_schema: ToolInputSchema {
                schema_type: "object".into(),
                properties: None,
                required: None,
            },
        },
        Tool {
            name: "watch_stop".into(),
            description: "End the proactive co-watching session.".into(),
            input_schema: ToolInputSchema {
                schema_type: "object".into(),
                properties: None,
                required: None,
            },
        },
        Tool {
            name: "watch_mute".into(),
            description: "Silence proactive interjections without ending the session (\"噓\").".into(),
            input_schema: ToolInputSchema {
                schema_type: "object".into(),
                properties: None,
                required: None,
            },
        },
        Tool {
            name: "watch_status".into(),
            description: "Report the current co-watching session state (active/muted/consent).".into(),
            input_schema: ToolInputSchema {
                schema_type: "object".into(),
                properties: None,
                required: None,
            },
        },
        // ── compress tools ──
        Tool {
            name: "mur_compress".into(),
            description: "Compress bulky agent text (tool output, logs, search results, diffs, JSON) before it reaches the LLM. Reversible: the original is stored locally and retrievable by hash via mur_retrieve.".into(),
            input_schema: ToolInputSchema {
                schema_type: "object".into(),
                properties: Some(BTreeMap::from([
                    ("content".into(), ToolParam {
                        param_type: "string".into(),
                        description: "The text to compress.".into(),
                        default: None,
                    }),
                    ("query".into(), ToolParam {
                        param_type: "string".into(),
                        description: "Optional query to bias which lines/items are kept.".into(),
                        default: None,
                    }),
                ])),
                required: Some(vec!["content".into()]),
            },
        },
        Tool {
            name: "mur_retrieve".into(),
            description: "Retrieve the original content stored by mur_compress, by its hash. With a query, returns only the BM25-relevant items.".into(),
            input_schema: ToolInputSchema {
                schema_type: "object".into(),
                properties: Some(BTreeMap::from([
                    ("hash".into(), ToolParam {
                        param_type: "string".into(),
                        description: "Hash from a prior mur_compress result (e.g. hash=abc123...).".into(),
                        default: None,
                    }),
                    ("query".into(), ToolParam {
                        param_type: "string".into(),
                        description: "Optional query to filter the stored items.".into(),
                        default: None,
                    }),
                ])),
                required: Some(vec!["hash".into()]),
            },
        },
        Tool {
            name: "mur_compress_stats".into(),
            description: "Show cumulative token-compression savings (compressions, tokens saved, % saved, estimated cost saved, store size).".into(),
            input_schema: ToolInputSchema {
                schema_type: "object".into(),
                properties: None,
                required: None,
            },
        },
        Tool {
            name: "parallel_jobs".into(),
            description: "Fan out N distinct jobs to running MUR agents in parallel over an ephemeral channel — no workflow file. Each job is delegated as its own concurrent turn. Before coding fan-out, apply the parallel-code gate: disjoint files (no shared registry/lockfile), contracts frozen first, one writer per file. Targets the agents you name; runtimes must already be running.".into(),
            input_schema: ToolInputSchema {
                schema_type: "object".into(),
                properties: Some(BTreeMap::from([
                    ("jobs".into(), ToolParam {
                        param_type: "array".into(),
                        description: "Jobs to run in parallel. Each: { description: string, agent?: string }.".into(),
                        default: None,
                    }),
                    ("agent".into(), ToolParam {
                        param_type: "string".into(),
                        description: "Default assignee agent name for jobs that omit their own `agent`.".into(),
                        default: None,
                    }),
                    ("max_concurrency".into(), ToolParam {
                        param_type: "integer".into(),
                        description: "Max jobs in flight at once, 1-32 (default 8).".into(),
                        default: Some(json!(8)),
                    }),
                    ("yes".into(), ToolParam {
                        param_type: "boolean".into(),
                        description: "Auto-approve risk-tiered steps. Default false (fail-closed).".into(),
                        default: Some(json!(false)),
                    }),
                ])),
                required: Some(vec!["jobs".into()]),
            },
        },
    ]
}

/// Tool names whose outputs must never be auto-compressed.
const AUTO_COMPRESS_SKIP: &[&str] = &["mur_compress", "mur_retrieve", "mur_compress_stats"];

/// Public entry point: dispatch the tool, then size-gate auto-compress the
/// result (Surface 1) — the boundary at which the model reads MUR tool output.
pub async fn call_tool(name: &str, arguments: &Value) -> Result<Value, String> {
    let out = dispatch_tool(name, arguments).await?;
    Ok(maybe_compress_tool_output(name, arguments, out))
}

/// Apply size-gated auto-compression to a tool result. Unit-testable: takes an
/// explicit engine + auto config; no env/filesystem beyond the engine.
fn apply_auto_compress(
    engine: &CompressEngine,
    auto: &AutoCfg,
    name: &str,
    arguments: &Value,
    out: Value,
) -> Value {
    if !auto.enabled || !auto.mcp || AUTO_COMPRESS_SKIP.contains(&name) {
        return out;
    }
    // args["query"] (when present) makes search-style tools query-aware (BM25-retrievable).
    let query = arguments.get("query").and_then(|v| v.as_str());
    // Guarded variant: even on this success surface, scan for embedded error
    // signals so an error-bearing payload is passed through (not offloaded) and
    // any residual bulk offload is annotated with its error count.
    match mur_compress::auto_compress_value_guarded(engine, &out, query, auto.min_tokens, false) {
        Some(replacement) => replacement,
        None => out,
    }
}

/// Build the per-call engine and apply auto-compression. Falls back to the
/// uncompressed output if the engine can't be built.
fn maybe_compress_tool_output(name: &str, arguments: &Value, out: Value) -> Value {
    let engine = match compress_engine() {
        Ok(e) => e,
        Err(_) => return out,
    };
    let auto = engine.config().auto.clone();
    apply_auto_compress(&engine, &auto, name, arguments, out)
}

/// Dispatch a tool call by name. Returns the result as a JSON Value.
async fn dispatch_tool(name: &str, arguments: &Value) -> Result<Value, String> {
    match name {
        "mur_notes_search" => {
            let query = arguments
                .get("query")
                .and_then(|v| v.as_str())
                .ok_or_else(|| "Missing required parameter: 'query' (string)".to_string())?;
            let limit = arguments
                .get("limit")
                .and_then(|v| v.as_u64())
                .unwrap_or(5)
                .clamp(1, 10) as usize;

            let home =
                resolve_mur_home().map_err(|e| format!("Failed to resolve MUR home: {}", e))?;
            let results = notes_cmd::do_search(&home, query, limit)
                .map_err(|e| format!("Search failed: {}", e))?;

            let items: Vec<Value> = results
                .iter()
                .map(|scored| {
                    json!({
                        "name": scored.item.manifest.name,
                        "description": scored.item.manifest.description,
                        "score": scored.score,
                        "maturity": format!("{:?}", scored.item.stats.lifecycle_state),
                    })
                })
                .collect();

            Ok(json!({
                "results": items,
                "count": items.len(),
            }))
        }

        "mur_notes_show" => {
            let name = arguments
                .get("name")
                .and_then(|v| v.as_str())
                .ok_or_else(|| "Missing required parameter: 'name' (string)".to_string())?;

            let home =
                resolve_mur_home().map_err(|e| format!("Failed to resolve MUR home: {}", e))?;
            let view =
                notes_cmd::do_show(&home, name).map_err(|e| format!("Note not found: {}", e))?;

            Ok(json!({
                "name": view.name,
                "description": view.description,
                "maturity": format!("{:?}", view.maturity),
                "body": view.body,
            }))
        }

        "mur_project_search" => {
            let query = arguments
                .get("query")
                .and_then(|v| v.as_str())
                .ok_or_else(|| "Missing required parameter: 'query' (string)".to_string())?;
            let project = arguments.get("project").and_then(|v| v.as_str());
            let limit = arguments
                .get("limit")
                .and_then(|v| v.as_u64())
                .unwrap_or(5)
                .clamp(1, 10) as usize;
            // Default to the current project (the dir the server runs in); set
            // `all: true` to search every indexed project.
            let all = arguments
                .get("all")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);

            let result = mur_core::cmd::project::do_project_search(query, project, limit, all)
                .await
                .map_err(|e| format!("Project search failed: {}", e))?;

            let snippets: Vec<Value> = result
                .chunks
                .iter()
                .map(|c| {
                    json!({
                        "file": c.file,
                        "lines": format!("{}-{}", c.line_start, c.line_end),
                        "content": c.content,
                        "score": c.score,
                        "project": c.project,
                    })
                })
                .collect();

            Ok(json!({
                "results": snippets,
                "count": result.total_hits,
            }))
        }

        "mur_project_status" => {
            let status = mur_core::cmd::project::do_project_status(None)
                .map_err(|e| format!("Project status failed: {}", e))?;
            let list = mur_core::cmd::project::do_project_list().unwrap_or_default();

            Ok(json!({
                "current_project": status,
                "all_indexed": list,
            }))
        }

        "mur_agent_status" => {
            if let Some(name) = arguments.get("name").and_then(|v| v.as_str()) {
                let status = mur_core::cmd::agent::lifecycle::do_status(name)
                    .map_err(|e| format!("Agent status failed: {}", e))?;
                Ok(serde_json::to_value(status).unwrap_or(Value::Null))
            } else {
                let list = mur_core::cmd::agent::lifecycle::do_list()
                    .map_err(|e| format!("Agent list failed: {}", e))?;
                Ok(json!({ "agents": list }))
            }
        }

        "mur_hook_context" => {
            let query = arguments
                .get("query")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            let compact = arguments
                .get("compact")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            let budget = arguments
                .get("budget")
                .and_then(|v| v.as_u64())
                .unwrap_or(2000) as usize;

            let result = mur_core::cmd::context::do_context(query, compact, budget)
                .await
                .map_err(|e| format!("Context retrieval failed: {}", e))?;

            Ok(json!({
                "patterns": result.patterns,
                "project": result.project_context,
                "token_count": result.token_count,
            }))
        }

        "vlc_open" => {
            let source = arguments
                .get("source")
                .and_then(|v| v.as_str())
                .ok_or_else(|| "Missing required parameter: 'source' (string)".to_string())?;
            let status = mur_core::cmd::media::vlc::open(source)
                .await
                .map_err(|e| format!("vlc_open failed: {}", e))?;
            Ok(serde_json::to_value(status).unwrap_or(Value::Null))
        }

        "vlc_playback" => {
            let action = arguments
                .get("action")
                .and_then(|v| v.as_str())
                .ok_or_else(|| "Missing required parameter: 'action' (string)".to_string())?;
            let value = arguments.get("value").and_then(|v| v.as_f64());
            let status = mur_core::cmd::media::vlc::playback(action, value)
                .await
                .map_err(|e| format!("vlc_playback failed: {}", e))?;
            Ok(serde_json::to_value(status).unwrap_or(Value::Null))
        }

        "vlc_status" => {
            let status = mur_core::cmd::media::vlc::status()
                .await
                .map_err(|e| format!("vlc_status failed: {}", e))?;
            Ok(serde_json::to_value(status).unwrap_or(Value::Null))
        }

        "scene_explain" => {
            let prompt = arguments.get("prompt").and_then(|v| v.as_str());
            let text = mur_core::cmd::media::scene::explain(prompt)
                .await
                .map_err(|e| format!("scene_explain failed: {}", e))?;
            Ok(json!({ "explanation": text }))
        }

        "video_analyze" => {
            let source = arguments.get("source").and_then(|v| v.as_str());
            let mode = arguments.get("mode").and_then(|v| v.as_str());
            let focus = arguments.get("focus").and_then(|v| v.as_str());
            let markdown = mur_core::cmd::media::analyze::analyze(source, mode, focus)
                .await
                .map_err(|e| format!("video_analyze failed: {}", e))?;
            Ok(json!({ "analysis": markdown }))
        }

        "watch_start" => {
            let home = resolve_mur_home().map_err(|e| format!("watch_start failed: {e}"))?;
            let s = mur_core::cmd::media::watch::start(&home)
                .map_err(|e| format!("watch_start failed: {e}"))?;
            Ok(serde_json::to_value(s).unwrap_or(Value::Null))
        }
        "watch_stop" => {
            let home = resolve_mur_home().map_err(|e| format!("watch_stop failed: {e}"))?;
            let s = mur_core::cmd::media::watch::stop(&home)
                .map_err(|e| format!("watch_stop failed: {e}"))?;
            Ok(serde_json::to_value(s).unwrap_or(Value::Null))
        }
        "watch_mute" => {
            let home = resolve_mur_home().map_err(|e| format!("watch_mute failed: {e}"))?;
            let s = mur_core::cmd::media::watch::mute(&home)
                .map_err(|e| format!("watch_mute failed: {e}"))?;
            Ok(serde_json::to_value(s).unwrap_or(Value::Null))
        }
        "watch_status" => {
            let home = resolve_mur_home().map_err(|e| format!("watch_status failed: {e}"))?;
            let s = mur_core::cmd::media::watch::status(&home);
            Ok(serde_json::to_value(s).unwrap_or(Value::Null))
        }

        "mur_compress" => {
            let content = arguments
                .get("content")
                .and_then(|v| v.as_str())
                .ok_or_else(|| "Missing required parameter: 'content' (string)".to_string())?;
            let query = arguments.get("query").and_then(|v| v.as_str());

            let eng = compress_engine()?;
            let r = eng.compress(content, query);
            let note = match &r.hash {
                Some(h) => format!(
                    "Original stored with hash={h}. Use mur_retrieve to fetch full content."
                ),
                None => "No content offloaded; nothing to retrieve.".to_string(),
            };
            Ok(json!({
                "compressed": r.compressed,
                "hash": r.hash,
                "content_type": r.content_type.as_str(),
                "original_tokens": r.original_tokens,
                "compressed_tokens": r.compressed_tokens,
                "tokens_saved": r.tokens_saved,
                "savings_percent": r.savings_percent,
                "transforms": r.transforms,
                "note": note,
            }))
        }

        "mur_retrieve" => {
            let hash = arguments
                .get("hash")
                .and_then(|v| v.as_str())
                .ok_or_else(|| "Missing required parameter: 'hash' (string)".to_string())?;
            let query = arguments.get("query").and_then(|v| v.as_str());

            let eng = compress_engine()?;
            match eng.retrieve(hash, query) {
                RetrieveResult::Full {
                    content_type,
                    original_content,
                    item_count,
                } => Ok(json!({
                    "hash": hash,
                    "content_type": content_type,
                    "original_content": original_content,
                    "item_count": item_count,
                })),
                RetrieveResult::Filtered {
                    query,
                    results,
                    count,
                } => Ok(json!({
                    "hash": hash,
                    "query": query,
                    "results": results,
                    "count": count,
                })),
                RetrieveResult::NotFound => Ok(json!({
                    "error": "Content not found or expired.",
                    "hash": hash,
                    "hint": "The hash may be wrong or the entry's TTL has elapsed.",
                })),
            }
        }

        "mur_compress_stats" => {
            let eng = compress_engine()?;
            let s = eng.stats_snapshot();
            Ok(json!({
                "compressions": s.compressions,
                "retrievals": s.retrievals,
                "total_input_tokens": s.total_input_tokens,
                "total_output_tokens": s.total_output_tokens,
                "total_tokens_saved": s.total_tokens_saved,
                "savings_percent": s.savings_percent,
                "estimated_cost_saved_usd": s.estimated_cost_saved_usd,
                "buckets": s.buckets,
                "store": { "entries": s.store_entries, "bytes": s.store_bytes },
            }))
        }

        "parallel_jobs" => {
            // Input guardrails (not behaviour config — see spec §3).
            const MAX_JOBS: usize = 32;
            const DEFAULT_MAX_CONCURRENCY: u64 = 8;

            let raw = arguments
                .get("jobs")
                .and_then(|v| v.as_array())
                .ok_or_else(|| "Missing required parameter: 'jobs' (array)".to_string())?;
            if raw.is_empty() || raw.len() > MAX_JOBS {
                return Err(format!(
                    "'jobs' must have 1..={MAX_JOBS} entries (got {})",
                    raw.len()
                ));
            }
            let jobs_in: Vec<mur_core::executor::jobs::RawJob> = raw
                .iter()
                .map(|j| mur_core::executor::jobs::RawJob {
                    description: j
                        .get("description")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string(),
                    agent: j
                        .get("agent")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string()),
                })
                .collect();
            let default_agent = arguments.get("agent").and_then(|v| v.as_str());
            let max_concurrency = arguments
                .get("max_concurrency")
                .and_then(|v| v.as_u64())
                .unwrap_or(DEFAULT_MAX_CONCURRENCY)
                .clamp(1, MAX_JOBS as u64) as usize;
            let yes = arguments
                .get("yes")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);

            let home = resolve_mur_home().map_err(|e| format!("parallel_jobs failed: {e}"))?;
            let jobs = mur_core::executor::jobs::resolve_jobs(&home, &jobs_in, default_agent)
                .map_err(|e| format!("parallel_jobs: {e}"))?;
            let (channel_id, out) = mur_core::executor::jobs::run_parallel_jobs(
                &home,
                &jobs,
                Some(max_concurrency),
                yes,
            )
            .await
            .map_err(|e| format!("parallel_jobs failed: {e}"))?;
            Ok(json!({
                "channel_id": channel_id,
                "output": out.output_text.unwrap_or_default(),
            }))
        }

        _ => Err(format!("Unknown tool: {}", name)),
    }
}

/// Resolve ~/.mur from environment or default.
fn resolve_mur_home() -> anyhow::Result<std::path::PathBuf> {
    mur_core::cmd::resolve_mur_home()
}

#[cfg(test)]
mod media_tool_tests {
    use super::*;
    #[test]
    fn media_tools_registered() {
        let names: Vec<_> = all_tools().into_iter().map(|t| t.name).collect();
        for n in [
            "vlc_open",
            "vlc_playback",
            "vlc_status",
            "scene_explain",
            "video_analyze",
            "watch_start",
            "watch_stop",
            "watch_mute",
            "watch_status",
        ] {
            assert!(names.contains(&n.to_string()), "missing {n}");
        }
    }
}

#[cfg(test)]
mod auto_compress_tests {
    use super::*;
    use mur_compress::{CompressConfig, CompressEngine};
    use serde_json::json;

    fn engine() -> (tempfile::TempDir, CompressEngine) {
        let dir = tempfile::tempdir().unwrap();
        let eng = CompressEngine::new(dir.path().to_path_buf(), CompressConfig::default()).unwrap();
        (dir, eng)
    }

    fn big_search_output() -> Value {
        let results: Vec<Value> = (0..3000)
            .map(|i| json!({"file": format!("src/f{i}.rs"), "score": 0.5, "content": format!("fn item_{i}() {{}}")}))
            .collect();
        json!({"results": results, "count": 3000})
    }

    #[test]
    fn skips_compression_tools() {
        let (_dir, eng) = engine();
        let auto = AutoCfg {
            enabled: true,
            min_tokens: 1,
            mcp: true,
            agent_runtime: true,
            claude_hook: true,
        };
        let big = big_search_output();
        let out = apply_auto_compress(&eng, &auto, "mur_compress", &json!({}), big.clone());
        assert_eq!(out, big, "compression tools must pass through");
    }

    #[test]
    fn small_output_unchanged() {
        let (_dir, eng) = engine();
        let auto = AutoCfg::default();
        let small = json!({"results": ["a", "b"], "count": 2});
        let out = apply_auto_compress(
            &eng,
            &auto,
            "mur_project_search",
            &json!({"query": "x"}),
            small.clone(),
        );
        assert_eq!(out, small);
    }

    #[test]
    fn large_search_output_compressed_with_query() {
        let (_dir, eng) = engine();
        let auto = AutoCfg {
            enabled: true,
            min_tokens: 50,
            mcp: true,
            agent_runtime: true,
            claude_hook: true,
        };
        let out = apply_auto_compress(
            &eng,
            &auto,
            "mur_project_search",
            &json!({"query": "item"}),
            big_search_output(),
        );
        assert_eq!(out["count"], json!(3000));
        assert_eq!(out["results"]["compressed"], json!(true));
        assert!(out["results"]["hash"].as_str().is_some());
        assert!(
            out["results"]["note"]
                .as_str()
                .unwrap()
                .contains("mur_retrieve")
        );
    }

    #[test]
    fn disabled_auto_passes_through() {
        let (_dir, eng) = engine();
        let auto = AutoCfg {
            enabled: false,
            ..AutoCfg::default()
        };
        let big = big_search_output();
        let out = apply_auto_compress(&eng, &auto, "mur_project_search", &json!({}), big.clone());
        assert_eq!(out, big);
    }
}
