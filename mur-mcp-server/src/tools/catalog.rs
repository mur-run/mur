//! Static catalog of the always-present MCP tools. Conditionally listed tools
//! (e.g. `ast_grep_search`) are appended by `tools::all_tools`.

use std::collections::BTreeMap;

use serde_json::json;

use super::{Tool, ToolInputSchema, ToolParam};

pub(super) fn base_tools() -> Vec<Tool> {
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
            description: "Fan out N distinct jobs to running MUR agents in parallel and return a handle at once: {run_id, channel_id, status: dispatched}. Poll mur_job_status <run_id> for progress; each job's reply lands in the channel. The run lives in this MCP server process — if the server exits, the run ends. Before coding fan-out, apply the parallel-code gate: disjoint files (no shared registry/lockfile), contracts frozen first, one writer per file. Targets the agents you name; runtimes must already be running. Pass `cwd` (absolute) when the jobs belong to a specific project; each job is told to work there, and each agent must be allowed to write there (an approval is parked otherwise).".into(),
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
                    ("cwd".into(), ToolParam {
                        param_type: "string".into(),
                        description: "Absolute path of the project the jobs should work in — the TARGET, not wherever you happen to be sitting. Every job is told to work there. Omitted: your session directory is assumed and the jobs are told it was a guess.".into(),
                        default: None,
                    }),
                    ("cwd_inferred".into(), ToolParam {
                        param_type: "boolean".into(),
                        description: "Set by the agent runtime when it filled `cwd` in from the session directory; marks the routing as assumed. Not for models to set.".into(),
                        default: Some(json!(false)),
                    }),
                ])),
                required: Some(vec!["jobs".into()]),
            },
        },
        Tool {
            name: "mur_job_status".into(),
            description: "Report the live status of a MUR run (a parallel_jobs dispatch, a fleet_run, or a workflow run) by its run_id. Returns both a semantic state (running / blocked / done / failed / stopped / abandoned — the process died without recording a result; not a failure verdict) and a liveness verdict (alive / STALLED / DEAD / unknown). Use this after parallel_jobs or fleet_run hand you a run_id — poll here instead of re-dispatching.".into(),
            input_schema: ToolInputSchema {
                schema_type: "object".into(),
                properties: Some(BTreeMap::from([(
                    "run_id".into(),
                    ToolParam {
                        param_type: "string".into(),
                        description: "The run id returned when the run was dispatched.".into(),
                        default: None,
                    },
                )])),
                required: Some(vec!["run_id".into()]),
            },
        },
    ]
}
