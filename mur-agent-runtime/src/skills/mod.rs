pub mod injector;
pub mod trigger_matcher;

use mur_common::skill::loader::LoadedSkill;
use trigger_matcher::RegisteredTrigger;

pub struct RuntimeSkills {
    pub loaded: Vec<LoadedSkill>,
    pub triggers: Vec<RegisteredTrigger>,
}

impl RuntimeSkills {
    pub fn build(loaded: Vec<LoadedSkill>) -> Self {
        let triggers = trigger_matcher::register_from(&loaded);
        Self { loaded, triggers }
    }
}
