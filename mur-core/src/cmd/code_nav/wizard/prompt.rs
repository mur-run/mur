//! Every question the wizard asks goes through [`Prompt`], so the flow is
//! testable with a script and the terminal UI lives in one place.

use std::io::{BufRead, Write};

use anyhow::{Result, bail};

pub(super) trait Prompt {
    /// Ticked indices. Esc cancels the wizard.
    fn multi_select(
        &mut self,
        prompt: &str,
        items: &[String],
        defaults: &[bool],
    ) -> Result<Vec<usize>>;
    /// One choice. Esc cancels the wizard.
    fn select(&mut self, prompt: &str, items: &[String]) -> Result<usize>;
    /// y/N; defaults to no.
    fn confirm(&mut self, prompt: &str) -> Result<bool>;
    /// Free text, pre-filled with `initial`.
    fn input(&mut self, prompt: &str, initial: &str) -> Result<String>;
    /// The literal `yes` every grant in code-nav setup is approved with.
    fn literal_yes(&mut self, out: &mut dyn Write) -> Result<bool>;
}

/// Thrown when the user presses Esc in a picker.
pub(super) const CANCELLED: &str =
    "code-nav setup --wizard cancelled — nothing was installed, written or granted";

/// dialoguer on the real terminal; the literal `yes` reads `input`.
pub(super) struct Terminal<'a> {
    pub(super) input: &'a mut dyn BufRead,
}

impl Prompt for Terminal<'_> {
    fn multi_select(
        &mut self,
        prompt: &str,
        items: &[String],
        defaults: &[bool],
    ) -> Result<Vec<usize>> {
        match dialoguer::MultiSelect::new()
            .with_prompt(prompt)
            .items(items)
            .defaults(defaults)
            .interact_opt()?
        {
            Some(v) => Ok(v),
            None => bail!(CANCELLED),
        }
    }

    fn select(&mut self, prompt: &str, items: &[String]) -> Result<usize> {
        match dialoguer::Select::new()
            .with_prompt(prompt)
            .items(items)
            .default(0)
            .interact_opt()?
        {
            Some(i) => Ok(i),
            None => bail!(CANCELLED),
        }
    }

    fn confirm(&mut self, prompt: &str) -> Result<bool> {
        Ok(dialoguer::Confirm::new()
            .with_prompt(prompt)
            .default(false)
            .interact()?)
    }

    fn input(&mut self, prompt: &str, initial: &str) -> Result<String> {
        Ok(dialoguer::Input::<String>::new()
            .with_prompt(prompt)
            .with_initial_text(initial)
            .allow_empty(true)
            .interact_text()?)
    }

    fn literal_yes(&mut self, out: &mut dyn Write) -> Result<bool> {
        crate::cmd::consent::literal_yes(self.input, out)
    }
}

/// Scripted answers for tests, consumed in order.
#[cfg(test)]
#[derive(Debug, Default)]
pub(super) struct Script {
    pub(super) answers: std::collections::VecDeque<Answer>,
    /// Every prompt shown, for assertions.
    pub(super) asked: Vec<String>,
}

#[cfg(test)]
#[derive(Debug)]
pub(super) enum Answer {
    Ticks(Vec<usize>),
    /// Keep the defaults.
    Defaults,
    Pick(usize),
    Bool(bool),
    Text(String),
}

#[cfg(test)]
impl Script {
    pub(super) fn new(a: impl IntoIterator<Item = Answer>) -> Self {
        Self {
            answers: a.into_iter().collect(),
            asked: Vec::new(),
        }
    }

    fn next(&mut self, prompt: &str) -> Answer {
        self.asked.push(prompt.to_owned());
        self.answers
            .pop_front()
            .unwrap_or_else(|| panic!("unscripted prompt: {prompt}"))
    }
}

#[cfg(test)]
impl Prompt for Script {
    fn multi_select(&mut self, p: &str, _: &[String], defaults: &[bool]) -> Result<Vec<usize>> {
        match self.next(p) {
            Answer::Ticks(v) => Ok(v),
            Answer::Defaults => Ok((0..defaults.len()).filter(|i| defaults[*i]).collect()),
            a => panic!("{p}: expected ticks, got {a:?}"),
        }
    }
    fn select(&mut self, p: &str, _: &[String]) -> Result<usize> {
        match self.next(p) {
            Answer::Pick(i) => Ok(i),
            a => panic!("{p}: expected a pick, got {a:?}"),
        }
    }
    fn confirm(&mut self, p: &str) -> Result<bool> {
        match self.next(p) {
            Answer::Bool(b) => Ok(b),
            a => panic!("{p}: expected y/n, got {a:?}"),
        }
    }
    fn input(&mut self, p: &str, initial: &str) -> Result<String> {
        match self.next(p) {
            Answer::Text(t) => Ok(t),
            Answer::Defaults => Ok(initial.to_owned()),
            a => panic!("{p}: expected text, got {a:?}"),
        }
    }
    fn literal_yes(&mut self, _: &mut dyn Write) -> Result<bool> {
        match self.next("yes?") {
            Answer::Bool(b) => Ok(b),
            a => panic!("yes?: expected y/n, got {a:?}"),
        }
    }
}
