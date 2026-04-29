//! Smoke benchmarks for companion warm paths.  The intent is to keep these
//! compiling on CI (`cargo bench --no-run`); the absolute numbers are advisory.
//! See Spec §8.6 for performance budgets.
//!
//! ## CI integration
//!
//! - PR CI: `cargo bench --no-run -p mur-agent-runtime --bench companion`
//!   (compile-check only — keeps the benches alive without spending budget).
//! - Nightly: full `cargo bench`; alert on > 10× regression vs the previous
//!   nightly baseline.

use chrono::{TimeZone, Utc};
use criterion::{Criterion, black_box, criterion_group, criterion_main};
use mur_agent_runtime::companion::picker::{BanditState, Picker, TemplateState};
use mur_agent_runtime::companion::telemetry::OutboxEvent;
use mur_agent_runtime::companion::voice::{VoiceInput, compose_in_memory};
use mur_agent_runtime::durable::ledger::Ledger;
use mur_common::companion::{Relationship, Situation};
use std::collections::BTreeMap;
use tempfile::TempDir;

fn bench_compose(c: &mut Criterion) {
    c.bench_function("voice/compose_in_memory_warm", |b| {
        b.iter(|| {
            let composed = compose_in_memory(VoiceInput {
                relationship: Relationship::Friend,
                locale: "en-US",
                name_for_user: "user",
                formality: "polite",
                extra_instructions: "",
            });
            black_box(composed);
        });
    });
}

fn build_state() -> BanditState {
    let mut map = BTreeMap::new();
    for id in ["a", "b", "c", "d"] {
        map.insert(
            id.to_string(),
            TemplateState {
                id: id.to_string(),
                situation: Situation::MorningGreeting,
                weight: 1.0,
                last_used_at: None,
                pos_count: 0,
                neg_count: 0,
                dismiss_count: 0,
                cooldown_days: 0,
            },
        );
    }
    BanditState {
        version: 1,
        morning_sent_today: None,
        templates: map,
    }
}

fn bench_picker_pick(c: &mut Criterion) {
    let now = Utc.with_ymd_and_hms(2026, 4, 29, 12, 0, 0).unwrap();
    let mut picker = Picker::with_seed(build_state(), 7);
    c.bench_function("picker/pick", |b| {
        b.iter(|| {
            let pick = picker.pick(Situation::MorningGreeting, now);
            black_box(pick);
        });
    });
}

fn bench_ledger_append(c: &mut Criterion) {
    let tmp = TempDir::new().unwrap();
    let mut ledger = Ledger::open(tmp.path()).unwrap();
    let now = Utc.with_ymd_and_hms(2026, 4, 29, 12, 0, 0).unwrap();
    let event = OutboxEvent::MessageScheduled {
        id: "msg-001".into(),
        situation: Situation::MorningGreeting,
        template_id: "tmpl-a".into(),
        scheduled_for: now,
    };
    c.bench_function("ledger/append", |b| {
        b.iter(|| {
            ledger.append(black_box(&event)).unwrap();
        });
    });
}

criterion_group!(
    benches,
    bench_compose,
    bench_picker_pick,
    bench_ledger_append
);
criterion_main!(benches);
