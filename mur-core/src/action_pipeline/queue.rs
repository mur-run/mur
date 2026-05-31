use anyhow::Result;
use chrono::Utc;
use mur_common::action::{
    Action, ActionEvent, Task, TaskOutcome, TaskState, TaskStep,
};
use std::collections::HashMap;
use uuid::Uuid;

use super::ledger::ActionLedger;
use super::{Pipeline, PipelineError};

pub struct TaskQueue {
    pipeline: Pipeline,
    tasks: HashMap<Uuid, Task>,
    ledger: ActionLedger,
}

impl TaskQueue {
    pub fn new(pipeline: &Pipeline) -> Result<Self> {
        let ledger = ActionLedger::open(&pipeline.ledger_dir())?;

        // Rebuild state from ledger
        let events = ActionLedger::replay_days(&pipeline.ledger_dir(), 7);
        let mut tasks: HashMap<Uuid, Task> = HashMap::new();
        for event in &events {
            match event {
                ActionEvent::TaskEnqueued { task } => {
                    tasks.insert(task.id, task.clone());
                }
                ActionEvent::TaskStarted { task_id } => {
                    if let Some(t) = tasks.get_mut(task_id) {
                        t.state = TaskState::Running;
                        t.started_at = Some(Utc::now());
                    }
                }
                ActionEvent::TaskPaused { task_id, .. } => {
                    if let Some(t) = tasks.get_mut(task_id) {
                        t.state = TaskState::Paused;
                    }
                }
                ActionEvent::TaskResumed { task_id } => {
                    if let Some(t) = tasks.get_mut(task_id) {
                        t.state = TaskState::Running;
                    }
                }
                ActionEvent::TaskCompleted { task_id, outcome } => {
                    if let Some(t) = tasks.get_mut(task_id) {
                        t.state = TaskState::Completed {
                            outcome: outcome.clone(),
                        };
                        t.completed_at = Some(Utc::now());
                    }
                }
                ActionEvent::TaskCancelled { task_id } => {
                    if let Some(t) = tasks.get_mut(task_id) {
                        t.state = TaskState::Cancelled;
                        t.completed_at = Some(Utc::now());
                    }
                }
                ActionEvent::TaskStepUpdated { task_id, step } => {
                    if let Some(t) = tasks.get_mut(task_id) {
                        if let Some(existing) = t.steps.iter_mut().find(|s| s.index == step.index) {
                            *existing = step.clone();
                        } else {
                            t.steps.push(step.clone());
                        }
                    }
                }
                _ => {}
            }
        }

        Ok(Self {
            pipeline: pipeline.clone(),
            tasks,
            ledger,
        })
    }

    /// Enqueue a new task. Returns `QueueFull` if at max_concurrent.
    pub fn enqueue(
        &mut self,
        pending_item_id: Uuid,
        action: Action,
        timeout_seconds: u32,
        initial_steps: Vec<TaskStep>,
    ) -> Result<Task, PipelineError> {
        let running_count = self
            .tasks
            .values()
            .filter(|t| matches!(t.state, TaskState::Running))
            .count();
        if running_count >= self.pipeline.config.queue.max_concurrent as usize {
            return Err(PipelineError::QueueFull {
                current: running_count,
                max: self.pipeline.config.queue.max_concurrent,
            });
        }

        let task = Task {
            id: Uuid::now_v7(),
            pending_item_id,
            action,
            state: TaskState::Queued,
            steps: initial_steps,
            created_at: Utc::now(),
            started_at: None,
            completed_at: None,
            timeout_seconds,
        };

        self.tasks.insert(task.id, task.clone());
        self.ledger
            .append(&ActionEvent::TaskEnqueued { task: task.clone() })?;
        Ok(task)
    }

    /// Transition a task from Queued → Running.
    pub fn start_task(&mut self, task_id: Uuid) -> Result<()> {
        let task = self
            .tasks
            .get_mut(&task_id)
            .ok_or(PipelineError::TaskNotFound {
                task_id: task_id.to_string(),
            })?;
        if !matches!(task.state, TaskState::Queued) {
            return Err(anyhow::anyhow!("task {task_id} is not queued"));
        }
        task.state = TaskState::Running;
        task.started_at = Some(Utc::now());
        self.ledger
            .append(&ActionEvent::TaskStarted { task_id })?;
        Ok(())
    }

    /// Transition Running → Paused.
    pub fn pause_task(&mut self, task_id: Uuid, reason: String) -> Result<()> {
        let task = self.tasks.get_mut(&task_id).ok_or(PipelineError::TaskNotFound {
            task_id: task_id.to_string(),
        })?;
        task.state = TaskState::Paused;
        self.ledger.append(&ActionEvent::TaskPaused {
            task_id,
            reason,
        })?;
        Ok(())
    }

    /// Transition Paused → Running.
    pub fn resume_task(&mut self, task_id: Uuid) -> Result<()> {
        let task = self.tasks.get_mut(&task_id).ok_or(PipelineError::TaskNotFound {
            task_id: task_id.to_string(),
        })?;
        task.state = TaskState::Running;
        self.ledger
            .append(&ActionEvent::TaskResumed { task_id })?;
        Ok(())
    }

    /// Transition Running → Completed.
    pub fn complete_task(&mut self, task_id: Uuid, outcome: TaskOutcome) -> Result<()> {
        let task = self.tasks.get_mut(&task_id).ok_or(PipelineError::TaskNotFound {
            task_id: task_id.to_string(),
        })?;
        task.state = TaskState::Completed {
            outcome: outcome.clone(),
        };
        task.completed_at = Some(Utc::now());
        self.ledger.append(&ActionEvent::TaskCompleted {
            task_id,
            outcome,
        })?;
        Ok(())
    }

    /// Transition Running | Queued → Cancelled.
    pub fn cancel_task(&mut self, task_id: Uuid) -> Result<()> {
        let task = self.tasks.get_mut(&task_id).ok_or(PipelineError::TaskNotFound {
            task_id: task_id.to_string(),
        })?;
        task.state = TaskState::Cancelled;
        task.completed_at = Some(Utc::now());
        self.ledger
            .append(&ActionEvent::TaskCancelled { task_id })?;
        Ok(())
    }

    /// Update a step on a running task.
    pub fn update_step(&mut self, task_id: Uuid, step: TaskStep) -> Result<()> {
        let task = self.tasks.get_mut(&task_id).ok_or(PipelineError::TaskNotFound {
            task_id: task_id.to_string(),
        })?;
        if let Some(existing) = task.steps.iter_mut().find(|s| s.index == step.index) {
            *existing = step.clone();
        } else {
            task.steps.push(step.clone());
        }
        self.ledger.append(&ActionEvent::TaskStepUpdated {
            task_id,
            step,
        })?;
        Ok(())
    }

    pub fn get(&self, task_id: Uuid) -> Option<&Task> {
        self.tasks.get(&task_id)
    }

    pub fn all_tasks(&self) -> Vec<&Task> {
        self.tasks.values().collect()
    }

    pub fn queued_tasks(&self) -> Vec<&Task> {
        self.tasks
            .values()
            .filter(|t| matches!(t.state, TaskState::Queued))
            .collect()
    }

    pub fn running_tasks(&self) -> Vec<&Task> {
        self.tasks
            .values()
            .filter(|t| matches!(t.state, TaskState::Running))
            .collect()
    }

    pub fn completed_tasks(&self) -> Vec<&Task> {
        self.tasks
            .values()
            .filter(|t| matches!(t.state, TaskState::Completed { .. }))
            .collect()
    }

    pub fn failed_tasks(&self) -> Vec<&Task> {
        self.tasks
            .values()
            .filter(|t| {
                matches!(
                    t.state,
                    TaskState::Completed {
                        outcome: TaskOutcome::Failed { .. }
                    }
                )
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mur_common::action::{Action, ActionPipelineConfig, TaskOutcome};
    use tempfile::TempDir;
    use uuid::Uuid;

    fn test_pipeline() -> (Pipeline, TempDir) {
        let tmp = TempDir::new().unwrap();
        let home = tmp.path().join("agent_home");
        std::fs::create_dir_all(&home).unwrap();
        let pipeline = Pipeline::new(home, ActionPipelineConfig::default());
        (pipeline, tmp)
    }

    #[test]
    fn enqueue_creates_task_in_queued_state() {
        let (pipeline, _tmp) = test_pipeline();
        let mut queue = TaskQueue::new(&pipeline).unwrap();
        let pending_id = Uuid::now_v7();
        let action = Action {
            id: "summarize".into(),
            label: "Summarize".into(),
            user_prompt: None,
        };

        let task = queue.enqueue(pending_id, action, 30, vec![]).unwrap();
        assert_eq!(task.state, TaskState::Queued);
        assert_eq!(task.pending_item_id, pending_id);
    }

    #[test]
    fn enqueue_at_capacity_returns_error() {
        let (mut pipeline, _tmp) = test_pipeline();
        pipeline.config.queue.max_concurrent = 1;

        let mut queue = TaskQueue::new(&pipeline).unwrap();
        let a = Action { id: "a".into(), label: "A".into(), user_prompt: None };

        let t1 = queue.enqueue(Uuid::now_v7(), a.clone(), 30, vec![]).unwrap();
        queue.start_task(t1.id).unwrap();

        // Second task at capacity
        let result = queue.enqueue(Uuid::now_v7(), a, 30, vec![]);
        assert!(result.is_err());
        match result.unwrap_err() {
            PipelineError::QueueFull { .. } => {}
            e => panic!("expected QueueFull, got {e:?}"),
        }
    }

    #[test]
    fn state_machine_transitions() {
        let (pipeline, _tmp) = test_pipeline();
        let mut queue = TaskQueue::new(&pipeline).unwrap();
        let a = Action { id: "t".into(), label: "T".into(), user_prompt: None };
        let task = queue.enqueue(Uuid::now_v7(), a, 30, vec![]).unwrap();
        let task_id = task.id;

        // Queued → Running
        queue.start_task(task_id).unwrap();
        assert_eq!(queue.get(task_id).unwrap().state, TaskState::Running);

        // Running → Paused
        queue.pause_task(task_id, "user request".into()).unwrap();
        assert_eq!(queue.get(task_id).unwrap().state, TaskState::Paused);

        // Paused → Running (resume)
        queue.resume_task(task_id).unwrap();
        assert_eq!(queue.get(task_id).unwrap().state, TaskState::Running);

        // Running → Completed
        let outcome = TaskOutcome::Success { outputs: vec![] };
        queue.complete_task(task_id, outcome.clone()).unwrap();
        assert_eq!(queue.get(task_id).unwrap().state, TaskState::Completed { outcome });
    }

    #[test]
    fn cancel_cleans_up_task() {
        let (pipeline, _tmp) = test_pipeline();
        let mut queue = TaskQueue::new(&pipeline).unwrap();
        let a = Action { id: "c".into(), label: "C".into(), user_prompt: None };
        let task = queue.enqueue(Uuid::now_v7(), a, 30, vec![]).unwrap();

        queue.cancel_task(task.id).unwrap();
        let task = queue.get(task.id).unwrap();
        assert_eq!(task.state, TaskState::Cancelled);
    }

    #[test]
    fn ledger_rebuild_on_crash() {
        let (pipeline, _tmp) = test_pipeline();
        let pending_id = Uuid::now_v7();
        let a = Action { id: "r".into(), label: "R".into(), user_prompt: None };

        {
            let mut queue = TaskQueue::new(&pipeline).unwrap();
            let task = queue.enqueue(pending_id, a.clone(), 30, vec![]).unwrap();
            queue.start_task(task.id).unwrap();
        } // "crash" — drop

        // Rebuild from ledger
        let queue = TaskQueue::new(&pipeline).unwrap();
        let tasks: Vec<_> = queue.all_tasks();
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].action.id, "r");
        assert_eq!(tasks[0].state, TaskState::Running);
    }

    #[test]
    fn step_reporting_updates_task() {
        let (pipeline, _tmp) = test_pipeline();
        let mut queue = TaskQueue::new(&pipeline).unwrap();
        let a = Action { id: "s".into(), label: "S".into(), user_prompt: None };
        let task = queue.enqueue(Uuid::now_v7(), a, 30, vec![]).unwrap();
        queue.start_task(task.id).unwrap();

        let step = TaskStep {
            index: 0,
            label: "Reading file...".into(),
            state: mur_common::action::StepState::Done,
            started_at: Some(Utc::now()),
            completed_at: Some(Utc::now()),
        };
        queue.update_step(task.id, step.clone()).unwrap();

        let task = queue.get(task.id).unwrap();
        assert_eq!(task.steps.len(), 1);
        assert_eq!(task.steps[0].label, "Reading file...");
        assert_eq!(task.steps[0].state, mur_common::action::StepState::Done);
    }
}
