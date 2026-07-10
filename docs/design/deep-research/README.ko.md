<!-- Languages: [English](README.md) · [繁體中文](README.zh-TW.md) · [简体中文](README.zh-CN.md) · [日本語](README.ja.md) · [한국어](README.ko.md) -->

# MUR Deep Research — 상세 설계

> MUR 플릿이 엔드투엔드로 소유하는 네이티브 딥 리서치.
> **v2.45.0**(2026-07-10)에 출시, PR #663–#672.

샌드박스화된 MUR 에이전트 분대가 질문을 분해하고, 단일 감사 게이트웨이를 통해 실시간 웹을 리서치하고, 각 주장을 적대적으로 검증하고, **인용이 달리고 암호학적으로 귀속 가능한 보고서**로 수렴한다 — 호스트 서브에이전트가 아니라 MUR 자체의 오케스트레이션 위에서.

- **명세:** `docs/superpowers/specs/2026-07-09-mur-native-deep-research-design.md`
- **크레이트:** `mur-research-gateway` · **명령:** `mur-core/src/cmd/deep_research`

---

## §1 · 왜 호스트 서브에이전트가 아니라 네이티브인가

Claude Code 내장 deep-research 는 이미 리서치를 잘한다 — 하지만 그것은 *호스트 서브에이전트* 로 실행된다. 작업은 Claude 의 것이고, MUR 은 라벨만 붙인다. 네이티브 오케스트레이션이야말로 결과를 *MUR 의 것* 으로, 그리고 증명 가능하게 만든다.

- **MUR 오케스트레이션이 소유한다.** 라우터의 반복별 동적 DAG(`cmd/fleet/plan.rs`)로 구동되는 MUR 에이전트 플릿이 실제로 리서치를 수행하는 주체다 — 호스트가 spawn 하고 잊어버린 서브에이전트가 아니다.
- **암호학적 프로비넌스.** 모든 주장은 그것을 만들어낸 워커에 의해 Ed25519 서명된 채널에 기록된다(Unified Channel v3d-2, *peer-writes-own*). "이 에이전트가 이것을 찾았다" 가 캡션이 아니라 검증 가능해진다.
- **플랫폼 통합.** 실측 per-token 예산, 킬 스위치, Commander 거버넌스, 스케줄링, 장기 메모리 — 모두 기존 플릿/에이전트 기구에 무료로 올라탄다.

**비목표:** 내장보다 더 병렬화하는 것. 동시성은 양쪽 다 상한이 있다(약 `min(16, cores−2)`). 네이티브가 이기는 지점은 소유권·프로비넌스·거버넌스이지, 순수 속도가 아니다.

---

## §2 · 아키텍처 — 세 계층, 하나의 초크 포인트

워커는 egress 를 **가지지 않으며** 유일한 주입 표면이다. 결정론적이고 LLM 이 없는 게이트웨이만이 웹에 도달한다 — 그것도 강제된 커널 샌드박스 안에서.

```
┌─────────────────────── fleet "deep-research" ───────────────────────┐
│  router (mur)  — 반복별 동적 DAG (plan.rs)                          │
│  done_when: marker:RESEARCH_COMPLETE · budget-usd · deadline · kill  │
└──────────────────────────────┬──────────────────────────────────────┘
             │ channel/delegate（Ed25519 서명된 응답）
             ▼
┌──────────────── worker × k （entitlements: restricted） ────────────┐
│  model_ref → 실제 LLM · MCP 1 개 마운트: research-gateway          │
│  도구: search · fetch      내장 도구는 전부 deny                    │
└──────────────────────────────┬──────────────────────────────────────┘
             │ MCP: search(query) / fetch(url)   — 읽기 전용 동사
             ▼
┌────────── research-gateway（Rust, LLM 없음, broad-audited egress）──┐
│  SSRF guard · tier ladder 1→2→3 · content budget · 호출별 audit      │
└──────────────────────────────────────────────────────────────────────┘
       강제 대상: B1 커널 샌드박스 + 루프백 egress 프록시
```

프롬프트 인젝션된 워커가 할 수 있는 것은 기껏해야 게이트웨이에 URL 을 `fetch` 시키는 것뿐이다 — 로그로 남고, SSRF 로 차단되며, 임의 호스트에 임의 데이터를 POST 할 수 없다. API 는 `fetch` 이지 "소켓을 연다" 가 아니다.

---

## §3 · 동적 흐름 — decompose → research → verify → synthesize

라우터는 `mur fleet run --loop` 의 매 반복마다 새 DAG 를 방출한다. 하위 질문 수는 decompose 시점에 정해진다. 수렴 마커가 자기 한 줄에 나타날 때까지 루프가 돈다.

1. **Decompose** — 라우터가 질문을 하위 질문(100+ 가능)으로 분해해 작업 큐로 채널에 쓴다.
2. **Research(×N 반복)** — 라우터가 배치를 워커에 할당한다(`max_concurrency` 로 제한). 각 워커는 `search()`·`fetch()` 하고, **각각 URL + 뒷받침 인용에 묶인 주장** 을 추출해 서명된 응답으로 되쓴다. 큐가 소진될 때까지 반복한다.
3. **Verify(3 표 적대적)** — 각 주장을 서로 다른 반증 렌즈(정확성 / 소스 독립성 / 최신성)를 가진 세 워커에 배분한다. 2/3 확인이면 주장은 살아남고, 아니면 폐기(페일세이프 = 폐기).
4. **Synthesize → 수렴** — 라우터가 확인된 주장을 인용 보고서로 접고, 자기 한 줄에 `RESEARCH_COMPLETE` 를 방출한다. 구조화된 `done_when: marker:…` 가 결정론적으로 수렴한다 — 추가 LLM 호출이 없고, 마커를 인용만 한 산문으로는 거짓 수렴이 불가하다.

**무료 상속:** *실측* per-token 회계를 동반한 `--budget-usd` · `mur fleet stop` 킬 스위치 · Commander kill/budget 훅(fail-closed) · 주장별 서명 채널 프로비넌스 · 반복 상한 / 데드라인 / 정체 감지 가드.

---

## §4 · research-gateway

MUR 에 동봉되는, 의존성이 가벼운 작은 Rust MCP 서버. 두 개의 읽기 전용 동사, 고정되고 코드로 구동되는 tier ladder(tier 를 LLM 이 정하지 않음), egress 의 1 바이트까지 통치된다.

```
search(query, limit?)  →  [{title, url, snippet}]
fetch(url, render?)    →  {url, status, title, text, tier}
```

### 에스컬레이션 사다리(스킬이 아니라 결정론적 코드)

| Tier | 엔진 | 비고 |
|------|------|------|
| **1 · http** | `reqwest` GET | 기본이자 가장 저렴. **Search 도 여기를 탄다:** DuckDuckGo 의 서버 렌더링 HTML 엔드포인트를 `fetch` 와 같은 프록시 존중 경로로 GET 한다(브라우저형 User-Agent 필요, 없으면 DDG 는 HTTP 202 반환). 그래서 브라우저를 spawn 할 수 없는 샌드박스에서도 search 가 동작한다. |
| **2 · lightpanda** | `agent-browser --engine lightpanda` | JS 렌더링 페이지. `--args ""` 는 필수 — Chrome stealth 플래그는 Lightpanda 를 망가뜨린다. fetch 마다 고유 `--session` id 로 동시 fetch 가 쿠키 jar 를 공유하지 않게 한다. |
| **3 · chrome** | `agent-browser --engine chrome` | 안티봇 / 스크린샷. stealth 플래그는 단일 `--args "<콤마 구분>"` 값으로 전달한다(맨 argv 는 서브커맨드로 해석됨). 렌더링 `fetch` 는 `Http` 실패 *또는* 빈 렌더링 시 lightpanda → chrome 으로 에스컬레이트한다. `chrome:true` 는 tier 3 를 강제한다. |

- **SSRF 가드(강제·설정 불가)** — 해석된 IP 가 private / link-local / loopback / unique-local 인 URL 을 거부하고, 모든 tier 에서 스크리닝한다. 브라우저 tier 에서는 가드와 `deny_hosts` 를 **spawn 전 게이트웨이 코드에서** 강제한다 — 프록시는 브라우저 자식 프로세스 자신의 연결을 볼 수 없다.
- **콘텐츠 예산** — 단일 5 MB 페이지는 워커 컨텍스트를 넘치게 한다. `fetch` 는 반환 텍스트를 `max_fetch_chars`(기본 50 000, `0` 이면 비활성)로 코드포인트 경계에서 자르고 마커를 붙인다. 5 MB 바디 상한은 전송/메모리를, `max_fetch_chars` 는 컨텍스트를 제한한다. search 스니펫은 자르지 않는다.
- **검색 신뢰성** — N 동시 워커에서 DDG 는 202 챌린지로 레이트 리밋한다. `search` 는 202 시 지수 백오프 + **쿼리 기반 지터** 로 재시도한다 — 서로 다른 하위 질문이 재시도를 어긋나게 하여 동기적으로 다시 폭주하지 않는다.
- **URL 수준 감사** — 각 호출은 `{worker, url, tier, outcome}` 을 채널과 텔레메트리에 기록한다. 보고서의 각 인용은 하나의 게이트웨이 감사 레코드에 대응된다.

---

## §5 · 보안 모델 — 샌드박스, 동의, 도구 정책

게이트웨이는 강제된 커널 샌드박스 아래에서 기본 egress 없이 동작한다. 접근은 정확히 하나의 명시적 동의 단계로 부여되며, 워커의 도구 정책은 헤드리스 턴이 탈출도 정체도 하지 않도록 형성된다.

| 제어 | 메커니즘 | 경계 |
|------|----------|------|
| **Egress 부여** | 워커별 `mcp set-network research-gateway --broad-audited` — 한 번의 오퍼레이터 동의, `EgressAuthorization` 으로 기록. | 플릿 생성이 egress 를 암묵적으로 여는 일은 **결코 없다**. "리서치 플릿이니까" 는 동의가 아니다. |
| **Egress 프록시** | 하나의 루프백 CONNECT 프록시. 각 워커의 게이트웨이 자식은 자신의 allow/deny 정책으로 스코프된 `HTTPS_PROXY` 토큰을 받는다. | 샌드박스가 봉인되기 **전** 에 기동하고, 그 포트를 루프백 전용 규칙으로 프로파일에 새겨야 한다 — 아니면 자식이 다이얼할 수 없다. |
| `mcp__research-gateway__*` | **allow** | 사전 승인되어 헤드리스 플릿 턴이 HITL 게이트(응답자 없음 → 300 초 타임아웃 → 실패)를 우회한다. 그 자체로는 egress 를 부여하지 않는다. |
| `bash` · `read_file` · `write_file` · `edit_file` | **deny** | 리서치 턴이 내장 도구로 손을 뻗으면 같은 응답 불가 게이트에서 막힌다. deny → 광고되지 않음 → 결코 호출되지 않음. |
| 그 외 전부 | **ask** | 위 두 규칙 밖의 도구는 fail-closed 기본을 유지한다. |
| **프로비넌스** | 각 워커는 자신의 응답을 `Agent{self}` 이벤트로 기록하고 Ed25519 서명(v3d-2 peer-writes-own)하며, fold 시 액터별로 검증한다. | 라우터는 더 이상 워커를 대신해 서명하지 않는다 — 귀속은 워커 자신의 키다. |
| **내보내기 안전성** | `.fleet` 임포트는 broad-audited 를 `inherit` 로 강등하고 승인을 지운다. | 공유된 deep-research 플릿은 로컬에서 재부여하기 전까지 egress 가 0 이다. |

**어드바이저리 강제의 정직함.** Tier 1 은 프록시를 존중한다. tier 2/3 의 브라우저 자식 프로세스는 그러지 않을 수 있다 — 전역 게이트웨이 URL 감사로 완화하고, 과장 없이 문서화한다. 완전한 봉쇄 = 향후 Phase-3 sbpl pin-to-proxy 이며, 그때 핀 고정하는 것은 바로 이 하나의 게이트웨이뿐이다.

---

## §6 · 운영 — provision, grant, run

```bash
# 1 · k 개의 restricted 워커를 만들고, 각각 게이트웨이를 마운트하고,
#     같은 동의 단계에서 broad-audited egress 를 부여한다
mur deep-research provision --count 4 --model claude_haiku --grant-egress --yes
#   tool policy: mcp__research-gateway__* → allow
#   tool policy: bash, read_file, write_file, edit_file → deny
#   Updated egress policy for 'research-gateway'.   # × 각 워커

# 2 · 플릿 생성(router = mur), done 마커 + 예산 설정
mur fleet create deep-research \
    --members dr_worker_1,dr_worker_2,dr_worker_3,dr_worker_4 \
    --goal "…당신의 리서치 질문…"
#   fleet.yaml loop: { max_iterations, budget_usd, done_when: marker:RESEARCH_COMPLETE }

# 3 · 가드된 루프 실행 — decompose → research → verify → synthesize
mur deep-research run deep-research --max-iterations 4 --deadline 30m
#   fleet 'deep-research' loop stopped after 1 iteration (~$6.14 spent): Converged
```

- **결과가 있는 곳** — 각 워커의 인용 응답은 `~/.mur/channels/fleet-deep-research/events.jsonl` 의 서명된 이벤트다. `~/.mur/index/channels/` 의 SQLite read-model 은 재구축 가능한 프로젝션이다. 각 인용은 하나의 게이트웨이 감사 레코드에 대응된다.
- **설정 노브(하드코딩 값 없음)** — `MUR_RESEARCH_MAX_FETCH_CHARS`, `…_SEARCH_LIMIT`, `…_TIMEOUT_SECS`, `…_LIGHTPANDA_PATH`, `…_DENY_HOSTS` — env 또는 `~/.mur/config.yaml` 의 `research_gateway:`, 결코 리터럴을 쓰지 않는다.

---

## §7 · 구현의 현실 — 깨끗한 설계를 실현하는 데 든 대가

명세의 4 박스 아키텍처는 옳았다. 그러나 플릿을 실제로 수렴시키는 것 — 샌드박스화·헤드리스·암호학적 귀속 — 은 코드를 읽어서가 아니라 라이브 오퍼레이터 검증으로 발견된 9 개의 독립된 수정을 드러냈다.

| PR | 영역 | 수정 |
|----|------|------|
| **#663** | feat | **Native deep-research 코어** — 게이트웨이 크레이트, 플릿 배선, router/worker/verify 스킬. |
| **#664** | HITL | **게이트웨이 도구 사전 승인** — 헤드리스 턴에는 `tool/approval_needed` 에 답할 자가 없어 → 300 초 타임아웃 → 실패. provision 시 `mcp__research-gateway__* → allow` 를 새긴다. |
| **#665** | G1 · sandbox | **egress 프록시를 봉인 전에 기동** — 프록시가 샌드박스 봉인 *후* 에, 결코 새겨지지 않은 포트에서 기동 → 모든 스코프 부여가 도착 즉시 사망. 봉인 전에 기동하고 루프백 전용 포트 규칙을 새긴다. |
| **#666** | G3 · channel | **channel read-model 디렉터리 부여** — 서명된 응답이 `events.jsonl` 에 착지했으나 SQLite 갱신이 읽기 전용 DB 에 부딪힘 → 거짓 실패. `index/channels` 를 부여하고, 추가 후 갱신을 비치명으로. |
| **#667** | G2 · search | **브라우저 없는 검색 + 동작하는 chrome** — search 가 샌드박스가 금지한 `agent-browser` 를 spawn 했다. tier-1 HTTP 로 우회. chrome `--args` 전달을 고쳐 렌더 폴백이 실제로 기동하게. |
| **#668** | egress | **`Proxy-Authorization` 대소문자 무시** — *egress 전체 잠금 해제*. 프록시가 auth 헤더를 대소문자 구분으로 대조했으나 hyper 는 소문자로 보냄 → 토큰 유실 → *모든* CONNECT 거부. `nc` 프록시 트레이스로 라이브 포착. |
| **#669** | content | **Fetch 콘텐츠 예산** — 단일 5 MB 페이지가 워커 컨텍스트를 넘치게 함(`anthropic 400: prompt too long`). 반환 텍스트를 `max_fetch_chars` 로 자른다. |
| **#670** | convergence | **워커 내장 도구 deny** — *수렴 잠금 해제*. 리서치 턴이 `bash`(기본 `ask`)로 손을 뻗음 → 응답 불가 HITL 게이트 → 턴 실패 → 빈 응답 → 스텝 실패. 내장을 deny 하면 모델은 그것들을 보지도 못한다. raw `channel/delegate` 소켓 프로브로 근본 원인 규명. |
| **#671** | reliability | **DDG 202 재시도 + 지터** — 동시 워커가 DuckDuckGo 레이트 리밋을 밟음. 202 를 백오프 + 쿼리 기반 지터로 재시도. 보고서가 "접근 제한" 에서 각각 16–19 인용으로. |
| **#672** | G4 · cleanup | **로드 시 비스킬 디렉터리 건너뛰기** — `skills/` 아래 플릿 실행 원장(`fleet:<name>/`)이 부팅마다 약 14 개의 이름 검증 경고를 뿌렸다. `load_all` 은 매니페스트 없는 디렉터리를 건너뛴다 — 주입 경로에서만, 성숙도 스윕은 건드리지 않는다. |

**엔드투엔드 검증:** 신규 provision → `run`: 스텝 실패 0, 모든 워커가 **서명된** 응답 기록(Ed25519 프로비넌스), 루프가 **Converged** 로 정지, 라우터가 인용이 달린 Ollama / LM Studio / LocalAI 비교를 생성 — 4 워커 동시성 하에서, 완전히 커널 샌드박스 안에서.

---

*이 설계 문서는 v2.45.0 시점의 출시 구현을 반영한다. "완전한" egress 봉쇄(sbpl pin-to-proxy)와 일급 검색 API 는 의도된 Phase-3 후속으로 남는다.*
