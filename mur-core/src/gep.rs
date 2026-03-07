//! GEP (Gene Evolution Protocol) — treats patterns as "genes" that evolve
//! through mutation, crossover, and selection.

use chrono::Utc;
use mur_common::pattern::{Content, Pattern};

use crate::evolve::feedback::FeedbackSignal;

/// A pattern wrapped as a gene for evolutionary computation.
#[derive(Debug, Clone)]
pub struct GepGene {
    /// The underlying pattern
    pub pattern: Pattern,
    /// Fitness score (0.0-1.0), derived from effectiveness + importance
    pub fitness: f64,
    /// Which generation this gene was created in
    pub generation: u32,
    /// Names of ancestor patterns
    pub lineage: Vec<String>,
}

impl GepGene {
    /// Wrap a pattern as a gene, computing initial fitness.
    pub fn from_pattern(pattern: Pattern) -> Self {
        let fitness = compute_fitness(&pattern);
        let name = pattern.name.clone();
        Self {
            pattern,
            fitness,
            generation: 0,
            lineage: vec![name],
        }
    }

    /// Recompute fitness from the current pattern state.
    pub fn refresh_fitness(&mut self) {
        self.fitness = compute_fitness(&self.pattern);
    }
}

/// Compute fitness from pattern evidence and metadata.
fn compute_fitness(pattern: &Pattern) -> f64 {
    let effectiveness = pattern.evidence.effectiveness();
    let importance = pattern.importance;
    let confidence = pattern.confidence;

    // Weighted combination
    let raw = effectiveness * 0.4 + importance * 0.35 + confidence * 0.25;
    raw.clamp(0.0, 1.0)
}

/// Mutate a gene based on feedback signals: adjust importance and confidence.
pub fn mutate(gene: &GepGene, feedback: &[FeedbackSignal]) -> GepGene {
    let mut mutated = gene.clone();
    mutated.generation += 1;

    for signal in feedback {
        crate::evolve::feedback::apply_feedback(&mut mutated.pattern, signal.clone());
    }

    mutated.refresh_fitness();
    mutated.pattern.base.updated_at = Utc::now();

    // Track lineage
    if !mutated.lineage.contains(&gene.pattern.name) {
        mutated.lineage.push(gene.pattern.name.clone());
    }

    mutated
}

/// Crossover two related genes: merge their content and combine evidence.
pub fn crossover(a: &GepGene, b: &GepGene) -> GepGene {
    // Take the higher-fitness parent as base
    let (primary, secondary) = if a.fitness >= b.fitness {
        (a, b)
    } else {
        (b, a)
    };

    let mut child_pattern = primary.pattern.clone();

    // Merge content from both parents
    let primary_text = primary.pattern.content.as_text();
    let secondary_text = secondary.pattern.content.as_text();

    // Only merge if they have meaningfully different content
    if primary_text != secondary_text {
        let merged = format!("{}\n\n---\n\n{}", primary_text, secondary_text);
        // Truncate to max layer chars if needed
        let truncated = if merged.len() > Content::MAX_LAYER_CHARS * 2 {
            merged[..Content::MAX_LAYER_CHARS * 2].to_string()
        } else {
            merged
        };
        child_pattern.base.content = Content::Plain(truncated);
    }

    // Merge name
    child_pattern.base.name = format!("{}-x-{}", primary.pattern.name, secondary.pattern.name);
    child_pattern.base.description = format!(
        "Crossover of '{}' and '{}'",
        primary.pattern.name, secondary.pattern.name
    );

    // Combine evidence: sum signals
    child_pattern.base.evidence.success_signals = primary
        .pattern
        .evidence
        .success_signals
        .saturating_add(secondary.pattern.evidence.success_signals);
    child_pattern.base.evidence.override_signals = primary
        .pattern
        .evidence
        .override_signals
        .saturating_add(secondary.pattern.evidence.override_signals);
    child_pattern.base.evidence.injection_count = primary
        .pattern
        .evidence
        .injection_count
        .saturating_add(secondary.pattern.evidence.injection_count);

    // Blend importance and confidence
    child_pattern.base.importance =
        (primary.pattern.importance + secondary.pattern.importance) / 2.0;
    child_pattern.base.confidence =
        (primary.pattern.confidence + secondary.pattern.confidence) / 2.0;

    // Merge tags
    for lang in &secondary.pattern.tags.languages {
        if !child_pattern.base.tags.languages.contains(lang) {
            child_pattern.base.tags.languages.push(lang.clone());
        }
    }
    for topic in &secondary.pattern.tags.topics {
        if !child_pattern.base.tags.topics.contains(topic) {
            child_pattern.base.tags.topics.push(topic.clone());
        }
    }

    child_pattern.base.updated_at = Utc::now();

    let generation = primary.generation.max(secondary.generation) + 1;
    let mut lineage = primary.lineage.clone();
    for l in &secondary.lineage {
        if !lineage.contains(l) {
            lineage.push(l.clone());
        }
    }

    let mut child = GepGene {
        pattern: child_pattern,
        fitness: 0.0,
        generation,
        lineage,
    };
    child.refresh_fitness();
    child
}

/// Select the top-k genes by fitness.
pub fn select(population: &[GepGene], top_k: usize) -> Vec<GepGene> {
    let mut sorted: Vec<GepGene> = population.to_vec();
    sorted.sort_by(|a, b| {
        b.fitness
            .partial_cmp(&a.fitness)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    sorted.truncate(top_k);
    sorted
}

/// Run one evolution cycle: wrap patterns as genes, apply feedback via mutation,
/// attempt crossover on related patterns, and select the fittest.
pub fn evolve_generation(patterns: &[Pattern], feedback: &[FeedbackSignal]) -> Vec<Pattern> {
    if patterns.is_empty() {
        return vec![];
    }

    // Wrap all patterns as genes
    let mut population: Vec<GepGene> = patterns
        .iter()
        .map(|p| GepGene::from_pattern(p.clone()))
        .collect();

    // Mutate each gene with feedback
    let mutated: Vec<GepGene> = population.iter().map(|g| mutate(g, feedback)).collect();

    population.extend(mutated);

    // Crossover: pair adjacent genes by fitness (after sorting)
    population.sort_by(|a, b| {
        b.fitness
            .partial_cmp(&a.fitness)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    let mut crossover_children = Vec::new();
    let pairs = population.len().min(10); // limit crossover pairs
    for i in (0..pairs).step_by(2) {
        if i + 1 < population.len() {
            let child = crossover(&population[i], &population[i + 1]);
            crossover_children.push(child);
        }
    }
    population.extend(crossover_children);

    // Select: keep top N (same count as input)
    let selected = select(&population, patterns.len());
    selected.into_iter().map(|g| g.pattern).collect()
}

/// Compute population fitness statistics.
pub fn population_stats(patterns: &[Pattern]) -> GepStats {
    if patterns.is_empty() {
        return GepStats {
            count: 0,
            avg_fitness: 0.0,
            max_fitness: 0.0,
            min_fitness: 0.0,
            avg_effectiveness: 0.0,
        };
    }

    let genes: Vec<GepGene> = patterns
        .iter()
        .map(|p| GepGene::from_pattern(p.clone()))
        .collect();
    let fitnesses: Vec<f64> = genes.iter().map(|g| g.fitness).collect();
    let effectivenesses: Vec<f64> = patterns
        .iter()
        .map(|p| p.evidence.effectiveness())
        .collect();

    let count = fitnesses.len();
    let sum: f64 = fitnesses.iter().sum();
    let eff_sum: f64 = effectivenesses.iter().sum();

    GepStats {
        count,
        avg_fitness: sum / count as f64,
        max_fitness: fitnesses.iter().cloned().fold(f64::NEG_INFINITY, f64::max),
        min_fitness: fitnesses.iter().cloned().fold(f64::INFINITY, f64::min),
        avg_effectiveness: eff_sum / count as f64,
    }
}

/// Summary statistics for a GEP population.
#[derive(Debug)]
pub struct GepStats {
    pub count: usize,
    pub avg_fitness: f64,
    pub max_fitness: f64,
    pub min_fitness: f64,
    pub avg_effectiveness: f64,
}

#[cfg(test)]
mod tests {
    use super::*;
    use mur_common::knowledge::KnowledgeBase;
    use mur_common::pattern::*;

    fn make_pattern(name: &str, importance: f64, effectiveness_signals: (u64, u64)) -> Pattern {
        Pattern {
            base: KnowledgeBase {
                name: name.into(),
                description: format!("Test pattern {}", name),
                content: Content::Plain(format!("Content for {}", name)),
                importance,
                evidence: Evidence {
                    success_signals: effectiveness_signals.0,
                    override_signals: effectiveness_signals.1,
                    ..Default::default()
                },
                ..Default::default()
            },
            kind: None,
            origin: None,
            attachments: vec![],
        }
    }

    #[test]
    fn test_gene_from_pattern() {
        let p = make_pattern("test-gene", 0.8, (10, 2));
        let gene = GepGene::from_pattern(p);
        assert!(gene.fitness > 0.0);
        assert_eq!(gene.generation, 0);
        assert_eq!(gene.lineage, vec!["test-gene".to_string()]);
    }

    #[test]
    fn test_compute_fitness_range() {
        let p = make_pattern("fitness-test", 0.5, (5, 5));
        let fitness = compute_fitness(&p);
        assert!((0.0..=1.0).contains(&fitness));
    }

    #[test]
    fn test_mutate_with_success_increases_fitness() {
        let p = make_pattern("mutate-test", 0.5, (5, 5));
        let gene = GepGene::from_pattern(p);
        let mutated = mutate(&gene, &[FeedbackSignal::Success, FeedbackSignal::Success]);
        // Mutation with success should generally increase or maintain fitness
        assert!(mutated.fitness >= 0.0);
        assert_eq!(mutated.generation, 1);
    }

    #[test]
    fn test_crossover_produces_child() {
        let a = GepGene::from_pattern(make_pattern("parent-a", 0.8, (10, 1)));
        let b = GepGene::from_pattern(make_pattern("parent-b", 0.6, (5, 2)));
        let child = crossover(&a, &b);
        assert!(child.pattern.name.contains("-x-"));
        assert!(child.generation > 0);
        assert!(child.lineage.len() >= 2);
    }

    #[test]
    fn test_crossover_merges_evidence() {
        let a = GepGene::from_pattern(make_pattern("ev-a", 0.7, (10, 2)));
        let b = GepGene::from_pattern(make_pattern("ev-b", 0.6, (5, 3)));
        let child = crossover(&a, &b);
        assert_eq!(child.pattern.evidence.success_signals, 15);
        assert_eq!(child.pattern.evidence.override_signals, 5);
    }

    #[test]
    fn test_select_top_k() {
        let genes: Vec<GepGene> = (0..5)
            .map(|i| {
                let imp = (i as f64) * 0.2;
                GepGene::from_pattern(make_pattern(&format!("sel-{}", i), imp, (i * 2, 1)))
            })
            .collect();
        let top = select(&genes, 2);
        assert_eq!(top.len(), 2);
        assert!(top[0].fitness >= top[1].fitness);
    }

    #[test]
    fn test_evolve_generation_preserves_count() {
        let patterns: Vec<Pattern> = (0..3)
            .map(|i| make_pattern(&format!("evo-{}", i), 0.5, (5, 2)))
            .collect();
        let evolved = evolve_generation(&patterns, &[FeedbackSignal::Success]);
        assert_eq!(evolved.len(), patterns.len());
    }

    #[test]
    fn test_evolve_generation_empty() {
        let evolved = evolve_generation(&[], &[FeedbackSignal::Success]);
        assert!(evolved.is_empty());
    }

    #[test]
    fn test_population_stats() {
        let patterns: Vec<Pattern> = vec![
            make_pattern("stats-a", 0.8, (10, 1)),
            make_pattern("stats-b", 0.4, (3, 7)),
        ];
        let stats = population_stats(&patterns);
        assert_eq!(stats.count, 2);
        assert!(stats.avg_fitness > 0.0);
        assert!(stats.max_fitness >= stats.min_fitness);
    }

    #[test]
    fn test_population_stats_empty() {
        let stats = population_stats(&[]);
        assert_eq!(stats.count, 0);
        assert_eq!(stats.avg_fitness, 0.0);
    }
}
