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

/// Return all registered tools. `ast_grep_search` appears only when the
/// pinned binary is installed under mur home.
pub fn all_tools() -> Vec<Tool> {
    let mut tools = catalog::base_tools();
    if let Ok(home) = resolve_mur_home()
        && ast_grep::resolve_binary(&home).is_some()
    {
        tools.push(ast_grep::tool());
    }
    tools
}

/// Tool names whose outputs must never be auto-compressed.
const AUTO_COMPRESS_SKIP: &[&str] = &[
    "mur_compress",
    "mur_retrieve",
    "mur_compress_stats",
    "mur_job_status",
];

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
        ast_grep::TOOL_NAME => {
            let home = resolve_mur_home().map_err(|e| format!("{name} failed: {e}"))?;
            ast_grep::call(&home, arguments).await
        }
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

            // Say what was searched. `do_search` reads the GLOBAL note store
            // (`~/.mur/skills`) and never an agent's own memories, so a bare
            // `count: 0` reads as "you remember nothing" when it means "nothing
            // global matched". An agent asking what it remembers should use its
            // built-in `recall`, which reads the set its own prompt was built
            // from.
            let mut out = json!({
                "results": items,
                "count": items.len(),
                "scope": "global notes only (~/.mur/skills)",
            });
            if items.is_empty() {
                out["note"] = json!(
                    "No GLOBAL note matched. This does not cover an agent's own memories — \
                     an agent should call its built-in `recall` tool instead; from the CLI, \
                     `mur notes list --agent <name>`."
                );
            }
            Ok(out)
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
            // Where the jobs are routed (#1607). Without this the member only
            // ever saw the job text and built in whatever directory it sat in.
            let cwd = mur_core::executor::jobs::RunCwd::from_tool_args(
                arguments.get("cwd").and_then(|v| v.as_str()),
                arguments
                    .get("cwd_inferred")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false),
            );

            let home = resolve_mur_home().map_err(|e| format!("parallel_jobs failed: {e}"))?;
            let jobs = mur_core::executor::jobs::resolve_jobs(&home, &jobs_in, default_agent)
                .map_err(|e| format!("parallel_jobs: {e}"))?;
            // Dispatch, do not await (spec 2026-09-12 execution-limits §3.6):
            // the handle is the reply and mur_job_status is the progress.
            // Dropping the JoinHandle detaches the task; it runs on this
            // server's runtime for as long as the server lives.
            let d = mur_core::executor::jobs::dispatch_parallel_jobs(
                &home,
                &jobs,
                Some(max_concurrency),
                yes,
                &cwd,
            )
            .await
            .map_err(|e| format!("parallel_jobs failed: {e}"))?;
            let (run_id, channel_id) = (d.run_id.clone(), d.channel_id.clone());
            drop(d.handle);
            Ok(json!({
                "run_id": run_id,
                "channel_id": channel_id,
                "status": "dispatched",
                "jobs": jobs.len(),
                "follow": format!("mur_job_status {run_id} — each job's reply lands in channel {channel_id}"),
            }))
        }

        "mur_job_status" => {
            let mur_home = resolve_mur_home().map_err(|e| format!("mur_job_status failed: {e}"))?;
            let run_id = arguments
                .get("run_id")
                .and_then(|v| v.as_str())
                .ok_or_else(|| "Missing required parameter: 'run_id' (string)".to_string())?;

            let loaded = mur_core::run_status::status_of(&mur_home, run_id)
                .map_err(|e| format!("read run {run_id}: {e}"))?;
            let Some(status) = loaded else {
                return Ok(Value::String(format!(
                    "no run recorded for `{run_id}` — it may predate run recording, or the id may be wrong"
                )));
            };

            let liveness = match status.liveness {
                mur_core::run_status::Liveness::Alive => "alive",
                mur_core::run_status::Liveness::Stalled => "STALLED",
                mur_core::run_status::Liveness::Dead => "DEAD",
                mur_core::run_status::Liveness::Unknown => "unknown",
                mur_core::run_status::Liveness::NotApplicable => "n/a",
            };
            // The same STATE cell `mur job status` prints, so a long-dead run
            // reads `abandoned` here too instead of an agent polling a
            // `running` corpse forever.
            let state = mur_core::cmd::job::state_cell(&status);
            let mut output = format!(
                "run {} — state: {state}, liveness: {liveness}\nlabel: {}\nstarted: {}\nsteps: {}",
                status.run.run_id,
                status.run.label,
                status.run.started_at.to_rfc3339(),
                status.run.steps.len()
            );
            if status.run.kind == mur_core::run_status::RunKind::Fleet
                && let Some(view) =
                    mur_core::cmd::fleet::progress::load_view(&mur_home, &status.run.label)
                && view.progress.run_id == run_id
            {
                output.push_str("\nprogress: ");
                output.push_str(&mur_core::cmd::fleet::progress::iteration_summary_line(
                    &view.progress,
                ));
                for step in
                    view.progress.steps.iter().filter(|step| {
                        step.state == mur_core::cmd::fleet::progress::StepState::Running
                    })
                {
                    output.push_str("\n  running: ");
                    output.push_str(&step.desc);
                }
            }
            Ok(Value::String(output))
        }

        _ => Err(format!("Unknown tool: {}", name)),
    }
}

/// Resolve ~/.mur from environment or default.
fn resolve_mur_home() -> anyhow::Result<std::path::PathBuf> {
    mur_core::cmd::resolve_mur_home()
}

mod ast_grep;
mod catalog;

#[cfg(test)]
mod media_tool_tests;

#[cfg(test)]
mod auto_compress_tests;

#[cfg(test)]
mod job_status_tests;
