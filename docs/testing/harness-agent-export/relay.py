#!/usr/bin/env python3
"""PM -> Rust -> QA relay over real A2A (`mur agent send`), threading each
role's artifact into the next. Writes transcript incrementally so partial
progress survives interruption. Deliverable: the `mur agent mood` feature."""
import json, subprocess, os, time, datetime, re

MUR = "/Volumes/Firecuda4tb/Projects/mur/target/release/mur"
ENV = {**os.environ, "MUR_HOME": "/tmp/mur-export-test"}
OUT = "/Volumes/Firecuda4tb/Projects/mur/docs/testing/harness-agent-export/relay-transcript.md"
PIPELINE = [("xx-pm","Product Manager"),
            ("xx-rust","Rust Engineer"),
            ("xx-qa","QA / Release Engineer")]

def send(agent, text):
    msg = json.dumps({"role":"user","parts":[{"kind":"text","text":text}]})
    t0 = time.time()
    p = subprocess.run([MUR,"agent","send",agent,msg], env=ENV,
                       capture_output=True, text=True, timeout=300)
    dt = time.time()-t0
    if p.returncode != 0:
        return f"[SEND ERROR exit={p.returncode}] {p.stderr.strip()[:300]}", dt
    try:
        return json.loads(p.stdout)["messages"][-1]["parts"][0]["text"], dt
    except Exception as e:
        return f"[PARSE ERROR {e}] {p.stdout[:300]}", dt

def main():
    kickoff = ("GO. Design the `mur agent mood` feature: a playful per-agent mood "
               "(a short phrase + an emoji) shown in `mur agent list` and the agent "
               "card. Produce YOUR role's artifact now, concisely.")
    artifact = kickoff
    with open(OUT,"w") as f:
        f.write(f"# Export-Team Relay Transcript (PM -> Rust -> QA)\n\n_Started "
                f"{datetime.datetime.now(datetime.timezone.utc).isoformat()} — "
                f"feature: mur agent mood — real Claude via cc-proxy bridge_\n")
    summary = []
    for i,(agent,role) in enumerate(PIPELINE):
        prev = "" if i==0 else f"\n\n--- PRIOR ARTIFACT (from {PIPELINE[i-1][1]}) ---\n{artifact}"
        reply,dt = send(agent, kickoff if i==0 else
                        f"Continue the relay. Do YOUR role ({role}) on the work so far.{prev}")
        handoff = bool(re.search(r"HANDOFF\s*->", reply))
        with open(OUT,"a") as f:
            f.write(f"\n## {i+1}. {role} (`{agent}`)  —  {dt:.1f}s\n\n{reply.strip()}\n")
        summary.append((role,len(reply),handoff,dt))
        print(f"[{i+1}/3] {role}: {len(reply)} chars, {dt:.1f}s, handoff={'Y' if handoff else 'N'}", flush=True)
        artifact = reply
    with open(OUT,"a") as f:
        f.write("\n---\n## Handoff-quality summary\n\n| # | Role | chars | handoff? | latency |\n|---|------|-------|----------|---------|\n")
        for i,(role,n,h,dt) in enumerate(summary):
            f.write(f"| {i+1} | {role} | {n} | {'YES' if h else 'NO'} | {dt:.1f}s |\n")
    print(f"RELAY_DONE handoff_lines={sum(1 for s in summary if s[2])}/3")

if __name__ == "__main__":
    main()
