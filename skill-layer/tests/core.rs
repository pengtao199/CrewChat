use crewchat_skill::config::Config;
use crewchat_skill::store::{MaterializeError, SkillRef, Store};
use std::fs;
use std::path::Path;

fn config(root: &Path) -> Config {
    Config::at(
        root.join(".agents"),
        "127.0.0.1".to_string(),
        3460,
        "test-token".to_string(),
        None,
    )
    .unwrap()
}

fn write_skill(root: &Path, name: &str, description: &str, body: &str, script: bool) {
    fs::create_dir_all(root).unwrap();
    fs::write(
        root.join("SKILL.md"),
        format!("---\nname: {name}\ndescription: {description}\nlicense: MIT\n---\n\n{body}\n"),
    )
    .unwrap();
    if script {
        fs::create_dir_all(root.join("scripts")).unwrap();
        fs::write(root.join("scripts/run.sh"), "#!/bin/sh\necho ok\n").unwrap();
    }
}

#[test]
fn installs_lists_and_materializes_only_selected_skills() {
    let temp = tempfile::tempdir().unwrap();
    let mut store = Store::open(config(temp.path())).unwrap();

    let backend = temp.path().join("src/backend");
    write_skill(
        &backend,
        "backend-api",
        "Backend API work",
        "# Backend\nUse the API carefully.",
        true,
    );
    let frontend = temp.path().join("src/frontend");
    write_skill(
        &frontend,
        "frontend-ui",
        "Frontend UI work",
        "# Frontend\nUse the design system.",
        false,
    );

    let backend_skill = store.install_local(&backend).unwrap();
    let frontend_skill = store.install_local(&frontend).unwrap();
    assert_eq!(backend_skill.id, "backend-api");
    assert_eq!(frontend_skill.id, "frontend-ui");
    assert_eq!(backend_skill.current_hash.len(), 64);
    assert!(backend_skill.has_scripts);
    assert!(!frontend_skill.has_scripts);

    let listed = store.list().unwrap();
    assert_eq!(listed.len(), 2);
    assert!(store
        .get("backend-api")
        .unwrap()
        .unwrap()
        .content
        .contains("Backend"));

    let detail = store.get("backend-api").unwrap().unwrap();
    assert!(detail
        .files
        .iter()
        .any(|file| file.path == "scripts/run.sh"));

    let home = temp.path().join("sessions/backend");
    fs::create_dir_all(&home).unwrap();
    let result = store
        .materialize(
            &home,
            None,
            &[SkillRef {
                id: "backend-api".to_string(),
                hash: None,
            }],
        )
        .unwrap();

    assert_eq!(result.links.len(), 1);
    let link = home.join("skills/backend-api");
    assert!(link.is_symlink());
    assert!(!home.join("skills/frontend-ui").exists());
    let target = fs::read_link(&link).unwrap();
    assert_eq!(
        target,
        temp.path()
            .join(".agents/skill-store/backend-api")
            .join(&backend_skill.current_hash)
    );
    assert!(fs::read_to_string(link.join("SKILL.md"))
        .unwrap()
        .contains("Backend"));

    // Idempotent: the same link is reused.
    let again = store
        .materialize(
            &home,
            None,
            &[SkillRef {
                id: "backend-api".to_string(),
                hash: None,
            }],
        )
        .unwrap();
    assert_eq!(again.links.len(), 1);
}

#[test]
fn update_moves_current_but_old_session_keeps_its_hash() {
    let temp = tempfile::tempdir().unwrap();
    let mut store = Store::open(config(temp.path())).unwrap();
    let source = temp.path().join("src/backend");
    write_skill(&source, "backend-api", "Backend API work", "v1", false);

    let first = store.install_local(&source).unwrap();
    let home = temp.path().join("sessions/backend");
    fs::create_dir_all(&home).unwrap();
    let pinned = store
        .materialize(
            &home,
            None,
            &[SkillRef {
                id: "backend-api".to_string(),
                hash: Some(first.current_hash.clone()),
            }],
        )
        .unwrap();
    let old_target = fs::read_link(&pinned.links[0].path).unwrap();

    fs::write(
        source.join("SKILL.md"),
        "---\nname: backend-api\ndescription: Backend API work\nlicense: MIT\n---\n\nv2\n",
    )
    .unwrap();
    let second = store.install_local(&source).unwrap();
    assert_ne!(first.current_hash, second.current_hash);
    assert_eq!(
        fs::read_link(&pinned.links[0].path).unwrap(),
        old_target,
        "a materialized session must keep its pinned version"
    );

    let current = temp.path().join(".agents/skills/backend-api");
    assert!(current.is_symlink());
    let current_target = fs::read_link(current).unwrap();
    assert!(current_target
        .to_string_lossy()
        .contains(&second.current_hash));
}

#[test]
fn materialize_reports_missing_skills() {
    let temp = tempfile::tempdir().unwrap();
    let store = Store::open(config(temp.path())).unwrap();
    let home = temp.path().join("sessions/ios");
    fs::create_dir_all(&home).unwrap();

    let error = store
        .materialize(
            &home,
            None,
            &[SkillRef {
                id: "does-not-exist".to_string(),
                hash: None,
            }],
        )
        .unwrap_err();

    match error {
        MaterializeError::Missing(missing) => assert_eq!(missing, vec!["does-not-exist"]),
        other => panic!("unexpected error: {other}"),
    }
}

#[test]
fn materialize_respects_the_session_root() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join(".agents");
    let sessions = temp.path().join("sessions");
    fs::create_dir_all(&sessions).unwrap();
    let config = Config::at(
        root,
        "127.0.0.1".to_string(),
        3460,
        "test-token".to_string(),
        Some(sessions.clone()),
    )
    .unwrap();
    let mut store = Store::open(config).unwrap();
    let source = temp.path().join("src/backend");
    write_skill(&source, "backend-api", "Backend API work", "v1", false);
    store.install_local(&source).unwrap();

    let outside = temp.path().join("outside");
    fs::create_dir_all(&outside).unwrap();
    let error = store
        .materialize(
            &outside,
            None,
            &[SkillRef {
                id: "backend-api".to_string(),
                hash: None,
            }],
        )
        .unwrap_err();
    assert!(error
        .to_string()
        .contains("outside the allowed session root"));

    let inside = sessions.join("backend");
    fs::create_dir_all(&inside).unwrap();
    store
        .materialize(
            &inside,
            None,
            &[SkillRef {
                id: "backend-api".to_string(),
                hash: None,
            }],
        )
        .unwrap();
}

#[test]
fn registry_survives_reopen_and_collections_validate_skills() {
    let temp = tempfile::tempdir().unwrap();
    let config = config(temp.path());
    let source = temp.path().join("src/backend");
    write_skill(&source, "backend-api", "Backend API work", "v1", false);

    {
        let mut store = Store::open(config.clone()).unwrap();
        store.install_local(&source).unwrap();
        let collection = store
            .create_collection("Backend", "Backend skills", &["backend-api".to_string()])
            .unwrap();
        assert_eq!(collection.skill_ids, vec!["backend-api"]);
    }

    let store = Store::open(config).unwrap();
    assert_eq!(store.list().unwrap().len(), 1);
    let collections = store.list_collections().unwrap();
    assert_eq!(collections.len(), 1);
    assert_eq!(collections[0].skill_ids, vec!["backend-api"]);
}
