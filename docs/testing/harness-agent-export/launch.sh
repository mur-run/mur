#!/usr/bin/env bash
# Launch the xx-* team with bridge creds so `mur agent send` reaches real
# Claude via the cc-proxy (ANTHROPIC_BASE_URL/ANTHROPIC_API_KEY from ~/.zshrc).
# Run via Bash run_in_background so `wait` keeps the children alive.
set -u
export MUR_HOME=/tmp/mur-export-test
source ~/.zshrc 2>/dev/null   # bridge creds (key never printed)
RT=/Volumes/Firecuda4tb/Projects/mur/target/release/mur-agent-runtime
LOG="$MUR_HOME/runlogs"; mkdir -p "$LOG"
for a in xx-pm xx-rust xx-qa; do
  MUR_HOME=$MUR_HOME "$RT" --profile "$a" >"$LOG/$a.out" 2>&1 &
  echo "started $a pid=$!"
done
wait
