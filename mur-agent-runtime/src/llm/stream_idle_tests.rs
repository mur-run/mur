//! Stream-idle behaviour for every provider that owns an SSE/NDJSON loop
//! (#1287).
//!
//! One file rather than three test modules, for two reasons. The harness below
//! — a listener that writes some bytes, waits, and then either closes or holds
//! the socket open — is identical for all three, and `anthropic.rs` is already
//! 1400 lines against CLAUDE.md §4's 800, so adding two hundred more there
//! makes a pre-existing violation worse for no benefit.
//!
//! **These tests run on a REAL clock, deliberately.** The first draft used
//! `#[tokio::test]` so the minutes-long production bounds
//! would cost no wall time, and it was wrong: with a paused clock a task
//! blocked on socket I/O counts as idle, so tokio advances straight to the
//! fake server's next sleep deadline and every gap this harness tries to
//! create collapses to nothing. A discriminator caught it — the
//! fragments-count-as-activity test passed, and then passed again with the
//! bound set BELOW the gap, which it could only do if the gap did not exist.
//!
//! So the bounds here are one second and the gaps are hundreds of
//! milliseconds. A few seconds of real test time buys gaps that are actually
//! gaps. The timing rule itself is still unit-tested on a paused clock in
//! `stream_activity_tests`, where nothing is blocked on I/O.

use super::{LlmClient, LlmError, LlmRequest, RichMessage, StopReason, StreamDelta};
use std::net::SocketAddr;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

/// One instruction for the fake model server.
enum Step {
    /// Write these bytes now.
    Send(&'static str),
    /// Go quiet for this many milliseconds of REAL time. See the module note:
    /// a paused clock cannot express this.
    Quiet(u64),
    /// Stop writing and never close — the shape of a model that stopped
    /// sending. Closing instead would be a clean end of stream, which is a
    /// different case entirely.
    Stall,
}

/// Serve one request from a script, then hold or close per the script.
async fn serve(script: Vec<Step>) -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let Ok((mut s, _)) = listener.accept().await else {
            return;
        };
        let mut buf = [0u8; 8192];
        let _ = s.read(&mut buf).await;
        let _ = s
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n\r\n")
            .await;
        let _ = s.flush().await;
        for step in script {
            match step {
                Step::Send(body) => {
                    let _ = s.write_all(body.as_bytes()).await;
                    let _ = s.flush().await;
                }
                Step::Quiet(ms) => {
                    tokio::time::sleep(std::time::Duration::from_millis(ms)).await;
                }
                Step::Stall => {
                    // Hold the socket open forever. `s` stays alive with it.
                    std::future::pending::<()>().await;
                }
            }
        }
        let _ = s.shutdown().await;
    });
    addr
}

/// Bounds in seconds — one is the smallest the parser accepts, and it is long
/// enough that a few hundred milliseconds of gap is comfortably inside it.
fn set_bounds(first: u64, idle: u64) {
    // SAFETY: nextest runs each test in its own process.
    unsafe {
        std::env::set_var("MUR_LLM_FIRST_DELTA_TIMEOUT_SECS", first.to_string());
        std::env::set_var("MUR_LLM_IDLE_TIMEOUT_SECS", idle.to_string());
    }
}

fn http() -> reqwest::Client {
    super::llm_client_builder().build().unwrap()
}

fn req() -> LlmRequest {
    LlmRequest {
        messages: vec![RichMessage::Text {
            role: "user".into(),
            content: "hi".into(),
        }],
        ..Default::default()
    }
}

fn sink() -> (
    tokio::sync::mpsc::Sender<StreamDelta>,
    tokio::sync::mpsc::Receiver<StreamDelta>,
) {
    tokio::sync::mpsc::channel(256)
}

fn drain(rx: &mut tokio::sync::mpsc::Receiver<StreamDelta>) -> Vec<StreamDelta> {
    let mut out = Vec::new();
    while let Ok(d) = rx.try_recv() {
        out.push(d);
    }
    out
}

// ── Ollama ──────────────────────────────────────────────────────────────────

fn ollama(addr: SocketAddr) -> super::ollama::OllamaClient {
    super::ollama::OllamaClient::with_http_client(
        format!("http://{addr}"),
        "llama3.2:3b".into(),
        http(),
    )
}

/// A stream whose gaps are inside the bound is untouched: every delta
/// forwarded, `EndTurn` preserved, no marker.
#[tokio::test]
async fn ollama_a_stream_inside_the_bound_is_untouched() {
    set_bounds(30, 1);
    let addr = serve(vec![
        Step::Send("{\"message\":{\"content\":\"one \"}}\n"),
        Step::Quiet(200),
        Step::Send("{\"message\":{\"content\":\"two\"}}\n"),
        Step::Quiet(200),
        Step::Send("{\"done\":true}\n"),
    ])
    .await;
    let (tx, mut rx) = sink();
    let resp = ollama(addr).generate_stream(req(), tx).await.expect("ok");
    assert_eq!(resp.text, "one two");
    assert_eq!(resp.stop_reason, StopReason::EndTurn);
    assert_eq!(drain(&mut rx).len(), 2);
}

/// The bug #1287 is about, from the other side: generation that legitimately
/// takes longer than any former total timeout must complete. 400 virtual
/// seconds of steady output, well past the old 60 s ceiling.
#[tokio::test]
async fn ollama_slow_but_steady_generation_is_never_cut_off() {
    set_bounds(30, 1);
    let mut script = vec![Step::Send("{\"message\":{\"content\":\"x\"}}\n")];
    for _ in 0..20 {
        script.push(Step::Quiet(300));
        script.push(Step::Send("{\"message\":{\"content\":\"x\"}}\n"));
    }
    script.push(Step::Send("{\"done\":true}\n"));
    let addr = serve(script).await;
    let (tx, _rx) = sink();
    let resp = ollama(addr).generate_stream(req(), tx).await.expect("ok");
    assert_eq!(
        resp.text.len(),
        21,
        "every chunk survived 400s of streaming"
    );
    assert_eq!(resp.stop_reason, StopReason::EndTurn);
}

/// Stopped sending after two deltas: the partial text is kept and marked, and
/// the sink got exactly the two real deltas. The marker is `task_runner`'s job
/// (D6a), not the client's, so it must NOT appear here.
#[tokio::test]
async fn ollama_a_stream_that_stops_sending_yields_its_partial() {
    set_bounds(30, 1);
    let addr = serve(vec![
        Step::Send("{\"message\":{\"content\":\"half \"}}\n"),
        Step::Send("{\"message\":{\"content\":\"an answer\"}}\n"),
        Step::Stall,
    ])
    .await;
    let (tx, mut rx) = sink();
    let resp = ollama(addr).generate_stream(req(), tx).await.expect("ok");
    assert_eq!(resp.text, "half an answer");
    assert_eq!(resp.stop_reason, StopReason::Interrupted);
    assert!(
        !resp.text.contains("truncated"),
        "the marker belongs to task_runner, not the client: {:?}",
        resp.text
    );
    assert_eq!(drain(&mut rx).len(), 2);
}

/// Never sent anything: nothing reached the sink, so a retry cannot duplicate
/// anything and `Timeout` is the right class.
#[tokio::test]
async fn ollama_a_stream_that_never_starts_is_a_timeout() {
    set_bounds(1, 1);
    let addr = serve(vec![Step::Stall]).await;
    let (tx, mut rx) = sink();
    let err = ollama(addr).generate_stream(req(), tx).await.unwrap_err();
    assert!(matches!(err, LlmError::Timeout), "{err:?}");
    assert!(drain(&mut rx).is_empty());
}

/// Thinking output is activity. A reasoning model that thinks for longer than
/// the idle bound before its first visible token is alive, and its reasoning
/// must not end up in the answer.
#[tokio::test]
async fn ollama_thinking_keeps_the_stream_alive_and_stays_out_of_the_answer() {
    set_bounds(30, 1);
    let addr = serve(vec![
        Step::Send("{\"message\":{\"thinking\":\"hmm \"}}\n"),
        Step::Quiet(300),
        Step::Send("{\"message\":{\"thinking\":\"still hmm \"}}\n"),
        Step::Quiet(300),
        Step::Send("{\"message\":{\"thinking\":\"nearly \"}}\n"),
        Step::Quiet(300),
        Step::Send("{\"message\":{\"content\":\"answer\"}}\n"),
        Step::Send("{\"done\":true}\n"),
    ])
    .await;
    let (tx, mut rx) = sink();
    let resp = ollama(addr).generate_stream(req(), tx).await.expect("ok");
    assert_eq!(resp.text, "answer", "thinking must not reach the answer");
    assert_eq!(resp.stop_reason, StopReason::EndTurn);
    let deltas = drain(&mut rx);
    assert_eq!(deltas.iter().filter(|d| d.thinking).count(), 3);
}

// ── OpenAI ──────────────────────────────────────────────────────────────────

fn openai(addr: SocketAddr) -> super::openai::OpenAiClient {
    super::openai::OpenAiClient::from_secret_string_with_http(
        &secrecy::SecretString::from("k".to_string()),
        "gpt-test".into(),
        Some(format!("http://{addr}/v1")),
        http(),
    )
}

#[tokio::test]
async fn openai_a_stream_inside_the_bound_is_untouched() {
    set_bounds(30, 1);
    let addr = serve(vec![
        Step::Send("data: {\"choices\":[{\"delta\":{\"content\":\"one \"}}]}\n\n"),
        Step::Quiet(200),
        Step::Send("data: {\"choices\":[{\"delta\":{\"content\":\"two\"}}]}\n\n"),
        Step::Send("data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n"),
        Step::Send("data: [DONE]\n\n"),
    ])
    .await;
    let (tx, mut rx) = sink();
    let resp = openai(addr).generate_stream(req(), tx).await.expect("ok");
    assert_eq!(resp.text, "one two");
    assert_eq!(resp.stop_reason, StopReason::EndTurn);
    assert_eq!(drain(&mut rx).len(), 2);
}

/// **F1's regression guard.** One text delta, then nothing but `tool_calls`
/// fragments for far longer than the idle bound, then a normal end.
///
/// This is the test the first design would have failed. Tool-call fragments
/// emit NO `StreamDelta` — they accumulate into a partial map — so a guard
/// timing the gaps between sink deltas sees this stream as idle from the
/// moment the text stops, aborts it mid-call, and (worse) returns the leading
/// narration as a finished answer with the tool call thrown away.
#[tokio::test]
async fn openai_tool_argument_fragments_count_as_activity() {
    set_bounds(30, 1);
    let mut script = vec![
        Step::Send("data: {\"choices\":[{\"delta\":{\"content\":\"Running it now.\"}}]}\n\n"),
        Step::Send(
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"c1\",\
             \"function\":{\"name\":\"bash\",\"arguments\":\"{\\\"command\\\":\\\"\"}}]}}]}\n\n",
        ),
    ];
    // Eight fragments, 20 virtual seconds apart: 160 s of total silence at the
    // sink, against a 30 s idle bound.
    for _ in 0..8 {
        script.push(Step::Quiet(300));
        script.push(Step::Send(
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\
             \"function\":{\"arguments\":\"xy\"}}]}}]}\n\n",
        ));
    }
    script.push(Step::Send(
        "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\
         \"function\":{\"arguments\":\"\\\"}\"}}]}}]}\n\n",
    ));
    script.push(Step::Send(
        "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"tool_calls\"}]}\n\n",
    ));
    script.push(Step::Send("data: [DONE]\n\n"));
    let addr = serve(script).await;
    let (tx, _rx) = sink();
    let resp = openai(addr).generate_stream(req(), tx).await.expect("ok");
    assert_eq!(
        resp.stop_reason,
        StopReason::ToolUse,
        "a stream busy emitting tool arguments is not idle"
    );
    assert_eq!(resp.tool_calls.len(), 1, "the tool call must survive");
    assert_eq!(resp.tool_calls[0].tool_name, "bash");
    assert_eq!(
        resp.tool_calls[0].input["command"], "xyxyxyxyxyxyxyxy",
        "every fragment was accumulated"
    );
}

/// Interrupted mid-arguments: the half-finished call is dropped rather than
/// being run with arguments the model never chose. The leading text survives,
/// marked.
#[tokio::test]
async fn openai_a_call_interrupted_mid_arguments_is_dropped_not_guessed() {
    set_bounds(30, 1);
    let addr = serve(vec![
        Step::Send("data: {\"choices\":[{\"delta\":{\"content\":\"Deleting it.\"}}]}\n\n"),
        Step::Send(
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"c1\",\
             \"function\":{\"name\":\"bash\",\"arguments\":\"{\\\"command\\\":\\\"rm -rf \"}}]}}]}\n\n",
        ),
        Step::Stall,
    ])
    .await;
    let (tx, _rx) = sink();
    let resp = openai(addr).generate_stream(req(), tx).await.expect("ok");
    assert_eq!(resp.stop_reason, StopReason::Interrupted);
    assert_eq!(resp.text, "Deleting it.");
    assert!(
        resp.tool_calls.is_empty(),
        "half-parsed arguments must never become a runnable call: {:?}",
        resp.tool_calls
    );
}

/// A no-argument call is legitimately empty, so the interrupted path must not
/// mistake it for a half-finished one.
#[tokio::test]
async fn openai_a_no_argument_call_survives_an_interruption() {
    set_bounds(30, 1);
    let addr = serve(vec![
        Step::Send(
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"c1\",\
             \"function\":{\"name\":\"now\",\"arguments\":\"\"}}]}}]}\n\n",
        ),
        Step::Stall,
    ])
    .await;
    let (tx, _rx) = sink();
    let resp = openai(addr).generate_stream(req(), tx).await.expect("ok");
    assert_eq!(resp.stop_reason, StopReason::Interrupted);
    assert_eq!(resp.tool_calls.len(), 1);
    assert_eq!(resp.tool_calls[0].tool_name, "now");
}

/// Interrupted with nothing assembled at all: `Timeout`, so the fallback chain
/// may retry without duplicating anything.
#[tokio::test]
async fn openai_nothing_assembled_is_a_timeout() {
    set_bounds(1, 1);
    let addr = serve(vec![Step::Stall]).await;
    let (tx, mut rx) = sink();
    let err = openai(addr).generate_stream(req(), tx).await.unwrap_err();
    assert!(matches!(err, LlmError::Timeout), "{err:?}");
    assert!(drain(&mut rx).is_empty());
}

/// The discriminator for the test above, and the reason this whole file runs on
/// a real clock.
///
/// If the gaps were not real — collapsed by a paused clock, or delivered by the
/// OS as one chunk — the fragments test would pass for a trivial reason and
/// prove nothing. Same shape, one gap LONGER than the bound: it must interrupt.
/// On a paused clock this test failed, which is how the collapse was found.
#[tokio::test]
async fn openai_the_fragment_test_is_measuring_real_gaps() {
    set_bounds(30, 1);
    let addr = serve(vec![
        Step::Send("data: {\"choices\":[{\"delta\":{\"content\":\"Running it now.\"}}]}\n\n"),
        Step::Send(
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"c1\",\
             \"function\":{\"name\":\"bash\",\"arguments\":\"{\\\"command\\\":\\\"\"}}]}}]}\n\n",
        ),
        Step::Quiet(1500),
        Step::Send(
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\
             \"function\":{\"arguments\":\"xy\\\"}\"}}]}}]}\n\n",
        ),
        Step::Send("data: [DONE]\n\n"),
    ])
    .await;
    let (tx, _rx) = sink();
    let resp = openai(addr).generate_stream(req(), tx).await.expect("ok");
    assert_eq!(
        resp.stop_reason,
        StopReason::Interrupted,
        "a 20s gap must trip a 10s bound; if this says ToolUse the gaps are not real"
    );
    assert!(
        resp.tool_calls.is_empty(),
        "the call was still mid-arguments when the stream went quiet"
    );
}

// ── Anthropic ───────────────────────────────────────────────────────────────

fn anthropic(addr: SocketAddr) -> super::anthropic::AnthropicClient {
    super::anthropic::AnthropicClient::from_secret_string_with_http(
        &secrecy::SecretString::from("k".to_string()),
        "claude-opus-5".into(),
        Some(format!("http://{addr}")),
        http(),
    )
}

#[tokio::test]
async fn anthropic_a_stream_inside_the_bound_is_untouched() {
    set_bounds(30, 1);
    let addr = serve(vec![
        Step::Send(
            "data: {\"type\":\"content_block_delta\",\"delta\":\
             {\"type\":\"text_delta\",\"text\":\"one \"}}\n\n",
        ),
        Step::Quiet(200),
        Step::Send(
            "data: {\"type\":\"content_block_delta\",\"delta\":\
             {\"type\":\"text_delta\",\"text\":\"two\"}}\n\n",
        ),
        Step::Send(
            "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"}}\n\n",
        ),
    ])
    .await;
    let (tx, mut rx) = sink();
    let resp = anthropic(addr)
        .generate_stream(req(), tx)
        .await
        .expect("ok");
    assert_eq!(resp.text, "one two");
    assert_eq!(resp.stop_reason, StopReason::EndTurn);
    assert_eq!(drain(&mut rx).len(), 2);
}

/// **F1's guard on the Anthropic side.** `input_json_delta` returns `None` from
/// `apply_sse_event`, so a tool call's arguments reach the sink as nothing at
/// all. Six of them, 300 ms apart, against a one-second idle bound: 1.8 s of
/// sink silence that a sink-side guard would have called death.
#[tokio::test]
async fn anthropic_input_json_delta_counts_as_activity() {
    set_bounds(30, 1);
    let mut script = vec![
        Step::Send(
            "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":\
             {\"type\":\"tool_use\",\"id\":\"c1\",\"name\":\"bash\",\"input\":{}}}\n\n",
        ),
        Step::Send(
            "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":\
             {\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"command\\\":\\\"\"}}\n\n",
        ),
    ];
    for _ in 0..6 {
        script.push(Step::Quiet(300));
        script.push(Step::Send(
            "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":\
             {\"type\":\"input_json_delta\",\"partial_json\":\"ab\"}}\n\n",
        ));
    }
    script.push(Step::Send(
        "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":\
         {\"type\":\"input_json_delta\",\"partial_json\":\"\\\"}\"}}\n\n",
    ));
    script.push(Step::Send(
        "data: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
    ));
    script.push(Step::Send(
        "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"tool_use\"}}\n\n",
    ));
    let addr = serve(script).await;
    let (tx, _rx) = sink();
    let resp = anthropic(addr)
        .generate_stream(req(), tx)
        .await
        .expect("ok");
    assert_eq!(resp.stop_reason, StopReason::ToolUse);
    assert_eq!(resp.tool_calls.len(), 1);
    assert_eq!(resp.tool_calls[0].input["command"], "abababababab");
}

/// Interrupted mid-arguments. Nothing extra is needed to protect against a
/// half-emitted call here: `cur_tool` is only committed at
/// `content_block_stop`, which never arrived.
#[tokio::test]
async fn anthropic_a_call_interrupted_mid_arguments_never_appears() {
    set_bounds(30, 1);
    let addr = serve(vec![
        Step::Send(
            "data: {\"type\":\"content_block_delta\",\"delta\":\
             {\"type\":\"text_delta\",\"text\":\"Deleting it.\"}}\n\n",
        ),
        Step::Send(
            "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":\
             {\"type\":\"tool_use\",\"id\":\"c1\",\"name\":\"bash\",\"input\":{}}}\n\n",
        ),
        Step::Send(
            "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":\
             {\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"command\\\":\\\"rm -rf \"}}\n\n",
        ),
        Step::Stall,
    ])
    .await;
    let (tx, _rx) = sink();
    let resp = anthropic(addr)
        .generate_stream(req(), tx)
        .await
        .expect("ok");
    assert_eq!(resp.stop_reason, StopReason::Interrupted);
    assert_eq!(resp.text, "Deleting it.");
    assert!(resp.tool_calls.is_empty(), "{:?}", resp.tool_calls);
}

/// Thinking is activity, and thinking is not the answer. Anthropic's
/// `thinking_delta` arm deliberately never touches `acc.text`, and this is what
/// keeps it that way.
#[tokio::test]
async fn anthropic_thinking_keeps_the_stream_alive_and_stays_out_of_the_answer() {
    set_bounds(30, 1);
    let mut script = vec![];
    for _ in 0..6 {
        script.push(Step::Send(
            "data: {\"type\":\"content_block_delta\",\"delta\":\
             {\"type\":\"thinking_delta\",\"thinking\":\"hmm \"}}\n\n",
        ));
        script.push(Step::Quiet(300));
    }
    script.push(Step::Send(
        "data: {\"type\":\"content_block_delta\",\"delta\":\
         {\"type\":\"text_delta\",\"text\":\"answer\"}}\n\n",
    ));
    script.push(Step::Send(
        "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"}}\n\n",
    ));
    let addr = serve(script).await;
    let (tx, mut rx) = sink();
    let resp = anthropic(addr)
        .generate_stream(req(), tx)
        .await
        .expect("ok");
    assert_eq!(resp.text, "answer");
    assert_eq!(resp.stop_reason, StopReason::EndTurn);
    let deltas = drain(&mut rx);
    assert_eq!(deltas.iter().filter(|d| d.thinking).count(), 6);
}

#[tokio::test]
async fn anthropic_nothing_assembled_is_a_timeout_not_an_invalid_response() {
    set_bounds(1, 1);
    let addr = serve(vec![Step::Stall]).await;
    let (tx, _rx) = sink();
    let err = anthropic(addr)
        .generate_stream(req(), tx)
        .await
        .unwrap_err();
    assert!(
        matches!(err, LlmError::Timeout),
        "InvalidResponse would Stop the fallback chain; this must retry: {err:?}"
    );
}
