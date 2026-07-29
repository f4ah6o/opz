use crate::*;

pub(crate) static OPZ_SKILL: &str = include_str!("../.agents/skills/opz/SKILL.md");

pub(crate) fn print_bundled_skill() -> Result<()> {
    instrumentation::with_span("main_operation", vec![], || ());
    instrumentation::with_span("write_outputs", vec![], || {
        print!("{OPZ_SKILL}");
    });
    Ok(())
}
