use pwcli::tools::{
    skills::{discover_skill_roots, watcher::scan_skill_roots, SkillManifest, SkillToolLoader},
    InvocationMode, ToolLoader,
};
use std::fs;

#[test]
fn parses_standard_skill_md_with_optional_resources() {
    let temp = tempfile::tempdir().unwrap();
    let skill_dir = temp.path().join("code-review");
    fs::create_dir_all(skill_dir.join("scripts")).unwrap();
    fs::create_dir_all(skill_dir.join("references")).unwrap();
    fs::write(
        skill_dir.join("SKILL.md"),
        r#"---
name: code-review
description: Use for code review.
---

# Code Review

Follow the review workflow.
"#,
    )
    .unwrap();

    let manifest = SkillManifest::from_skill_dir(&skill_dir).unwrap();
    assert_eq!(manifest.name, "code-review");
    assert_eq!(manifest.description, "Use for code review.");
    assert!(manifest.resources.scripts_dir.is_some());
    assert!(manifest.resources.references_dir.is_some());
    assert!(manifest.executable.is_none());
}

#[test]
fn falls_back_to_directory_name_and_first_paragraph() {
    let temp = tempfile::tempdir().unwrap();
    let skill_dir = temp.path().join("fallback-skill");
    fs::create_dir_all(&skill_dir).unwrap();
    fs::write(
        skill_dir.join("SKILL.md"),
        r#"# Fallback Skill

Use when frontmatter is missing.
"#,
    )
    .unwrap();

    let manifest = SkillManifest::from_skill_dir(&skill_dir).unwrap();
    assert_eq!(manifest.name, "fallback-skill");
    assert_eq!(manifest.description, "Use when frontmatter is missing.");
    assert_eq!(manifest.warnings.len(), 2);
}

#[test]
fn loader_registers_executable_skill_when_declared() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join(".agents/skills");
    let skill_dir = root.join("exec-skill");
    fs::create_dir_all(skill_dir.join("scripts")).unwrap();
    fs::write(
        skill_dir.join("SKILL.md"),
        r#"---
name: exec-skill
description: Use executable skill.
executable:
  command: ["python3", "${skill_dir}/scripts/run.py"]
---

Run the script.
"#,
    )
    .unwrap();

    let loader = SkillToolLoader::new(vec![root]);
    let tools = loader.load().unwrap();
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0].descriptor.id, "skill.exec-skill");
    assert_eq!(
        tools[0].descriptor.invocation_mode,
        InvocationMode::ExecutableJson
    );
    assert!(tools[0].executor.is_some());
}

#[test]
fn discovers_user_and_workspace_skill_roots() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("home");
    let repo = temp.path().join("repo");
    let nested = repo.join("crates/app");
    fs::create_dir_all(home.join(".agents/skills")).unwrap();
    fs::create_dir_all(&nested).unwrap();
    fs::create_dir_all(repo.join(".git")).unwrap();

    let roots = discover_skill_roots(&home, &nested);
    assert!(roots.contains(&home.join(".agents/skills")));
    assert!(roots.contains(&home.join(".claude/skills")));
    assert!(roots.contains(&nested.join(".agents/skills")));
    assert!(roots.contains(&repo.join(".agents/skills")));
}

#[test]
fn skill_watcher_inventory_tracks_changes_and_conflicts() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join(".agents/skills");
    let a = root.join("a");
    let b = root.join("b");
    fs::create_dir_all(&a).unwrap();
    fs::create_dir_all(&b).unwrap();
    fs::write(
        a.join("SKILL.md"),
        r#"---
name: duplicate
description: A.
---

A skill.
"#,
    )
    .unwrap();
    fs::write(
        b.join("SKILL.md"),
        r#"---
name: duplicate
description: B.
---

B skill.
"#,
    )
    .unwrap();

    let first = scan_skill_roots(std::slice::from_ref(&root)).unwrap();
    assert_eq!(first.conflicts.len(), 1);
    assert_eq!(first.conflicts[0].tool_id, "skill.duplicate");

    std::thread::sleep(std::time::Duration::from_millis(2));
    fs::write(
        b.join("SKILL.md"),
        r#"---
name: unique
description: B.
---

B skill.
"#,
    )
    .unwrap();
    let second = scan_skill_roots(&[root]).unwrap();
    assert_ne!(first.fingerprint, second.fingerprint);
    assert!(second.conflicts.is_empty());
    assert!(second.tool_ids.contains(&"skill.unique".to_string()));
}
