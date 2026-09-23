# FreeBSD platform compatibility audit

Reviewed: 2026-09-20

This inventory covers every Rust source line matched by the command below. The five release binaries are `mur`, `mur-mcp-server`, `murmurd`, `mur-agent-runtime`, and `mur-research-gateway`. GUI-only crates are recorded because the scan is repository-wide, but they are not reachable from those five artifacts.

## Verification

```sh
git grep -nE 'target_os|target_family|cfg!\(|std::env::consts::OS|systemd|launchd|notify-send|osascript' -- '*.rs' > /tmp/freebsd-platform-hits.txt
python3 scripts/check-freebsd-audit.py
```

The checker recomputes the same matches and fails for either missing or stale `file:line` rows.

## Inventory

| file:line | binary reachability | classification | required change | task |
|---|---|---|---|---|
| `mur-agent-gui/src-tauri/src/bootstrap.rs:210` | not reachable (GUI-only crate) | `unsupported` | No release-binary change; FreeBSD GUI is outside this support scope. | Task 9 |
| `mur-agent-gui/src-tauri/src/main.rs:55` | not reachable (GUI-only crate) | `unsupported` | No release-binary change; FreeBSD GUI is outside this support scope. | Task 9 |
| `mur-agent-gui/src-tauri/src/main.rs:258` | not reachable (GUI-only crate) | `unsupported` | No release-binary change; FreeBSD GUI is outside this support scope. | Task 9 |
| `mur-agent-gui/src-tauri/src/main.rs:502` | not reachable (GUI-only crate) | `unsupported` | No release-binary change; FreeBSD GUI is outside this support scope. | Task 9 |
| `mur-agent-gui/src-tauri/src/main.rs:508` | not reachable (GUI-only crate) | `unsupported` | No release-binary change; FreeBSD GUI is outside this support scope. | Task 9 |
| `mur-agent-gui/src-tauri/src/main.rs:566` | not reachable (GUI-only crate) | `unsupported` | No release-binary change; FreeBSD GUI is outside this support scope. | Task 9 |
| `mur-agent-gui/src-tauri/src/main.rs:571` | not reachable (GUI-only crate) | `unsupported` | No release-binary change; FreeBSD GUI is outside this support scope. | Task 9 |
| `mur-agent-gui/src-tauri/src/main.rs:576` | not reachable (GUI-only crate) | `unsupported` | No release-binary change; FreeBSD GUI is outside this support scope. | Task 9 |
| `mur-agent-gui/src-tauri/src/multimodal/decode.rs:119` | not reachable (GUI-only crate) | `unsupported` | No release-binary change; FreeBSD GUI is outside this support scope. | Task 9 |
| `mur-agent-gui/src-tauri/src/multimodal/heic.rs:31` | not reachable (GUI-only crate) | `unsupported` | No release-binary change; FreeBSD GUI is outside this support scope. | Task 9 |
| `mur-agent-gui/src-tauri/src/multimodal/heic.rs:38` | not reachable (GUI-only crate) | `unsupported` | No release-binary change; FreeBSD GUI is outside this support scope. | Task 9 |
| `mur-agent-gui/src-tauri/src/multimodal/heic.rs:63` | not reachable (GUI-only crate) | `unsupported` | No release-binary change; FreeBSD GUI is outside this support scope. | Task 9 |
| `mur-agent-gui/src-tauri/src/multimodal/heic.rs:133` | not reachable (GUI-only crate) | `unsupported` | No release-binary change; FreeBSD GUI is outside this support scope. | Task 9 |
| `mur-agent-gui/src-tauri/src/multimodal/ocr/mod.rs:18` | not reachable (GUI-only crate) | `unsupported` | No release-binary change; FreeBSD GUI is outside this support scope. | Task 9 |
| `mur-agent-gui/src-tauri/src/multimodal/ocr/mod.rs:21` | not reachable (GUI-only crate) | `unsupported` | No release-binary change; FreeBSD GUI is outside this support scope. | Task 9 |
| `mur-agent-gui/src-tauri/src/multimodal/ocr/platform.rs:7` | not reachable (GUI-only crate) | `unsupported` | No release-binary change; FreeBSD GUI is outside this support scope. | Task 9 |
| `mur-agent-gui/src-tauri/src/multimodal/ocr/platform.rs:15` | not reachable (GUI-only crate) | `unsupported` | No release-binary change; FreeBSD GUI is outside this support scope. | Task 9 |
| `mur-agent-gui/src-tauri/src/multimodal/ocr/tesseract.rs:24` | not reachable (GUI-only crate) | `unsupported` | No release-binary change; FreeBSD GUI is outside this support scope. | Task 9 |
| `mur-agent-gui/src-tauri/src/multimodal/ocr/vision.rs:18` | not reachable (GUI-only crate) | `unsupported` | No release-binary change; FreeBSD GUI is outside this support scope. | Task 9 |
| `mur-agent-gui/src-tauri/src/self_test.rs:57` | not reachable (GUI-only crate) | `unsupported` | No release-binary change; FreeBSD GUI is outside this support scope. | Task 9 |
| `mur-agent-gui/src-tauri/src/self_test.rs:59` | not reachable (GUI-only crate) | `unsupported` | No release-binary change; FreeBSD GUI is outside this support scope. | Task 9 |
| `mur-agent-gui/src-tauri/src/self_test.rs:92` | not reachable (GUI-only crate) | `unsupported` | No release-binary change; FreeBSD GUI is outside this support scope. | Task 9 |
| `mur-agent-gui/src-tauri/src/self_test.rs:104` | not reachable (GUI-only crate) | `unsupported` | No release-binary change; FreeBSD GUI is outside this support scope. | Task 9 |
| `mur-agent-gui/src-tauri/src/self_test.rs:117` | not reachable (GUI-only crate) | `unsupported` | No release-binary change; FreeBSD GUI is outside this support scope. | Task 9 |
| `mur-agent-gui/src-tauri/src/self_test.rs:142` | not reachable (GUI-only crate) | `unsupported` | No release-binary change; FreeBSD GUI is outside this support scope. | Task 9 |
| `mur-agent-gui/src-tauri/src/self_test.rs:148` | not reachable (GUI-only crate) | `unsupported` | No release-binary change; FreeBSD GUI is outside this support scope. | Task 9 |
| `mur-agent-gui/src-tauri/src/send/dock.rs:18` | not reachable (GUI-only crate) | `unsupported` | No release-binary change; FreeBSD GUI is outside this support scope. | Task 9 |
| `mur-agent-gui/src-tauri/src/send/mod.rs:22` | not reachable (GUI-only crate) | `unsupported` | No release-binary change; FreeBSD GUI is outside this support scope. | Task 9 |
| `mur-agent-gui/src-tauri/src/send/mod.rs:24` | not reachable (GUI-only crate) | `unsupported` | No release-binary change; FreeBSD GUI is outside this support scope. | Task 9 |
| `mur-agent-gui/src-tauri/src/send/mod.rs:26` | not reachable (GUI-only crate) | `unsupported` | No release-binary change; FreeBSD GUI is outside this support scope. | Task 9 |
| `mur-agent-gui/src-tauri/src/send/services.rs:21` | not reachable (GUI-only crate) | `unsupported` | No release-binary change; FreeBSD GUI is outside this support scope. | Task 9 |
| `mur-agent-gui/src-tauri/src/send/services_provider.rs:28` | not reachable (GUI-only crate) | `unsupported` | No release-binary change; FreeBSD GUI is outside this support scope. | Task 9 |
| `mur-agent-gui/src-tauri/src/sidecar.rs:209` | not reachable (GUI-only crate) | `unsupported` | No release-binary change; FreeBSD GUI is outside this support scope. | Task 9 |
| `mur-agent-gui/src-tauri/src/sidecar.rs:211` | not reachable (GUI-only crate) | `unsupported` | No release-binary change; FreeBSD GUI is outside this support scope. | Task 9 |
| `mur-agent-gui/src-tauri/src/sidecar.rs:224` | not reachable (GUI-only crate) | `unsupported` | No release-binary change; FreeBSD GUI is outside this support scope. | Task 9 |
| `mur-agent-gui/src-tauri/src/sidecar.rs:244` | not reachable (GUI-only crate) | `unsupported` | No release-binary change; FreeBSD GUI is outside this support scope. | Task 9 |
| `mur-agent-gui/src-tauri/src/test_harness.rs:128` | not reachable (GUI-only crate) | `unsupported` | No release-binary change; FreeBSD GUI is outside this support scope. | Task 9 |
| `mur-agent-gui/src-tauri/src/test_harness.rs:146` | not reachable (GUI-only crate) | `unsupported` | No release-binary change; FreeBSD GUI is outside this support scope. | Task 9 |
| `mur-agent-gui/src-tauri/src/voice/hotkey.rs:39` | not reachable (GUI-only crate) | `unsupported` | No release-binary change; FreeBSD GUI is outside this support scope. | Task 9 |
| `mur-agent-gui/src-tauri/src/voice/stt/whisper.rs:3` | not reachable (GUI-only crate) | `unsupported` | No release-binary change; FreeBSD GUI is outside this support scope. | Task 9 |
| `mur-agent-gui/src-tauri/tests/multimodal_heic.rs:3` | not reachable (GUI-only crate) | `unsupported` | No release-binary change; FreeBSD GUI is outside this support scope. | Task 9 |
| `mur-agent-gui/src-tauri/tests/multimodal_heic.rs:32` | not reachable (GUI-only crate) | `unsupported` | No release-binary change; FreeBSD GUI is outside this support scope. | Task 9 |
| `mur-agent-gui/src-tauri/tests/multimodal_heic.rs:41` | not reachable (GUI-only crate) | `unsupported` | No release-binary change; FreeBSD GUI is outside this support scope. | Task 9 |
| `mur-agent-gui/src-tauri/tests/multimodal_pipeline_image.rs:100` | not reachable (GUI-only crate) | `unsupported` | No release-binary change; FreeBSD GUI is outside this support scope. | Task 9 |
| `mur-agent-gui/src-tauri/tests/multimodal_pipeline_image.rs:105` | not reachable (GUI-only crate) | `unsupported` | No release-binary change; FreeBSD GUI is outside this support scope. | Task 9 |
| `mur-agent-gui/src-tauri/tests/multimodal_pipeline_image.rs:156` | not reachable (GUI-only crate) | `unsupported` | No release-binary change; FreeBSD GUI is outside this support scope. | Task 9 |
| `mur-agent-gui/src-tauri/tests/send_dock.rs:13` | not reachable (GUI-only crate) | `unsupported` | No release-binary change; FreeBSD GUI is outside this support scope. | Task 9 |
| `mur-agent-gui/src-tauri/tests/send_dock.rs:15` | not reachable (GUI-only crate) | `unsupported` | No release-binary change; FreeBSD GUI is outside this support scope. | Task 9 |
| `mur-agent-gui/src-tauri/tests/send_dock.rs:17` | not reachable (GUI-only crate) | `unsupported` | No release-binary change; FreeBSD GUI is outside this support scope. | Task 9 |
| `mur-agent-gui/src-tauri/tests/send_dock.rs:20` | not reachable (GUI-only crate) | `unsupported` | No release-binary change; FreeBSD GUI is outside this support scope. | Task 9 |
| `mur-agent-gui/src-tauri/tests/send_dock.rs:43` | not reachable (GUI-only crate) | `unsupported` | No release-binary change; FreeBSD GUI is outside this support scope. | Task 9 |
| `mur-agent-gui/src-tauri/tests/send_dock.rs:70` | not reachable (GUI-only crate) | `unsupported` | No release-binary change; FreeBSD GUI is outside this support scope. | Task 9 |
| `mur-agent-gui/src-tauri/tests/send_services.rs:3` | not reachable (GUI-only crate) | `unsupported` | No release-binary change; FreeBSD GUI is outside this support scope. | Task 9 |
| `mur-agent-gui/src-tauri/tests/send_services.rs:17` | not reachable (GUI-only crate) | `unsupported` | No release-binary change; FreeBSD GUI is outside this support scope. | Task 9 |
| `mur-agent-runtime/src/exec_dirs.rs:16` | `mur-agent-runtime` runtime path | `portable` | No source change identified; native workspace tests verify the generic Unix path. | Task 6 |
| `mur-agent-runtime/src/hooks/b0.rs:168` | `mur-agent-runtime` runtime path | `portable` | No source change identified; native workspace tests verify the generic Unix path. | Task 6 |
| `mur-agent-runtime/src/hooks/b0_helpers.rs:377` | `mur-agent-runtime` runtime path | `portable` | No source change identified; native workspace tests verify the generic Unix path. | Task 6 |
| `mur-agent-runtime/src/hooks/b0_helpers.rs:445` | `mur-agent-runtime` runtime path | `portable` | No source change identified; native workspace tests verify the generic Unix path. | Task 6 |
| `mur-agent-runtime/src/hooks/b0_helpers.rs:553` | `mur-agent-runtime` runtime path | `portable` | No source change identified; native workspace tests verify the generic Unix path. | Task 6 |
| `mur-agent-runtime/src/hooks/b0_helpers.rs:585` | `mur-agent-runtime` runtime path | `portable` | No source change identified; native workspace tests verify the generic Unix path. | Task 6 |
| `mur-agent-runtime/src/hooks/b0_helpers.rs:600` | `mur-agent-runtime` runtime path | `portable` | No source change identified; native workspace tests verify the generic Unix path. | Task 6 |
| `mur-agent-runtime/src/hooks/b0_helpers.rs:604` | `mur-agent-runtime` runtime path | `portable` | No source change identified; native workspace tests verify the generic Unix path. | Task 6 |
| `mur-agent-runtime/src/mcp_repin.rs:16` | `mur-agent-runtime` runtime path | `portable` | No source change identified; native workspace tests verify the generic Unix path. | Task 6 |
| `mur-agent-runtime/src/oauth/mod.rs:237` | `mur-agent-runtime` runtime path | `portable` | No source change identified; native workspace tests verify the generic Unix path. | Task 6 |
| `mur-agent-runtime/src/protocol/mcp_client.rs:239` | `mur-agent-runtime` runtime path | `portable` | No source change identified; native workspace tests verify the generic Unix path. | Task 6 |
| `mur-agent-runtime/src/sandbox/child.rs:120` | `mur-agent-runtime` sandbox path | `explicit-freebsd` | Use the existing non-Linux/macOS/Windows fallback; native CI must compile and test it. | Task 6 |
| `mur-agent-runtime/src/sandbox/child.rs:151` | `mur-agent-runtime` sandbox path | `explicit-freebsd` | Use the existing non-Linux/macOS/Windows fallback; native CI must compile and test it. | Task 6 |
| `mur-agent-runtime/src/sandbox/launch_chain.rs:393` | `mur-agent-runtime` sandbox path | `explicit-freebsd` | Use the existing non-Linux/macOS/Windows fallback; native CI must compile and test it. | Task 6 |
| `mur-agent-runtime/src/sandbox/launch_chain.rs:401` | `mur-agent-runtime` sandbox path | `explicit-freebsd` | Use the existing non-Linux/macOS/Windows fallback; native CI must compile and test it. | Task 6 |
| `mur-agent-runtime/src/sandbox/launch_chain.rs:404` | `mur-agent-runtime` sandbox path | `explicit-freebsd` | Use the existing non-Linux/macOS/Windows fallback; native CI must compile and test it. | Task 6 |
| `mur-agent-runtime/src/sandbox/launch_chain.rs:409` | `mur-agent-runtime` sandbox path | `explicit-freebsd` | Use the existing non-Linux/macOS/Windows fallback; native CI must compile and test it. | Task 6 |
| `mur-agent-runtime/src/sandbox/linux.rs:7` | `mur-agent-runtime` sandbox path | `explicit-freebsd` | Use the existing non-Linux/macOS/Windows fallback; native CI must compile and test it. | Task 6 |
| `mur-agent-runtime/src/sandbox/linux.rs:9` | `mur-agent-runtime` sandbox path | `explicit-freebsd` | Use the existing non-Linux/macOS/Windows fallback; native CI must compile and test it. | Task 6 |
| `mur-agent-runtime/src/sandbox/linux.rs:11` | `mur-agent-runtime` sandbox path | `explicit-freebsd` | Use the existing non-Linux/macOS/Windows fallback; native CI must compile and test it. | Task 6 |
| `mur-agent-runtime/src/sandbox/linux.rs:17` | `mur-agent-runtime` sandbox path | `explicit-freebsd` | Use the existing non-Linux/macOS/Windows fallback; native CI must compile and test it. | Task 6 |
| `mur-agent-runtime/src/sandbox/linux.rs:103` | `mur-agent-runtime` sandbox path | `explicit-freebsd` | Use the existing non-Linux/macOS/Windows fallback; native CI must compile and test it. | Task 6 |
| `mur-agent-runtime/src/sandbox/linux.rs:108` | `mur-agent-runtime` sandbox path | `explicit-freebsd` | Use the existing non-Linux/macOS/Windows fallback; native CI must compile and test it. | Task 6 |
| `mur-agent-runtime/src/sandbox/linux.rs:110` | `mur-agent-runtime` sandbox path | `explicit-freebsd` | Use the existing non-Linux/macOS/Windows fallback; native CI must compile and test it. | Task 6 |
| `mur-agent-runtime/src/sandbox/mod.rs:10` | `mur-agent-runtime` sandbox path | `explicit-freebsd` | Use the existing non-Linux/macOS/Windows fallback; native CI must compile and test it. | Task 6 |
| `mur-agent-runtime/src/sandbox/mod.rs:12` | `mur-agent-runtime` sandbox path | `explicit-freebsd` | Use the existing non-Linux/macOS/Windows fallback; native CI must compile and test it. | Task 6 |
| `mur-agent-runtime/src/sandbox/mod.rs:75` | `mur-agent-runtime` sandbox path | `explicit-freebsd` | Use the existing non-Linux/macOS/Windows fallback; native CI must compile and test it. | Task 6 |
| `mur-agent-runtime/src/sandbox/mod.rs:80` | `mur-agent-runtime` sandbox path | `explicit-freebsd` | Use the existing non-Linux/macOS/Windows fallback; native CI must compile and test it. | Task 6 |
| `mur-agent-runtime/src/sandbox/mod.rs:85` | `mur-agent-runtime` sandbox path | `explicit-freebsd` | Use the existing non-Linux/macOS/Windows fallback; native CI must compile and test it. | Task 6 |
| `mur-agent-runtime/src/sandbox/mod.rs:90` | `mur-agent-runtime` sandbox path | `explicit-freebsd` | Use the existing non-Linux/macOS/Windows fallback; native CI must compile and test it. | Task 6 |
| `mur-agent-runtime/src/sandbox/policy.rs:2` | `mur-agent-runtime` sandbox path | `explicit-freebsd` | Use the existing non-Linux/macOS/Windows fallback; native CI must compile and test it. | Task 6 |
| `mur-agent-runtime/src/sandbox/policy.rs:773` | `mur-agent-runtime` sandbox path | `explicit-freebsd` | Use the existing non-Linux/macOS/Windows fallback; native CI must compile and test it. | Task 6 |
| `mur-agent-runtime/src/sandbox/policy.rs:920` | `mur-agent-runtime` sandbox path | `explicit-freebsd` | Use the existing non-Linux/macOS/Windows fallback; native CI must compile and test it. | Task 6 |
| `mur-agent-runtime/src/sandbox/policy.rs:930` | `mur-agent-runtime` sandbox path | `explicit-freebsd` | Use the existing non-Linux/macOS/Windows fallback; native CI must compile and test it. | Task 6 |
| `mur-agent-runtime/src/sandbox/policy.rs:984` | `mur-agent-runtime` sandbox path | `explicit-freebsd` | Use the existing non-Linux/macOS/Windows fallback; native CI must compile and test it. | Task 6 |
| `mur-agent-runtime/src/sandbox/policy.rs:993` | `mur-agent-runtime` sandbox path | `explicit-freebsd` | Use the existing non-Linux/macOS/Windows fallback; native CI must compile and test it. | Task 6 |
| `mur-agent-runtime/src/sandbox/policy.rs:1004` | `mur-agent-runtime` sandbox path | `explicit-freebsd` | Use the existing non-Linux/macOS/Windows fallback; native CI must compile and test it. | Task 6 |
| `mur-agent-runtime/src/sandbox/policy.rs:1017` | `mur-agent-runtime` sandbox path | `explicit-freebsd` | Use the existing non-Linux/macOS/Windows fallback; native CI must compile and test it. | Task 6 |
| `mur-agent-runtime/src/sandbox/policy.rs:1615` | `mur-agent-runtime` sandbox path | `explicit-freebsd` | Use the existing non-Linux/macOS/Windows fallback; native CI must compile and test it. | Task 6 |
| `mur-agent-runtime/src/sandbox/policy.rs:1626` | `mur-agent-runtime` sandbox path | `explicit-freebsd` | Use the existing non-Linux/macOS/Windows fallback; native CI must compile and test it. | Task 6 |
| `mur-agent-runtime/src/sandbox/policy.rs:1759` | `mur-agent-runtime` sandbox path | `explicit-freebsd` | Use the existing non-Linux/macOS/Windows fallback; native CI must compile and test it. | Task 6 |
| `mur-agent-runtime/src/sandbox/policy.rs:1776` | `mur-agent-runtime` sandbox path | `explicit-freebsd` | Use the existing non-Linux/macOS/Windows fallback; native CI must compile and test it. | Task 6 |
| `mur-agent-runtime/src/sandbox/policy.rs:2040` | `mur-agent-runtime` sandbox path | `explicit-freebsd` | Use the existing non-Linux/macOS/Windows fallback; native CI must compile and test it. | Task 6 |
| `mur-agent-runtime/src/sandbox/policy.rs:2101` | `mur-agent-runtime` sandbox path | `explicit-freebsd` | Use the existing non-Linux/macOS/Windows fallback; native CI must compile and test it. | Task 6 |
| `mur-agent-runtime/src/tools/bash.rs:29` | `mur-agent-runtime` runtime path | `portable` | No source change identified; native workspace tests verify the generic Unix path. | Task 6 |
| `mur-agent-runtime/src/transport/unix_socket.rs:137` | `mur-agent-runtime` runtime path | `portable` | No source change identified; native workspace tests verify the generic Unix path. | Task 6 |
| `mur-agent-runtime/src/transport/unix_socket.rs:163` | `mur-agent-runtime` runtime path | `portable` | No source change identified; native workspace tests verify the generic Unix path. | Task 6 |
| `mur-agent-runtime/src/transport/unix_socket.rs:190` | `mur-agent-runtime` runtime path | `portable` | No source change identified; native workspace tests verify the generic Unix path. | Task 6 |
| `mur-agent-runtime/tests/b0_rule11_mcp_signature.rs:19` | test-only | `portable` | Keep compiling/running under the native FreeBSD workspace test lane. | Task 6 |
| `mur-agent-runtime/tests/b0_rule11_mcp_signature.rs:49` | test-only | `portable` | Keep compiling/running under the native FreeBSD workspace test lane. | Task 6 |
| `mur-agent-runtime/tests/b0_rule11_mcp_signature.rs:73` | test-only | `portable` | Keep compiling/running under the native FreeBSD workspace test lane. | Task 6 |
| `mur-agent-runtime/tests/b0_rule11_signability.rs:83` | test-only | `portable` | Keep compiling/running under the native FreeBSD workspace test lane. | Task 6 |
| `mur-agent-runtime/tests/b0_rule11_signability.rs:96` | test-only | `portable` | Keep compiling/running under the native FreeBSD workspace test lane. | Task 6 |
| `mur-agent-runtime/tests/b0_rule11_signability.rs:102` | test-only | `portable` | Keep compiling/running under the native FreeBSD workspace test lane. | Task 6 |
| `mur-agent-runtime/tests/b0_rule11_signability.rs:107` | test-only | `portable` | Keep compiling/running under the native FreeBSD workspace test lane. | Task 6 |
| `mur-agent-runtime/tests/b1_spawn_allowlist_enforce.rs:32` | test-only | `portable` | Keep compiling/running under the native FreeBSD workspace test lane. | Task 6 |
| `mur-agent-runtime/tests/sandbox_build_lane.rs:19` | test-only | `portable` | Keep compiling/running under the native FreeBSD workspace test lane. | Task 6 |
| `mur-agent-runtime/tests/sandbox_build_lane.rs:50` | test-only | `portable` | Keep compiling/running under the native FreeBSD workspace test lane. | Task 6 |
| `mur-agent-runtime/tests/sandbox_e2e.rs:4` | test-only | `portable` | Keep compiling/running under the native FreeBSD workspace test lane. | Task 6 |
| `mur-agent-runtime/tests/sandbox_e2e.rs:17` | test-only | `portable` | Keep compiling/running under the native FreeBSD workspace test lane. | Task 6 |
| `mur-agent-runtime/tests/sandbox_e2e.rs:149` | test-only | `portable` | Keep compiling/running under the native FreeBSD workspace test lane. | Task 6 |
| `mur-agent-runtime/tests/sandbox_e2e.rs:274` | test-only | `portable` | Keep compiling/running under the native FreeBSD workspace test lane. | Task 6 |
| `mur-common/src/agent.rs:1565` | shared by release binaries | `portable` | No source change identified; platform APIs or generic Unix behavior already cover FreeBSD. | Task 6 |
| `mur-common/src/binary_attestation.rs:34` | shared by release binaries | `portable` | No source change identified; platform APIs or generic Unix behavior already cover FreeBSD. | Task 6 |
| `mur-common/src/deps/detect.rs:48` | shared by release binaries | `portable` | No source change identified; platform APIs or generic Unix behavior already cover FreeBSD. | Task 6 |
| `mur-common/src/deps/mod.rs:65` | shared by release binaries | `portable` | No source change identified; platform APIs or generic Unix behavior already cover FreeBSD. | Task 6 |
| `mur-common/src/exec.rs:183` | shared by release binaries | `portable` | No source change identified; platform APIs or generic Unix behavior already cover FreeBSD. | Task 6 |
| `mur-common/src/exec.rs:202` | shared by release binaries | `portable` | No source change identified; platform APIs or generic Unix behavior already cover FreeBSD. | Task 6 |
| `mur-common/src/local_llm.rs:4` | shared by release binaries | `portable` | No source change identified; platform APIs or generic Unix behavior already cover FreeBSD. | Task 6 |
| `mur-common/src/schedule.rs:23` | shared by release binaries | `portable` | No source change identified; platform APIs or generic Unix behavior already cover FreeBSD. | Task 6 |
| `mur-core/src/agent_wizard/apply.rs:57` | `mur` CLI path | `portable` | No source change identified; native workspace tests verify the existing fallback. | Task 6 |
| `mur-core/src/auth.rs:260` | `mur` CLI path | `portable` | No source change identified; native workspace tests verify the existing fallback. | Task 6 |
| `mur-core/src/auth.rs:318` | `mur` CLI path | `portable` | No source change identified; native workspace tests verify the existing fallback. | Task 6 |
| `mur-core/src/auth.rs:322` | `mur` CLI path | `portable` | No source change identified; native workspace tests verify the existing fallback. | Task 6 |
| `mur-core/src/auth.rs:326` | `mur` CLI path | `portable` | No source change identified; native workspace tests verify the existing fallback. | Task 6 |
| `mur-core/src/cli/agent.rs:39` | `mur` CLI path | `portable` | No source change identified; native workspace tests verify the existing fallback. | Task 6 |
| `mur-core/src/cli/agent.rs:45` | `mur` CLI path | `portable` | No source change identified; native workspace tests verify the existing fallback. | Task 6 |
| `mur-core/src/cli/agent.rs:55` | `mur` CLI path | `portable` | No source change identified; native workspace tests verify the existing fallback. | Task 6 |
| `mur-core/src/cli/agent.rs:70` | `mur` CLI path | `portable` | No source change identified; native workspace tests verify the existing fallback. | Task 6 |
| `mur-core/src/cli/agent.rs:243` | `mur` CLI path | `portable` | No source change identified; native workspace tests verify the existing fallback. | Task 6 |
| `mur-core/src/cmd/agent/cli/app/mod.rs:31` | `mur` CLI path | `portable` | No source change identified; native workspace tests verify the existing fallback. | Task 6 |
| `mur-core/src/cmd/agent/cli/app/mod.rs:33` | `mur` CLI path | `portable` | No source change identified; native workspace tests verify the existing fallback. | Task 6 |
| `mur-core/src/cmd/agent/cli/input.rs:27` | `mur` CLI path | `portable` | No source change identified; native workspace tests verify the existing fallback. | Task 6 |
| `mur-core/src/cmd/agent/cli/input.rs:29` | `mur` CLI path | `portable` | No source change identified; native workspace tests verify the existing fallback. | Task 6 |
| `mur-core/src/cmd/agent/cli/input.rs:46` | `mur` CLI path | `portable` | No source change identified; native workspace tests verify the existing fallback. | Task 6 |
| `mur-core/src/cmd/agent/cli/input.rs:59` | `mur` CLI path | `portable` | No source change identified; native workspace tests verify the existing fallback. | Task 6 |
| `mur-core/src/cmd/agent/cli/login.rs:13` | `mur` CLI path | `portable` | No source change identified; native workspace tests verify the existing fallback. | Task 6 |
| `mur-core/src/cmd/agent/cli/login.rs:34` | `mur` CLI path | `portable` | No source change identified; native workspace tests verify the existing fallback. | Task 6 |
| `mur-core/src/cmd/agent/cli/login.rs:528` | `mur` CLI path | `portable` | No source change identified; native workspace tests verify the existing fallback. | Task 6 |
| `mur-core/src/cmd/agent/cli/login/stamp.rs:15` | `mur` CLI path | `portable` | No source change identified; native workspace tests verify the existing fallback. | Task 6 |
| `mur-core/src/cmd/agent/cli/login/stamp.rs:49` | `mur` CLI path | `portable` | No source change identified; native workspace tests verify the existing fallback. | Task 6 |
| `mur-core/src/cmd/agent/cli/login/stamp.rs:63` | `mur` CLI path | `portable` | No source change identified; native workspace tests verify the existing fallback. | Task 6 |
| `mur-core/src/cmd/agent/cli/login/tests.rs:176` | test-only | `portable` | Keep compiling/running under the native FreeBSD workspace test lane. | Task 6 |
| `mur-core/src/cmd/agent/cli/notify.rs:4` | `mur` CLI path | `portable` | No source change identified; native workspace tests verify the existing fallback. | Task 6 |
| `mur-core/src/cmd/agent/cli/notify.rs:6` | `mur` CLI path | `portable` | No source change identified; native workspace tests verify the existing fallback. | Task 6 |
| `mur-core/src/cmd/agent/cli/notify.rs:19` | `mur` CLI path | `portable` | No source change identified; native workspace tests verify the existing fallback. | Task 6 |
| `mur-core/src/cmd/agent/cli/notify.rs:21` | `mur` CLI path | `portable` | No source change identified; native workspace tests verify the existing fallback. | Task 6 |
| `mur-core/src/cmd/agent/cli/notify.rs:27` | `mur` CLI path | `portable` | No source change identified; native workspace tests verify the existing fallback. | Task 6 |
| `mur-core/src/cmd/agent/cli/notify.rs:29` | `mur` CLI path | `portable` | No source change identified; native workspace tests verify the existing fallback. | Task 6 |
| `mur-core/src/cmd/agent/cli/notify.rs:35` | `mur` CLI path | `portable` | No source change identified; native workspace tests verify the existing fallback. | Task 6 |
| `mur-core/src/cmd/agent/cli/panel.rs:238` | `mur` CLI path | `portable` | No source change identified; native workspace tests verify the existing fallback. | Task 6 |
| `mur-core/src/cmd/agent/cli/panel.rs:251` | `mur` CLI path | `portable` | No source change identified; native workspace tests verify the existing fallback. | Task 6 |
| `mur-core/src/cmd/agent/hub.rs:118` | `mur` CLI path | `portable` | No source change identified; native workspace tests verify the existing fallback. | Task 6 |
| `mur-core/src/cmd/agent/hub.rs:142` | `mur` CLI path | `portable` | No source change identified; native workspace tests verify the existing fallback. | Task 6 |
| `mur-core/src/cmd/agent/lifecycle.rs:228` | `mur` CLI path | `portable` | No source change identified; native workspace tests verify the existing fallback. | Task 6 |
| `mur-core/src/cmd/agent/lifecycle.rs:727` | `mur` CLI path | `portable` | No source change identified; native workspace tests verify the existing fallback. | Task 6 |
| `mur-core/src/cmd/agent/lifecycle.rs:882` | `mur` CLI path | `portable` | No source change identified; native workspace tests verify the existing fallback. | Task 6 |
| `mur-core/src/cmd/agent/lifecycle.rs:912` | `mur` CLI path | `portable` | No source change identified; native workspace tests verify the existing fallback. | Task 6 |
| `mur-core/src/cmd/agent/lifecycle.rs:937` | `mur` CLI path | `portable` | No source change identified; native workspace tests verify the existing fallback. | Task 6 |
| `mur-core/src/cmd/agent/mod.rs:172` | `mur` CLI path | `portable` | No source change identified; native workspace tests verify the existing fallback. | Task 6 |
| `mur-core/src/cmd/agent/model_resolve.rs:30` | `mur` CLI path | `portable` | No source change identified; native workspace tests verify the existing fallback. | Task 6 |
| `mur-core/src/cmd/agent/restart.rs:5` | `mur` agent/service command | `unsupported` | Retain the explicit non-macOS/Linux fallback and document the unsupported service integration. | Task 9 |
| `mur-core/src/cmd/agent/restart.rs:6` | `mur` agent/service command | `unsupported` | Retain the explicit non-macOS/Linux fallback and document the unsupported service integration. | Task 9 |
| `mur-core/src/cmd/agent/restart.rs:330` | `mur` agent/service command | `unsupported` | Retain the explicit non-macOS/Linux fallback and document the unsupported service integration. | Task 9 |
| `mur-core/src/cmd/agent/restart.rs:399` | `mur` agent/service command | `unsupported` | Retain the explicit non-macOS/Linux fallback and document the unsupported service integration. | Task 9 |
| `mur-core/src/cmd/agent/restart.rs:525` | `mur` agent/service command | `unsupported` | Retain the explicit non-macOS/Linux fallback and document the unsupported service integration. | Task 9 |
| `mur-core/src/cmd/agent/restart.rs:545` | `mur` agent/service command | `unsupported` | Retain the explicit non-macOS/Linux fallback and document the unsupported service integration. | Task 9 |
| `mur-core/src/cmd/agent/restart.rs:570` | `mur` agent/service command | `unsupported` | Retain the explicit non-macOS/Linux fallback and document the unsupported service integration. | Task 9 |
| `mur-core/src/cmd/agent/restart.rs:585` | `mur` agent/service command | `unsupported` | Retain the explicit non-macOS/Linux fallback and document the unsupported service integration. | Task 9 |
| `mur-core/src/cmd/agent/restart.rs:602` | `mur` agent/service command | `unsupported` | Retain the explicit non-macOS/Linux fallback and document the unsupported service integration. | Task 9 |
| `mur-core/src/cmd/agent/restart.rs:606` | `mur` agent/service command | `unsupported` | Retain the explicit non-macOS/Linux fallback and document the unsupported service integration. | Task 9 |
| `mur-core/src/cmd/agent/restart.rs:611` | `mur` agent/service command | `unsupported` | Retain the explicit non-macOS/Linux fallback and document the unsupported service integration. | Task 9 |
| `mur-core/src/cmd/agent/restart.rs:613` | `mur` agent/service command | `unsupported` | Retain the explicit non-macOS/Linux fallback and document the unsupported service integration. | Task 9 |
| `mur-core/src/cmd/agent/restart.rs:616` | `mur` agent/service command | `unsupported` | Retain the explicit non-macOS/Linux fallback and document the unsupported service integration. | Task 9 |
| `mur-core/src/cmd/agent/restart.rs:908` | `mur` agent/service command | `unsupported` | Retain the explicit non-macOS/Linux fallback and document the unsupported service integration. | Task 9 |
| `mur-core/src/cmd/agent/restart_confirm.rs:23` | `mur` CLI path | `portable` | No source change identified; native workspace tests verify the existing fallback. | Task 6 |
| `mur-core/src/cmd/agent/restart_confirm.rs:25` | `mur` CLI path | `portable` | No source change identified; native workspace tests verify the existing fallback. | Task 6 |
| `mur-core/src/cmd/agent/restart_confirm.rs:52` | `mur` CLI path | `portable` | No source change identified; native workspace tests verify the existing fallback. | Task 6 |
| `mur-core/src/cmd/agent/restart_confirm.rs:149` | `mur` CLI path | `portable` | No source change identified; native workspace tests verify the existing fallback. | Task 6 |
| `mur-core/src/cmd/agent/restart_confirm.rs:166` | `mur` CLI path | `portable` | No source change identified; native workspace tests verify the existing fallback. | Task 6 |
| `mur-core/src/cmd/agent/restart_confirm.rs:187` | `mur` CLI path | `portable` | No source change identified; native workspace tests verify the existing fallback. | Task 6 |
| `mur-core/src/cmd/agent/service.rs:1` | `mur` agent/service command | `unsupported` | Retain the explicit non-macOS/Linux fallback and document the unsupported service integration. | Task 9 |
| `mur-core/src/cmd/agent/service.rs:18` | `mur` agent/service command | `unsupported` | Retain the explicit non-macOS/Linux fallback and document the unsupported service integration. | Task 9 |
| `mur-core/src/cmd/agent/service.rs:20` | `mur` agent/service command | `unsupported` | Retain the explicit non-macOS/Linux fallback and document the unsupported service integration. | Task 9 |
| `mur-core/src/cmd/agent/service.rs:22` | `mur` agent/service command | `unsupported` | Retain the explicit non-macOS/Linux fallback and document the unsupported service integration. | Task 9 |
| `mur-core/src/cmd/agent/service.rs:26` | `mur` agent/service command | `unsupported` | Retain the explicit non-macOS/Linux fallback and document the unsupported service integration. | Task 9 |
| `mur-core/src/cmd/agent/service.rs:28` | `mur` agent/service command | `unsupported` | Retain the explicit non-macOS/Linux fallback and document the unsupported service integration. | Task 9 |
| `mur-core/src/cmd/agent/service.rs:35` | `mur` agent/service command | `unsupported` | Retain the explicit non-macOS/Linux fallback and document the unsupported service integration. | Task 9 |
| `mur-core/src/cmd/agent/service.rs:37` | `mur` agent/service command | `unsupported` | Retain the explicit non-macOS/Linux fallback and document the unsupported service integration. | Task 9 |
| `mur-core/src/cmd/agent/service.rs:39` | `mur` agent/service command | `unsupported` | Retain the explicit non-macOS/Linux fallback and document the unsupported service integration. | Task 9 |
| `mur-core/src/cmd/agent/service.rs:44` | `mur` agent/service command | `unsupported` | Retain the explicit non-macOS/Linux fallback and document the unsupported service integration. | Task 9 |
| `mur-core/src/cmd/agent/service.rs:70` | `mur` agent/service command | `unsupported` | Retain the explicit non-macOS/Linux fallback and document the unsupported service integration. | Task 9 |
| `mur-core/src/cmd/agent/service.rs:77` | `mur` agent/service command | `unsupported` | Retain the explicit non-macOS/Linux fallback and document the unsupported service integration. | Task 9 |
| `mur-core/src/cmd/agent/service.rs:93` | `mur` agent/service command | `unsupported` | Retain the explicit non-macOS/Linux fallback and document the unsupported service integration. | Task 9 |
| `mur-core/src/cmd/agent/service.rs:100` | `mur` agent/service command | `unsupported` | Retain the explicit non-macOS/Linux fallback and document the unsupported service integration. | Task 9 |
| `mur-core/src/cmd/agent/service.rs:107` | `mur` agent/service command | `unsupported` | Retain the explicit non-macOS/Linux fallback and document the unsupported service integration. | Task 9 |
| `mur-core/src/cmd/agent/service.rs:118` | `mur` agent/service command | `unsupported` | Retain the explicit non-macOS/Linux fallback and document the unsupported service integration. | Task 9 |
| `mur-core/src/cmd/agent/service.rs:121` | `mur` agent/service command | `unsupported` | Retain the explicit non-macOS/Linux fallback and document the unsupported service integration. | Task 9 |
| `mur-core/src/cmd/agent/service.rs:123` | `mur` agent/service command | `unsupported` | Retain the explicit non-macOS/Linux fallback and document the unsupported service integration. | Task 9 |
| `mur-core/src/cmd/agent/service.rs:128` | `mur` agent/service command | `unsupported` | Retain the explicit non-macOS/Linux fallback and document the unsupported service integration. | Task 9 |
| `mur-core/src/cmd/agent/service.rs:152` | `mur` agent/service command | `unsupported` | Retain the explicit non-macOS/Linux fallback and document the unsupported service integration. | Task 9 |
| `mur-core/src/cmd/agent/service.rs:168` | `mur` agent/service command | `unsupported` | Retain the explicit non-macOS/Linux fallback and document the unsupported service integration. | Task 9 |
| `mur-core/src/cmd/agent/service.rs:186` | `mur` agent/service command | `unsupported` | Retain the explicit non-macOS/Linux fallback and document the unsupported service integration. | Task 9 |
| `mur-core/src/cmd/agent/service.rs:188` | `mur` agent/service command | `unsupported` | Retain the explicit non-macOS/Linux fallback and document the unsupported service integration. | Task 9 |
| `mur-core/src/cmd/agent/service.rs:202` | `mur` agent/service command | `unsupported` | Retain the explicit non-macOS/Linux fallback and document the unsupported service integration. | Task 9 |
| `mur-core/src/cmd/agent/service.rs:221` | `mur` agent/service command | `unsupported` | Retain the explicit non-macOS/Linux fallback and document the unsupported service integration. | Task 9 |
| `mur-core/src/cmd/agent/service.rs:223` | `mur` agent/service command | `unsupported` | Retain the explicit non-macOS/Linux fallback and document the unsupported service integration. | Task 9 |
| `mur-core/src/cmd/agent/service.rs:226` | `mur` agent/service command | `unsupported` | Retain the explicit non-macOS/Linux fallback and document the unsupported service integration. | Task 9 |
| `mur-core/src/cmd/agent/service.rs:231` | `mur` agent/service command | `unsupported` | Retain the explicit non-macOS/Linux fallback and document the unsupported service integration. | Task 9 |
| `mur-core/src/cmd/agent/service.rs:236` | `mur` agent/service command | `unsupported` | Retain the explicit non-macOS/Linux fallback and document the unsupported service integration. | Task 9 |
| `mur-core/src/cmd/agent/service.rs:241` | `mur` agent/service command | `unsupported` | Retain the explicit non-macOS/Linux fallback and document the unsupported service integration. | Task 9 |
| `mur-core/src/cmd/agent/service.rs:246` | `mur` agent/service command | `unsupported` | Retain the explicit non-macOS/Linux fallback and document the unsupported service integration. | Task 9 |
| `mur-core/src/cmd/agent/service.rs:251` | `mur` agent/service command | `unsupported` | Retain the explicit non-macOS/Linux fallback and document the unsupported service integration. | Task 9 |
| `mur-core/src/cmd/agent/service.rs:276` | `mur` agent/service command | `unsupported` | Retain the explicit non-macOS/Linux fallback and document the unsupported service integration. | Task 9 |
| `mur-core/src/cmd/agent/service.rs:277` | `mur` agent/service command | `unsupported` | Retain the explicit non-macOS/Linux fallback and document the unsupported service integration. | Task 9 |
| `mur-core/src/cmd/agent/service.rs:283` | `mur` agent/service command | `unsupported` | Retain the explicit non-macOS/Linux fallback and document the unsupported service integration. | Task 9 |
| `mur-core/src/cmd/agent/service.rs:319` | `mur` agent/service command | `unsupported` | Retain the explicit non-macOS/Linux fallback and document the unsupported service integration. | Task 9 |
| `mur-core/src/cmd/agent/service.rs:346` | `mur` agent/service command | `unsupported` | Retain the explicit non-macOS/Linux fallback and document the unsupported service integration. | Task 9 |
| `mur-core/src/cmd/agent/service.rs:359` | `mur` agent/service command | `unsupported` | Retain the explicit non-macOS/Linux fallback and document the unsupported service integration. | Task 9 |
| `mur-core/src/cmd/agent/service.rs:369` | `mur` agent/service command | `unsupported` | Retain the explicit non-macOS/Linux fallback and document the unsupported service integration. | Task 9 |
| `mur-core/src/cmd/agent/service.rs:403` | `mur` agent/service command | `unsupported` | Retain the explicit non-macOS/Linux fallback and document the unsupported service integration. | Task 9 |
| `mur-core/src/cmd/agent/skill_github.rs:27` | `mur` CLI path | `portable` | No source change identified; native workspace tests verify the existing fallback. | Task 6 |
| `mur-core/src/cmd/agent/stale.rs:127` | `mur` CLI path | `portable` | No source change identified; native workspace tests verify the existing fallback. | Task 6 |
| `mur-core/src/cmd/agent/stale.rs:148` | `mur` CLI path | `portable` | No source change identified; native workspace tests verify the existing fallback. | Task 6 |
| `mur-core/src/cmd/agent/stale.rs:187` | `mur` CLI path | `portable` | No source change identified; native workspace tests verify the existing fallback. | Task 6 |
| `mur-core/src/cmd/agent/start.rs:5` | `mur` agent/service command | `unsupported` | Retain the explicit non-macOS/Linux fallback and document the unsupported service integration. | Task 9 |
| `mur-core/src/cmd/agent/start.rs:6` | `mur` agent/service command | `unsupported` | Retain the explicit non-macOS/Linux fallback and document the unsupported service integration. | Task 9 |
| `mur-core/src/cmd/agent/start.rs:68` | `mur` agent/service command | `unsupported` | Retain the explicit non-macOS/Linux fallback and document the unsupported service integration. | Task 9 |
| `mur-core/src/cmd/agent/start.rs:75` | `mur` agent/service command | `unsupported` | Retain the explicit non-macOS/Linux fallback and document the unsupported service integration. | Task 9 |
| `mur-core/src/cmd/agent/start.rs:87` | `mur` agent/service command | `unsupported` | Retain the explicit non-macOS/Linux fallback and document the unsupported service integration. | Task 9 |
| `mur-core/src/cmd/agent/start.rs:97` | `mur` agent/service command | `unsupported` | Retain the explicit non-macOS/Linux fallback and document the unsupported service integration. | Task 9 |
| `mur-core/src/cmd/agent/start.rs:101` | `mur` agent/service command | `unsupported` | Retain the explicit non-macOS/Linux fallback and document the unsupported service integration. | Task 9 |
| `mur-core/src/cmd/agent/start.rs:107` | `mur` agent/service command | `unsupported` | Retain the explicit non-macOS/Linux fallback and document the unsupported service integration. | Task 9 |
| `mur-core/src/cmd/agent/start.rs:111` | `mur` agent/service command | `unsupported` | Retain the explicit non-macOS/Linux fallback and document the unsupported service integration. | Task 9 |
| `mur-core/src/cmd/agent/start.rs:113` | `mur` agent/service command | `unsupported` | Retain the explicit non-macOS/Linux fallback and document the unsupported service integration. | Task 9 |
| `mur-core/src/cmd/agent/start.rs:121` | `mur` agent/service command | `unsupported` | Retain the explicit non-macOS/Linux fallback and document the unsupported service integration. | Task 9 |
| `mur-core/src/cmd/agent/start.rs:125` | `mur` agent/service command | `unsupported` | Retain the explicit non-macOS/Linux fallback and document the unsupported service integration. | Task 9 |
| `mur-core/src/cmd/agent/start.rs:214` | `mur` agent/service command | `unsupported` | Retain the explicit non-macOS/Linux fallback and document the unsupported service integration. | Task 9 |
| `mur-core/src/cmd/agent/start.rs:233` | `mur` agent/service command | `unsupported` | Retain the explicit non-macOS/Linux fallback and document the unsupported service integration. | Task 9 |
| `mur-core/src/cmd/agent/start.rs:241` | `mur` agent/service command | `unsupported` | Retain the explicit non-macOS/Linux fallback and document the unsupported service integration. | Task 9 |
| `mur-core/src/cmd/agent/stats.rs:106` | `mur` CLI path | `portable` | No source change identified; native workspace tests verify the existing fallback. | Task 6 |
| `mur-core/src/cmd/agent/stats.rs:114` | `mur` CLI path | `portable` | No source change identified; native workspace tests verify the existing fallback. | Task 6 |
| `mur-core/src/cmd/agent/stats.rs:116` | `mur` CLI path | `portable` | No source change identified; native workspace tests verify the existing fallback. | Task 6 |
| `mur-core/src/cmd/agent/stats.rs:137` | `mur` CLI path | `portable` | No source change identified; native workspace tests verify the existing fallback. | Task 6 |
| `mur-core/src/cmd/agent/stats.rs:160` | `mur` CLI path | `portable` | No source change identified; native workspace tests verify the existing fallback. | Task 6 |
| `mur-core/src/cmd/agent/stats.rs:179` | `mur` CLI path | `portable` | No source change identified; native workspace tests verify the existing fallback. | Task 6 |
| `mur-core/src/cmd/agent/stats.rs:182` | `mur` CLI path | `portable` | No source change identified; native workspace tests verify the existing fallback. | Task 6 |
| `mur-core/src/cmd/agent/stats.rs:197` | `mur` CLI path | `portable` | No source change identified; native workspace tests verify the existing fallback. | Task 6 |
| `mur-core/src/cmd/agent/stats.rs:210` | `mur` CLI path | `portable` | No source change identified; native workspace tests verify the existing fallback. | Task 6 |
| `mur-core/src/cmd/agent/stats.rs:216` | `mur` CLI path | `portable` | No source change identified; native workspace tests verify the existing fallback. | Task 6 |
| `mur-core/src/cmd/agent_export_gui.rs:650` | `mur` CLI path | `portable` | No source change identified; native workspace tests verify the existing fallback. | Task 6 |
| `mur-core/src/cmd/agent_export_gui.rs:657` | `mur` CLI path | `portable` | No source change identified; native workspace tests verify the existing fallback. | Task 6 |
| `mur-core/src/cmd/agent_export_gui.rs:707` | `mur` CLI path | `portable` | No source change identified; native workspace tests verify the existing fallback. | Task 6 |
| `mur-core/src/cmd/agent_export_gui.rs:743` | `mur` CLI path | `portable` | No source change identified; native workspace tests verify the existing fallback. | Task 6 |
| `mur-core/src/cmd/agent_export_gui.rs:793` | `mur` CLI path | `portable` | No source change identified; native workspace tests verify the existing fallback. | Task 6 |
| `mur-core/src/cmd/agent_export_gui.rs:849` | `mur` CLI path | `portable` | No source change identified; native workspace tests verify the existing fallback. | Task 6 |
| `mur-core/src/cmd/agent_export_gui.rs:871` | `mur` CLI path | `portable` | No source change identified; native workspace tests verify the existing fallback. | Task 6 |
| `mur-core/src/cmd/agent_export_gui.rs:911` | `mur` CLI path | `portable` | No source change identified; native workspace tests verify the existing fallback. | Task 6 |
| `mur-core/src/cmd/agent_export_gui.rs:1036` | `mur` CLI path | `portable` | No source change identified; native workspace tests verify the existing fallback. | Task 6 |
| `mur-core/src/cmd/browser/mod.rs:536` | `mur` CLI path | `portable` | No source change identified; native workspace tests verify the existing fallback. | Task 6 |
| `mur-core/src/cmd/browser/mod.rs:586` | `mur` CLI path | `portable` | No source change identified; native workspace tests verify the existing fallback. | Task 6 |
| `mur-core/src/cmd/browser/mod.rs:614` | `mur` CLI path | `portable` | No source change identified; native workspace tests verify the existing fallback. | Task 6 |
| `mur-core/src/cmd/browser/mod.rs:630` | `mur` CLI path | `portable` | No source change identified; native workspace tests verify the existing fallback. | Task 6 |
| `mur-core/src/cmd/browser/mod.rs:642` | `mur` CLI path | `portable` | No source change identified; native workspace tests verify the existing fallback. | Task 6 |
| `mur-core/src/cmd/browser/mod.rs:651` | `mur` CLI path | `portable` | No source change identified; native workspace tests verify the existing fallback. | Task 6 |
| `mur-core/src/cmd/deep_research/ask.rs:55` | `mur` CLI path | `portable` | No source change identified; native workspace tests verify the existing fallback. | Task 6 |
| `mur-core/src/cmd/doctor.rs:260` | `mur` CLI path | `portable` | No source change identified; native workspace tests verify the existing fallback. | Task 6 |
| `mur-core/src/cmd/doctor.rs:275` | `mur` CLI path | `portable` | No source change identified; native workspace tests verify the existing fallback. | Task 6 |
| `mur-core/src/cmd/doctor.rs:294` | `mur` CLI path | `portable` | No source change identified; native workspace tests verify the existing fallback. | Task 6 |
| `mur-core/src/cmd/doctor.rs:316` | `mur` CLI path | `portable` | No source change identified; native workspace tests verify the existing fallback. | Task 6 |
| `mur-core/src/cmd/doctor.rs:334` | `mur` CLI path | `portable` | No source change identified; native workspace tests verify the existing fallback. | Task 6 |
| `mur-core/src/cmd/doctor.rs:338` | `mur` CLI path | `portable` | No source change identified; native workspace tests verify the existing fallback. | Task 6 |
| `mur-core/src/cmd/doctor.rs:356` | `mur` CLI path | `portable` | No source change identified; native workspace tests verify the existing fallback. | Task 6 |
| `mur-core/src/cmd/doctor.rs:369` | `mur` CLI path | `portable` | No source change identified; native workspace tests verify the existing fallback. | Task 6 |
| `mur-core/src/cmd/doctor.rs:455` | `mur` CLI path | `portable` | No source change identified; native workspace tests verify the existing fallback. | Task 6 |
| `mur-core/src/cmd/doctor.rs:817` | `mur` CLI path | `portable` | No source change identified; native workspace tests verify the existing fallback. | Task 6 |
| `mur-core/src/cmd/init_daemon.rs:1` | `mur` agent/service command | `unsupported` | Retain the explicit non-macOS/Linux fallback and document the unsupported service integration. | Task 9 |
| `mur-core/src/cmd/init_daemon.rs:9` | `mur` agent/service command | `unsupported` | Retain the explicit non-macOS/Linux fallback and document the unsupported service integration. | Task 9 |
| `mur-core/src/cmd/init_daemon.rs:11` | `mur` agent/service command | `unsupported` | Retain the explicit non-macOS/Linux fallback and document the unsupported service integration. | Task 9 |
| `mur-core/src/cmd/init_daemon.rs:14` | `mur` agent/service command | `unsupported` | Retain the explicit non-macOS/Linux fallback and document the unsupported service integration. | Task 9 |
| `mur-core/src/cmd/init_daemon.rs:16` | `mur` agent/service command | `unsupported` | Retain the explicit non-macOS/Linux fallback and document the unsupported service integration. | Task 9 |
| `mur-core/src/cmd/init_daemon.rs:23` | `mur` agent/service command | `unsupported` | Retain the explicit non-macOS/Linux fallback and document the unsupported service integration. | Task 9 |
| `mur-core/src/cmd/init_daemon.rs:24` | `mur` agent/service command | `unsupported` | Retain the explicit non-macOS/Linux fallback and document the unsupported service integration. | Task 9 |
| `mur-core/src/cmd/init_daemon.rs:73` | `mur` agent/service command | `unsupported` | Retain the explicit non-macOS/Linux fallback and document the unsupported service integration. | Task 9 |
| `mur-core/src/cmd/init_daemon.rs:74` | `mur` agent/service command | `unsupported` | Retain the explicit non-macOS/Linux fallback and document the unsupported service integration. | Task 9 |
| `mur-core/src/cmd/init_daemon.rs:78` | `mur` agent/service command | `unsupported` | Retain the explicit non-macOS/Linux fallback and document the unsupported service integration. | Task 9 |
| `mur-core/src/cmd/init_daemon.rs:91` | `mur` agent/service command | `unsupported` | Retain the explicit non-macOS/Linux fallback and document the unsupported service integration. | Task 9 |
| `mur-core/src/cmd/init_local.rs:17` | `mur` CLI path | `portable` | No source change identified; native workspace tests verify the existing fallback. | Task 6 |
| `mur-core/src/cmd/media/resolve.rs:35` | `mur` CLI path | `portable` | No source change identified; native workspace tests verify the existing fallback. | Task 6 |
| `mur-core/src/cmd/misc.rs:110` | `mur` CLI path | `portable` | No source change identified; native workspace tests verify the existing fallback. | Task 6 |
| `mur-core/src/cmd/misc.rs:131` | `mur` CLI path | `portable` | No source change identified; native workspace tests verify the existing fallback. | Task 6 |
| `mur-core/src/cmd/misc.rs:213` | `mur` CLI path | `portable` | No source change identified; native workspace tests verify the existing fallback. | Task 6 |
| `mur-core/src/cmd/project/mod.rs:663` | `mur` CLI path | `portable` | No source change identified; native workspace tests verify the existing fallback. | Task 6 |
| `mur-core/src/cmd/project/mod.rs:665` | `mur` CLI path | `portable` | No source change identified; native workspace tests verify the existing fallback. | Task 6 |
| `mur-core/src/cmd/project/mod.rs:672` | `mur` CLI path | `portable` | No source change identified; native workspace tests verify the existing fallback. | Task 6 |
| `mur-core/src/cmd/project/mod.rs:678` | `mur` CLI path | `portable` | No source change identified; native workspace tests verify the existing fallback. | Task 6 |
| `mur-core/src/cmd/project/mod.rs:682` | `mur` CLI path | `portable` | No source change identified; native workspace tests verify the existing fallback. | Task 6 |
| `mur-core/src/cmd/project/mod.rs:683` | `mur` CLI path | `portable` | No source change identified; native workspace tests verify the existing fallback. | Task 6 |
| `mur-core/src/cmd/project/mod.rs:685` | `mur` CLI path | `portable` | No source change identified; native workspace tests verify the existing fallback. | Task 6 |
| `mur-core/src/cmd/session.rs:72` | `mur` CLI path | `portable` | No source change identified; native workspace tests verify the existing fallback. | Task 6 |
| `mur-core/src/cmd/session.rs:74` | `mur` CLI path | `portable` | No source change identified; native workspace tests verify the existing fallback. | Task 6 |
| `mur-core/src/cmd/session.rs:80` | `mur` CLI path | `portable` | No source change identified; native workspace tests verify the existing fallback. | Task 6 |
| `mur-core/src/cmd/session.rs:947` | `mur` CLI path | `portable` | No source change identified; native workspace tests verify the existing fallback. | Task 6 |
| `mur-core/src/cmd/session.rs:951` | `mur` CLI path | `portable` | No source change identified; native workspace tests verify the existing fallback. | Task 6 |
| `mur-core/src/cmd/session.rs:955` | `mur` CLI path | `portable` | No source change identified; native workspace tests verify the existing fallback. | Task 6 |
| `mur-core/src/cmd/source_cmd.rs:66` | `mur` CLI path | `portable` | No source change identified; native workspace tests verify the existing fallback. | Task 6 |
| `mur-core/src/cmd/source_cmd.rs:789` | `mur` CLI path | `portable` | No source change identified; native workspace tests verify the existing fallback. | Task 6 |
| `mur-core/src/cmd/source_cmd.rs:798` | `mur` CLI path | `portable` | No source change identified; native workspace tests verify the existing fallback. | Task 6 |
| `mur-core/src/cmd/source_cmd.rs:833` | `mur` CLI path | `portable` | No source change identified; native workspace tests verify the existing fallback. | Task 6 |
| `mur-core/src/cmd/source_cmd.rs:837` | `mur` CLI path | `portable` | No source change identified; native workspace tests verify the existing fallback. | Task 6 |
| `mur-core/src/cmd/source_cmd.rs:857` | `mur` CLI path | `portable` | No source change identified; native workspace tests verify the existing fallback. | Task 6 |
| `mur-core/src/cmd/system_schedule.rs:1` | `mur` agent/service command | `unsupported` | Retain the explicit non-macOS/Linux fallback and document the unsupported service integration. | Task 9 |
| `mur-core/src/cmd/system_schedule.rs:17` | `mur` agent/service command | `unsupported` | Retain the explicit non-macOS/Linux fallback and document the unsupported service integration. | Task 9 |
| `mur-core/src/cmd/system_schedule.rs:38` | `mur` agent/service command | `unsupported` | Retain the explicit non-macOS/Linux fallback and document the unsupported service integration. | Task 9 |
| `mur-core/src/cmd/system_schedule.rs:39` | `mur` agent/service command | `unsupported` | Retain the explicit non-macOS/Linux fallback and document the unsupported service integration. | Task 9 |
| `mur-core/src/cmd/system_schedule.rs:47` | `mur` agent/service command | `unsupported` | Retain the explicit non-macOS/Linux fallback and document the unsupported service integration. | Task 9 |
| `mur-core/src/cmd/system_schedule.rs:48` | `mur` agent/service command | `unsupported` | Retain the explicit non-macOS/Linux fallback and document the unsupported service integration. | Task 9 |
| `mur-core/src/cmd/system_schedule.rs:56` | `mur` agent/service command | `unsupported` | Retain the explicit non-macOS/Linux fallback and document the unsupported service integration. | Task 9 |
| `mur-core/src/cmd/system_schedule.rs:57` | `mur` agent/service command | `unsupported` | Retain the explicit non-macOS/Linux fallback and document the unsupported service integration. | Task 9 |
| `mur-core/src/cmd/system_schedule.rs:63` | `mur` agent/service command | `unsupported` | Retain the explicit non-macOS/Linux fallback and document the unsupported service integration. | Task 9 |
| `mur-core/src/cmd/system_schedule.rs:72` | `mur` agent/service command | `unsupported` | Retain the explicit non-macOS/Linux fallback and document the unsupported service integration. | Task 9 |
| `mur-core/src/cmd/system_schedule.rs:74` | `mur` agent/service command | `unsupported` | Retain the explicit non-macOS/Linux fallback and document the unsupported service integration. | Task 9 |
| `mur-core/src/cmd/system_schedule.rs:111` | `mur` agent/service command | `unsupported` | Retain the explicit non-macOS/Linux fallback and document the unsupported service integration. | Task 9 |
| `mur-core/src/cmd/system_schedule.rs:179` | `mur` agent/service command | `unsupported` | Retain the explicit non-macOS/Linux fallback and document the unsupported service integration. | Task 9 |
| `mur-core/src/cmd/system_schedule.rs:190` | `mur` agent/service command | `unsupported` | Retain the explicit non-macOS/Linux fallback and document the unsupported service integration. | Task 9 |
| `mur-core/src/cmd/system_schedule.rs:205` | `mur` agent/service command | `unsupported` | Retain the explicit non-macOS/Linux fallback and document the unsupported service integration. | Task 9 |
| `mur-core/src/cmd/system_schedule.rs:345` | `mur` agent/service command | `unsupported` | Retain the explicit non-macOS/Linux fallback and document the unsupported service integration. | Task 9 |
| `mur-core/src/cmd/system_schedule.rs:346` | `mur` agent/service command | `unsupported` | Retain the explicit non-macOS/Linux fallback and document the unsupported service integration. | Task 9 |
| `mur-core/src/cmd/system_schedule.rs:416` | `mur` agent/service command | `unsupported` | Retain the explicit non-macOS/Linux fallback and document the unsupported service integration. | Task 9 |
| `mur-core/src/cmd/workflow.rs:54` | `mur` CLI path | `portable` | No source change identified; native workspace tests verify the existing fallback. | Task 6 |
| `mur-core/src/cmd/workflow.rs:959` | `mur` CLI path | `portable` | No source change identified; native workspace tests verify the existing fallback. | Task 6 |
| `mur-core/src/cmd/workflow.rs:1042` | `mur` CLI path | `portable` | No source change identified; native workspace tests verify the existing fallback. | Task 6 |
| `mur-core/src/cmd/workflow.rs:1074` | `mur` CLI path | `portable` | No source change identified; native workspace tests verify the existing fallback. | Task 6 |
| `mur-core/src/cmd/workflow.rs:1077` | `mur` CLI path | `portable` | No source change identified; native workspace tests verify the existing fallback. | Task 6 |
| `mur-core/src/cmd/workflow.rs:1079` | `mur` CLI path | `portable` | No source change identified; native workspace tests verify the existing fallback. | Task 6 |
| `mur-core/src/model_setup/slots.rs:97` | `mur` CLI path | `portable` | No source change identified; native workspace tests verify the existing fallback. | Task 6 |
| `mur-core/src/monitor/notify/desktop.rs:12` | `mur` CLI path | `portable` | No source change identified; native workspace tests verify the existing fallback. | Task 6 |
| `mur-core/src/monitor/notify/desktop.rs:18` | `mur` CLI path | `portable` | No source change identified; native workspace tests verify the existing fallback. | Task 6 |
| `mur-core/src/monitor/notify/desktop.rs:28` | `mur` CLI path | `portable` | No source change identified; native workspace tests verify the existing fallback. | Task 6 |
| `mur-core/src/monitor/notify/desktop.rs:40` | `mur` CLI path | `portable` | No source change identified; native workspace tests verify the existing fallback. | Task 6 |
| `mur-core/src/monitor/notify/desktop.rs:62` | `mur` CLI path | `portable` | No source change identified; native workspace tests verify the existing fallback. | Task 6 |
| `mur-core/src/monitor/notify/desktop.rs:64` | `mur` CLI path | `portable` | No source change identified; native workspace tests verify the existing fallback. | Task 6 |
| `mur-core/src/monitor/notify/desktop.rs:70` | `mur` CLI path | `portable` | No source change identified; native workspace tests verify the existing fallback. | Task 6 |
| `mur-core/src/monitor/notify/desktop.rs:72` | `mur` CLI path | `portable` | No source change identified; native workspace tests verify the existing fallback. | Task 6 |
| `mur-core/src/monitor/notify/desktop.rs:74` | `mur` CLI path | `portable` | No source change identified; native workspace tests verify the existing fallback. | Task 6 |
| `mur-core/src/monitor/notify/desktop.rs:80` | `mur` CLI path | `portable` | No source change identified; native workspace tests verify the existing fallback. | Task 6 |
| `mur-core/src/monitor/notify/desktop.rs:82` | `mur` CLI path | `portable` | No source change identified; native workspace tests verify the existing fallback. | Task 6 |
| `mur-core/src/monitor/notify/desktop.rs:95` | `mur` CLI path | `portable` | No source change identified; native workspace tests verify the existing fallback. | Task 6 |
| `mur-core/src/monitor/notify/desktop.rs:104` | `mur` CLI path | `portable` | No source change identified; native workspace tests verify the existing fallback. | Task 6 |
| `mur-core/src/parallel/backend/cow.rs:22` | `mur` CLI path | `portable` | No source change identified; native workspace tests verify the existing fallback. | Task 6 |
| `mur-core/src/parallel/backend/cow.rs:37` | `mur` CLI path | `portable` | No source change identified; native workspace tests verify the existing fallback. | Task 6 |
| `mur-core/src/parallel/backend/cow.rs:40` | `mur` CLI path | `portable` | No source change identified; native workspace tests verify the existing fallback. | Task 6 |
| `mur-core/src/parallel/backend/cow.rs:43` | `mur` CLI path | `portable` | No source change identified; native workspace tests verify the existing fallback. | Task 6 |
| `mur-core/src/parallel/backend/cow.rs:111` | `mur` CLI path | `portable` | No source change identified; native workspace tests verify the existing fallback. | Task 6 |
| `mur-core/src/parallel/backend/cow.rs:154` | `mur` CLI path | `portable` | No source change identified; native workspace tests verify the existing fallback. | Task 6 |
| `mur-core/src/parallel/backend/detect.rs:19` | `mur` CLI path | `portable` | No source change identified; native workspace tests verify the existing fallback. | Task 6 |
| `mur-core/src/parallel/backend/detect.rs:25` | `mur` CLI path | `portable` | No source change identified; native workspace tests verify the existing fallback. | Task 6 |
| `mur-core/src/schedule_status.rs:5` | `mur` CLI path | `portable` | No source change identified; native workspace tests verify the existing fallback. | Task 6 |
| `mur-core/src/server/mod.rs:381` | `mur` CLI path | `portable` | No source change identified; native workspace tests verify the existing fallback. | Task 6 |
| `mur-core/src/server/mod.rs:385` | `mur` CLI path | `portable` | No source change identified; native workspace tests verify the existing fallback. | Task 6 |
| `mur-core/src/server/mod.rs:389` | `mur` CLI path | `portable` | No source change identified; native workspace tests verify the existing fallback. | Task 6 |
| `mur-core/src/update/mod.rs:87` | `mur` update path | `explicit-freebsd` | Add or verify FreeBSD asset/pkg behavior with host-independent tests. | Task 3 |
| `mur-core/src/update/mod.rs:133` | `mur` update path | `explicit-freebsd` | Add or verify FreeBSD asset/pkg behavior with host-independent tests. | Task 3 |
| `mur-core/src/update/mod.rs:141` | `mur` update path | `explicit-freebsd` | Add or verify FreeBSD asset/pkg behavior with host-independent tests. | Task 3 |
| `mur-core/src/update/mod.rs:148` | `mur` update path | `explicit-freebsd` | Add or verify FreeBSD asset/pkg behavior with host-independent tests. | Task 3 |
| `mur-core/src/update/mod.rs:179` | `mur` update path | `explicit-freebsd` | Add or verify FreeBSD asset/pkg behavior with host-independent tests. | Task 3 |
| `mur-core/src/update/source.rs:44` | `mur` update path | `explicit-freebsd` | FreeBSD branch of install-source detection; covered by host-independent tests. | Task 3 |
| `mur-core/src/update/source.rs:54` | `mur` update path | `explicit-freebsd` | Non-FreeBSD branch of install-source detection; covered by host-independent tests. | Task 3 |
| `mur-core/src/update/release.rs:20` | `mur` update path | `explicit-freebsd` | Add or verify FreeBSD asset/pkg behavior with host-independent tests. | Task 3 |
| `mur-core/src/update/resign.rs:5` | `mur` update path | `explicit-freebsd` | Add or verify FreeBSD asset/pkg behavior with host-independent tests. | Task 3 |
| `mur-core/src/update/resign.rs:17` | `mur` update path | `explicit-freebsd` | Add or verify FreeBSD asset/pkg behavior with host-independent tests. | Task 3 |
| `mur-core/src/update/resign.rs:31` | `mur` update path | `explicit-freebsd` | Add or verify FreeBSD asset/pkg behavior with host-independent tests. | Task 3 |
| `mur-core/src/update/resign.rs:36` | `mur` update path | `explicit-freebsd` | Add or verify FreeBSD asset/pkg behavior with host-independent tests. | Task 3 |
| `mur-core/src/update/resign.rs:38` | `mur` update path | `explicit-freebsd` | Add or verify FreeBSD asset/pkg behavior with host-independent tests. | Task 3 |
| `mur-core/src/update/resign.rs:142` | `mur` update path | `explicit-freebsd` | Add or verify FreeBSD asset/pkg behavior with host-independent tests. | Task 3 |
| `mur-core/src/update/resign.rs:154` | `mur` update path | `explicit-freebsd` | Add or verify FreeBSD asset/pkg behavior with host-independent tests. | Task 3 |
| `mur-core/src/update/resign.rs:161` | `mur` update path | `explicit-freebsd` | Add or verify FreeBSD asset/pkg behavior with host-independent tests. | Task 3 |
| `mur-core/src/update/resign.rs:179` | `mur` update path | `explicit-freebsd` | Add or verify FreeBSD asset/pkg behavior with host-independent tests. | Task 3 |
| `mur-core/src/update/resign.rs:255` | `mur` update path | `explicit-freebsd` | Add or verify FreeBSD asset/pkg behavior with host-independent tests. | Task 3 |
| `mur-core/src/update/resign.rs:305` | `mur` update path | `explicit-freebsd` | Add or verify FreeBSD asset/pkg behavior with host-independent tests. | Task 3 |
| `mur-core/tests/agent_card_ephemeral.rs:46` | test-only | `portable` | Keep compiling/running under the native FreeBSD workspace test lane. | Task 6 |
| `mur-core/tests/agent_install_service.rs:1` | test-only | `portable` | Keep compiling/running under the native FreeBSD workspace test lane. | Task 6 |
| `mur-core/tests/agent_install_service.rs:44` | test-only | `portable` | Keep compiling/running under the native FreeBSD workspace test lane. | Task 6 |
| `mur-core/tests/agent_install_service.rs:53` | test-only | `portable` | Keep compiling/running under the native FreeBSD workspace test lane. | Task 6 |
| `mur-core/tests/agent_start_without_symlink.rs:6` | test-only | `portable` | Keep compiling/running under the native FreeBSD workspace test lane. | Task 6 |
| `mur-core/tests/gate_golden.rs:143` | test-only | `portable` | Keep compiling/running under the native FreeBSD workspace test lane. | Task 6 |
| `mur-gui-core/src/autostart/linux.rs:1` | not reachable (GUI-only crate) | `unsupported` | No release-binary change; FreeBSD GUI is outside this support scope. | Task 9 |
| `mur-gui-core/src/autostart/linux.rs:3` | not reachable (GUI-only crate) | `unsupported` | No release-binary change; FreeBSD GUI is outside this support scope. | Task 9 |
| `mur-gui-core/src/autostart/linux.rs:17` | not reachable (GUI-only crate) | `unsupported` | No release-binary change; FreeBSD GUI is outside this support scope. | Task 9 |
| `mur-gui-core/src/autostart/linux.rs:36` | not reachable (GUI-only crate) | `unsupported` | No release-binary change; FreeBSD GUI is outside this support scope. | Task 9 |
| `mur-gui-core/src/autostart/linux.rs:106` | not reachable (GUI-only crate) | `unsupported` | No release-binary change; FreeBSD GUI is outside this support scope. | Task 9 |
| `mur-gui-core/src/autostart/macos.rs:1` | not reachable (GUI-only crate) | `unsupported` | No release-binary change; FreeBSD GUI is outside this support scope. | Task 9 |
| `mur-gui-core/src/autostart/macos.rs:11` | not reachable (GUI-only crate) | `unsupported` | No release-binary change; FreeBSD GUI is outside this support scope. | Task 9 |
| `mur-gui-core/src/autostart/macos.rs:12` | not reachable (GUI-only crate) | `unsupported` | No release-binary change; FreeBSD GUI is outside this support scope. | Task 9 |
| `mur-gui-core/src/autostart/macos.rs:24` | not reachable (GUI-only crate) | `unsupported` | No release-binary change; FreeBSD GUI is outside this support scope. | Task 9 |
| `mur-gui-core/src/autostart/macos.rs:117` | not reachable (GUI-only crate) | `unsupported` | No release-binary change; FreeBSD GUI is outside this support scope. | Task 9 |
| `mur-gui-core/src/autostart/macos.rs:158` | not reachable (GUI-only crate) | `unsupported` | No release-binary change; FreeBSD GUI is outside this support scope. | Task 9 |
| `mur-gui-core/src/autostart/macos.rs:182` | not reachable (GUI-only crate) | `unsupported` | No release-binary change; FreeBSD GUI is outside this support scope. | Task 9 |
| `mur-gui-core/src/autostart/macos.rs:237` | not reachable (GUI-only crate) | `unsupported` | No release-binary change; FreeBSD GUI is outside this support scope. | Task 9 |
| `mur-gui-core/src/autostart/mod.rs:9` | not reachable (GUI-only crate) | `unsupported` | No release-binary change; FreeBSD GUI is outside this support scope. | Task 9 |
| `mur-gui-core/src/autostart/mod.rs:11` | not reachable (GUI-only crate) | `unsupported` | No release-binary change; FreeBSD GUI is outside this support scope. | Task 9 |
| `mur-gui-core/src/autostart/mod.rs:13` | not reachable (GUI-only crate) | `unsupported` | No release-binary change; FreeBSD GUI is outside this support scope. | Task 9 |
| `mur-gui-core/src/autostart/mod.rs:23` | not reachable (GUI-only crate) | `unsupported` | No release-binary change; FreeBSD GUI is outside this support scope. | Task 9 |
| `mur-gui-core/src/autostart/mod.rs:25` | not reachable (GUI-only crate) | `unsupported` | No release-binary change; FreeBSD GUI is outside this support scope. | Task 9 |
| `mur-gui-core/src/autostart/mod.rs:27` | not reachable (GUI-only crate) | `unsupported` | No release-binary change; FreeBSD GUI is outside this support scope. | Task 9 |
| `mur-gui-core/src/autostart/mod.rs:29` | not reachable (GUI-only crate) | `unsupported` | No release-binary change; FreeBSD GUI is outside this support scope. | Task 9 |
| `mur-gui-core/src/autostart/mod.rs:38` | not reachable (GUI-only crate) | `unsupported` | No release-binary change; FreeBSD GUI is outside this support scope. | Task 9 |
| `mur-gui-core/src/autostart/mod.rs:40` | not reachable (GUI-only crate) | `unsupported` | No release-binary change; FreeBSD GUI is outside this support scope. | Task 9 |
| `mur-gui-core/src/autostart/mod.rs:42` | not reachable (GUI-only crate) | `unsupported` | No release-binary change; FreeBSD GUI is outside this support scope. | Task 9 |
| `mur-gui-core/src/autostart/mod.rs:44` | not reachable (GUI-only crate) | `unsupported` | No release-binary change; FreeBSD GUI is outside this support scope. | Task 9 |
| `mur-gui-core/src/autostart/mod.rs:53` | not reachable (GUI-only crate) | `unsupported` | No release-binary change; FreeBSD GUI is outside this support scope. | Task 9 |
| `mur-gui-core/src/autostart/mod.rs:55` | not reachable (GUI-only crate) | `unsupported` | No release-binary change; FreeBSD GUI is outside this support scope. | Task 9 |
| `mur-gui-core/src/autostart/mod.rs:57` | not reachable (GUI-only crate) | `unsupported` | No release-binary change; FreeBSD GUI is outside this support scope. | Task 9 |
| `mur-gui-core/src/autostart/mod.rs:59` | not reachable (GUI-only crate) | `unsupported` | No release-binary change; FreeBSD GUI is outside this support scope. | Task 9 |
| `mur-gui-core/src/autostart/mod.rs:68` | not reachable (GUI-only crate) | `unsupported` | No release-binary change; FreeBSD GUI is outside this support scope. | Task 9 |
| `mur-gui-core/src/autostart/mod.rs:70` | not reachable (GUI-only crate) | `unsupported` | No release-binary change; FreeBSD GUI is outside this support scope. | Task 9 |
| `mur-gui-core/src/autostart/mod.rs:72` | not reachable (GUI-only crate) | `unsupported` | No release-binary change; FreeBSD GUI is outside this support scope. | Task 9 |
| `mur-gui-core/src/autostart/mod.rs:74` | not reachable (GUI-only crate) | `unsupported` | No release-binary change; FreeBSD GUI is outside this support scope. | Task 9 |
| `mur-gui-core/src/autostart/mod.rs:83` | not reachable (GUI-only crate) | `unsupported` | No release-binary change; FreeBSD GUI is outside this support scope. | Task 9 |
| `mur-gui-core/src/autostart/mod.rs:85` | not reachable (GUI-only crate) | `unsupported` | No release-binary change; FreeBSD GUI is outside this support scope. | Task 9 |
| `mur-gui-core/src/autostart/mod.rs:87` | not reachable (GUI-only crate) | `unsupported` | No release-binary change; FreeBSD GUI is outside this support scope. | Task 9 |
| `mur-gui-core/src/autostart/mod.rs:89` | not reachable (GUI-only crate) | `unsupported` | No release-binary change; FreeBSD GUI is outside this support scope. | Task 9 |
| `mur-gui-core/src/ipc/mod.rs:71` | not reachable (GUI-only crate) | `unsupported` | No release-binary change; FreeBSD GUI is outside this support scope. | Task 9 |
| `mur-gui-core/src/ipc/mod.rs:75` | not reachable (GUI-only crate) | `unsupported` | No release-binary change; FreeBSD GUI is outside this support scope. | Task 9 |
| `mur-gui-core/src/ipc/mod.rs:89` | not reachable (GUI-only crate) | `unsupported` | No release-binary change; FreeBSD GUI is outside this support scope. | Task 9 |
| `mur-gui-core/src/ipc/mod.rs:93` | not reachable (GUI-only crate) | `unsupported` | No release-binary change; FreeBSD GUI is outside this support scope. | Task 9 |
| `mur-gui-core/src/ipc/mod.rs:116` | not reachable (GUI-only crate) | `unsupported` | No release-binary change; FreeBSD GUI is outside this support scope. | Task 9 |
| `mur-gui-core/src/ipc/mod.rs:119` | not reachable (GUI-only crate) | `unsupported` | No release-binary change; FreeBSD GUI is outside this support scope. | Task 9 |
| `mur-gui-core/src/ipc/mod.rs:284` | not reachable (GUI-only crate) | `unsupported` | No release-binary change; FreeBSD GUI is outside this support scope. | Task 9 |
| `mur-gui-core/src/ipc/mod.rs:287` | not reachable (GUI-only crate) | `unsupported` | No release-binary change; FreeBSD GUI is outside this support scope. | Task 9 |
| `mur-gui-core/src/ipc/mod.rs:337` | not reachable (GUI-only crate) | `unsupported` | No release-binary change; FreeBSD GUI is outside this support scope. | Task 9 |
| `mur-gui-core/src/sidecar.rs:4` | not reachable (GUI-only crate) | `unsupported` | No release-binary change; FreeBSD GUI is outside this support scope. | Task 9 |
| `mur-gui-core/src/sidecar.rs:10` | not reachable (GUI-only crate) | `unsupported` | No release-binary change; FreeBSD GUI is outside this support scope. | Task 9 |
| `mur-gui-core/src/sidecar.rs:396` | not reachable (GUI-only crate) | `unsupported` | No release-binary change; FreeBSD GUI is outside this support scope. | Task 9 |
| `mur-gui-core/src/sidecar.rs:398` | not reachable (GUI-only crate) | `unsupported` | No release-binary change; FreeBSD GUI is outside this support scope. | Task 9 |
| `mur-gui-core/src/sidecar.rs:474` | not reachable (GUI-only crate) | `unsupported` | No release-binary change; FreeBSD GUI is outside this support scope. | Task 9 |
| `mur-gui-core/src/sidecar.rs:484` | not reachable (GUI-only crate) | `unsupported` | No release-binary change; FreeBSD GUI is outside this support scope. | Task 9 |
| `mur-gui-core/src/sidecar.rs:610` | not reachable (GUI-only crate) | `unsupported` | No release-binary change; FreeBSD GUI is outside this support scope. | Task 9 |
| `mur-gui-core/src/sidecar.rs:612` | not reachable (GUI-only crate) | `unsupported` | No release-binary change; FreeBSD GUI is outside this support scope. | Task 9 |
| `mur-gui-core/src/stub/mod.rs:16` | not reachable (GUI-only crate) | `unsupported` | No release-binary change; FreeBSD GUI is outside this support scope. | Task 9 |
| `mur-gui-core/src/stub/mod.rs:19` | not reachable (GUI-only crate) | `unsupported` | No release-binary change; FreeBSD GUI is outside this support scope. | Task 9 |
| `mur-gui-core/src/stub/mod.rs:22` | not reachable (GUI-only crate) | `unsupported` | No release-binary change; FreeBSD GUI is outside this support scope. | Task 9 |
| `mur-gui-core/src/stub/mod.rs:47` | not reachable (GUI-only crate) | `unsupported` | No release-binary change; FreeBSD GUI is outside this support scope. | Task 9 |
| `mur-gui-core/src/stub/mod.rs:58` | not reachable (GUI-only crate) | `unsupported` | No release-binary change; FreeBSD GUI is outside this support scope. | Task 9 |
| `mur-gui-core/src/stub/mod.rs:69` | not reachable (GUI-only crate) | `unsupported` | No release-binary change; FreeBSD GUI is outside this support scope. | Task 9 |
| `mur-gui-core/src/stub/mod.rs:80` | not reachable (GUI-only crate) | `unsupported` | No release-binary change; FreeBSD GUI is outside this support scope. | Task 9 |
| `mur-gui-core/src/stub/mod.rs:110` | not reachable (GUI-only crate) | `unsupported` | No release-binary change; FreeBSD GUI is outside this support scope. | Task 9 |
| `mur-gui-core/src/stub/mod.rs:113` | not reachable (GUI-only crate) | `unsupported` | No release-binary change; FreeBSD GUI is outside this support scope. | Task 9 |
| `mur-gui-core/src/stub/mod.rs:121` | not reachable (GUI-only crate) | `unsupported` | No release-binary change; FreeBSD GUI is outside this support scope. | Task 9 |
| `mur-gui-core/src/stub/mod.rs:169` | not reachable (GUI-only crate) | `unsupported` | No release-binary change; FreeBSD GUI is outside this support scope. | Task 9 |
| `mur-gui-core/src/voice/dnd.rs:23` | not reachable (GUI-only crate) | `unsupported` | No release-binary change; FreeBSD GUI is outside this support scope. | Task 9 |
| `mur-gui-core/src/voice/dnd.rs:82` | not reachable (GUI-only crate) | `unsupported` | No release-binary change; FreeBSD GUI is outside this support scope. | Task 9 |
| `mur-gui-core/src/voice/dnd.rs:103` | not reachable (GUI-only crate) | `unsupported` | No release-binary change; FreeBSD GUI is outside this support scope. | Task 9 |
| `mur-gui-core/src/voice/dnd.rs:112` | not reachable (GUI-only crate) | `unsupported` | No release-binary change; FreeBSD GUI is outside this support scope. | Task 9 |
| `mur-gui-core/src/voice/dnd.rs:162` | not reachable (GUI-only crate) | `unsupported` | No release-binary change; FreeBSD GUI is outside this support scope. | Task 9 |
| `mur-hub-gui/src-tauri/src/chat_window.rs:4` | not reachable (GUI-only crate) | `unsupported` | No release-binary change; FreeBSD GUI is outside this support scope. | Task 9 |
| `mur-hub-gui/src-tauri/src/chat_window.rs:66` | not reachable (GUI-only crate) | `unsupported` | No release-binary change; FreeBSD GUI is outside this support scope. | Task 9 |
| `mur-hub-gui/src-tauri/src/chatgpt_subscription/app_server.rs:379` | not reachable (GUI-only crate) | `unsupported` | No release-binary change; FreeBSD GUI is outside this support scope. | Task 9 |
| `mur-hub-gui/src-tauri/src/chatgpt_subscription/process.rs:178` | not reachable (GUI-only crate) | `unsupported` | No release-binary change; FreeBSD GUI is outside this support scope. | Task 9 |
| `mur-hub-gui/src-tauri/src/chatgpt_subscription/process.rs:184` | not reachable (GUI-only crate) | `unsupported` | No release-binary change; FreeBSD GUI is outside this support scope. | Task 9 |
| `mur-hub-gui/src-tauri/src/chatgpt_subscription/process.rs:185` | not reachable (GUI-only crate) | `unsupported` | No release-binary change; FreeBSD GUI is outside this support scope. | Task 9 |
| `mur-hub-gui/src-tauri/src/chatgpt_subscription/process.rs:306` | not reachable (GUI-only crate) | `unsupported` | No release-binary change; FreeBSD GUI is outside this support scope. | Task 9 |
| `mur-hub-gui/src-tauri/src/chatgpt_subscription/process.rs:331` | not reachable (GUI-only crate) | `unsupported` | No release-binary change; FreeBSD GUI is outside this support scope. | Task 9 |
| `mur-hub-gui/src-tauri/src/chatgpt_subscription/process.rs:352` | not reachable (GUI-only crate) | `unsupported` | No release-binary change; FreeBSD GUI is outside this support scope. | Task 9 |
| `mur-hub-gui/src-tauri/src/cli_tools.rs:104` | not reachable (GUI-only crate) | `unsupported` | No release-binary change; FreeBSD GUI is outside this support scope. | Task 9 |
| `mur-hub-gui/src-tauri/src/companion_notify.rs:8` | not reachable (GUI-only crate) | `unsupported` | No release-binary change; FreeBSD GUI is outside this support scope. | Task 9 |
| `mur-hub-gui/src-tauri/src/companion_notify.rs:10` | not reachable (GUI-only crate) | `unsupported` | No release-binary change; FreeBSD GUI is outside this support scope. | Task 9 |
| `mur-hub-gui/src-tauri/src/detail_window.rs:64` | not reachable (GUI-only crate) | `unsupported` | No release-binary change; FreeBSD GUI is outside this support scope. | Task 9 |
| `mur-hub-gui/src-tauri/src/import_muragent.rs:231` | not reachable (GUI-only crate) | `unsupported` | No release-binary change; FreeBSD GUI is outside this support scope. | Task 9 |
| `mur-hub-gui/src-tauri/src/import_muragent.rs:233` | not reachable (GUI-only crate) | `unsupported` | No release-binary change; FreeBSD GUI is outside this support scope. | Task 9 |
| `mur-hub-gui/src-tauri/src/lib.rs:150` | not reachable (GUI-only crate) | `unsupported` | No release-binary change; FreeBSD GUI is outside this support scope. | Task 9 |
| `mur-hub-gui/src-tauri/src/lib.rs:154` | not reachable (GUI-only crate) | `unsupported` | No release-binary change; FreeBSD GUI is outside this support scope. | Task 9 |
| `mur-hub-gui/src-tauri/src/lib.rs:492` | not reachable (GUI-only crate) | `unsupported` | No release-binary change; FreeBSD GUI is outside this support scope. | Task 9 |
| `mur-hub-gui/src-tauri/src/lib.rs:848` | not reachable (GUI-only crate) | `unsupported` | No release-binary change; FreeBSD GUI is outside this support scope. | Task 9 |
| `mur-hub-gui/src-tauri/src/macos_un.rs:19` | not reachable (GUI-only crate) | `unsupported` | No release-binary change; FreeBSD GUI is outside this support scope. | Task 9 |
| `mur-hub-gui/src-tauri/src/macos_un.rs:73` | not reachable (GUI-only crate) | `unsupported` | No release-binary change; FreeBSD GUI is outside this support scope. | Task 9 |
| `mur-hub-gui/src-tauri/src/mcp_skills.rs:405` | not reachable (GUI-only crate) | `unsupported` | No release-binary change; FreeBSD GUI is outside this support scope. | Task 9 |
| `mur-hub-gui/src-tauri/src/mcp_skills.rs:415` | not reachable (GUI-only crate) | `unsupported` | No release-binary change; FreeBSD GUI is outside this support scope. | Task 9 |
| `mur-hub-gui/src-tauri/src/mcp_skills.rs:425` | not reachable (GUI-only crate) | `unsupported` | No release-binary change; FreeBSD GUI is outside this support scope. | Task 9 |
| `mur-hub-gui/src-tauri/src/mlx_sidecar.rs:3` | not reachable (GUI-only crate) | `unsupported` | No release-binary change; FreeBSD GUI is outside this support scope. | Task 9 |
| `mur-hub-gui/src-tauri/src/onboarding/first_launch.rs:28` | not reachable (GUI-only crate) | `unsupported` | No release-binary change; FreeBSD GUI is outside this support scope. | Task 9 |
| `mur-hub-gui/src-tauri/src/onboarding/first_launch.rs:60` | not reachable (GUI-only crate) | `unsupported` | No release-binary change; FreeBSD GUI is outside this support scope. | Task 9 |
| `mur-hub-gui/src-tauri/src/onboarding/first_launch.rs:73` | not reachable (GUI-only crate) | `unsupported` | No release-binary change; FreeBSD GUI is outside this support scope. | Task 9 |
| `mur-hub-gui/src-tauri/src/onboarding/first_launch.rs:79` | not reachable (GUI-only crate) | `unsupported` | No release-binary change; FreeBSD GUI is outside this support scope. | Task 9 |
| `mur-hub-gui/src-tauri/src/onboarding/first_launch.rs:92` | not reachable (GUI-only crate) | `unsupported` | No release-binary change; FreeBSD GUI is outside this support scope. | Task 9 |
| `mur-hub-gui/src-tauri/src/panel/pos.rs:62` | not reachable (GUI-only crate) | `unsupported` | No release-binary change; FreeBSD GUI is outside this support scope. | Task 9 |
| `mur-hub-gui/src-tauri/src/panel/pos.rs:78` | not reachable (GUI-only crate) | `unsupported` | No release-binary change; FreeBSD GUI is outside this support scope. | Task 9 |
| `mur-hub-gui/src-tauri/src/panel/pos.rs:83` | not reachable (GUI-only crate) | `unsupported` | No release-binary change; FreeBSD GUI is outside this support scope. | Task 9 |
