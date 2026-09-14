# Deep-research runtime regressions fix plan

- [x] RED/GREEN: router plan deterministically includes a final router synthesis step that may emit `RESEARCH_COMPLETE`; cap repeated no-progress plans before another paid iteration.
- [x] RED/GREEN: `/deep-research ask …` strips `ask`; bare command dispatches status; `stop` bypasses the shell slot.
- [x] RED/GREEN: cancellation writes the fleet stop sentinel and visibly reports stopping/stopped while the in-flight iteration drains.
- [x] RED/GREEN: terminal progress is stamped/reconciled when the foreground child exits, including interrupted runs.
- [x] RED/GREEN: progress appears once (shell card owns it; no duplicate transcript Note).
- [x] Run focused tests, then full relevant suites, fmt and diff checks.
