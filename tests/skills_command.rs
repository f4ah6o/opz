use std::process::Command;

const EXPECTED_SKILL: &str = include_str!("../.agents/skills/opz/SKILL.md");

#[test]
fn skills_command_prints_bundled_skill() {
    let output = Command::new(env!("CARGO_BIN_EXE_opz"))
        .arg("skills")
        .output()
        .unwrap();

    assert!(output.status.success());
    assert_eq!(String::from_utf8(output.stdout).unwrap(), EXPECTED_SKILL);
    assert!(output.stderr.is_empty());
}
