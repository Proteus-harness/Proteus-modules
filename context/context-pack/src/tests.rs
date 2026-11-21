use super::*;
use crate::search_queries::extract_search_queries;
use std::process::Command;

#[test]
fn context_entrypoints_decode_config_before_calling_providers() {
    use proteus_contracts::process_module::{ContextBuilderModuleHost, ProcessModuleResult};

    #[derive(Default)]
    struct FixtureHost(std::sync::Mutex<Vec<String>>);

    impl ContextBuilderModuleHost for FixtureHost {
        fn context_provider_json(
            &self,
            provider: String,
            _request: String,
        ) -> ProcessModuleResult<String> {
            self.0.lock().unwrap().push(provider);
            Ok("[]".to_owned())
        }
    }

    let task = proteus_contracts::domain::AgentTask::new("inspect the note", "fixture-root".into());
    let invocation = |config| {
        serde_json::to_string(&ContextBuilderModuleInput {
            task: task.clone(),
            config,
            attribution: proteus_contracts::contracts::ExecutionAttribution::detached(
                proteus_contracts::domain::new_execution_id(),
            ),
        })
        .unwrap()
    };
    for (builder, config) in [
        (
            &SimpleContextBuilderModule as &dyn ContextBuilderModule,
            json!({"memory_provider":"fixture_provider","max_search_results":0}),
        ),
        (
            &RepoAwareContextBuilderModule as &dyn ContextBuilderModule,
            json!({"providers":["fixture_provider"]}),
        ),
        (
            &CodexContextBuilderModule as &dyn ContextBuilderModule,
            json!({"providers":["fixture_provider"]}),
        ),
    ] {
        let mut host = FixtureHost::default();
        let error = builder
            .build_json(invocation(json!({"fixture_typo":true})), &mut host)
            .unwrap_err();
        assert!(error.message.contains("unknown field `fixture_typo`"));
        assert!(host.0.lock().unwrap().is_empty());
        builder.build_json(invocation(config), &mut host).unwrap();
        assert_eq!(host.0.into_inner().unwrap(), ["fixture_provider"]);
    }
}

#[test]
fn context_configs_reject_unknown_keys_and_keep_known_limits() {
    for invalid in [
        serde_json::from_value::<SimpleContextConfig>(json!({"max_search_results": 0, "typo": 1}))
            .is_err(),
        serde_json::from_value::<RepoAwareContextConfig>(
            json!({"max_search_results": 0, "typo": 1}),
        )
        .is_err(),
        serde_json::from_value::<CodexContextConfig>(json!({"max_search_results": 0, "typo": 1}))
            .is_err(),
    ] {
        assert!(invalid);
    }
    assert_eq!(
        serde_json::from_value::<SimpleContextConfig>(json!({"max_search_results": 0}))
            .unwrap()
            .max_search_results,
        0
    );
    assert_eq!(
        serde_json::from_value::<RepoAwareContextConfig>(json!({"max_search_results": 0}))
            .unwrap()
            .max_search_results,
        0
    );
    assert_eq!(
        serde_json::from_value::<CodexContextConfig>(json!({"max_search_results": 0}))
            .unwrap()
            .max_search_results,
        0
    );
}

#[test]
fn zero_search_limit_skips_host_search_in_simple_and_repo_aware() {
    use proteus_contracts::process_module::{ContextBuilderModuleHost, ProcessModuleResult};

    struct NoSearchHost;
    impl ContextBuilderModuleHost for NoSearchHost {
        fn context_provider_json(&self, _: String, _: String) -> ProcessModuleResult<String> {
            panic!("disabled retrieval must not call a provider")
        }
    }
    let input = ContextBuilderModuleInput {
        task: proteus_contracts::domain::AgentTask::new(
            "Find approval policy",
            PathBuf::from("/ws"),
        ),
        config: Value::Null,
        attribution: proteus_contracts::contracts::ExecutionAttribution::detached(
            proteus_contracts::domain::new_execution_id(),
        ),
    };
    let mut host = NoSearchHost;
    let simple = build_simple_context(
        input.clone(),
        &mut host,
        SimpleContextConfig {
            max_search_results: 0,
            search_provider: Some("search-fixture".into()),
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(simple.chunks.len(), 1);
    let repo = search_chunks(
        &input,
        &mut host,
        &RepoAwareContextConfig {
            max_search_results: 0,
            search_provider: Some("search-fixture".into()),
            ..Default::default()
        },
    )
    .unwrap();
    assert!(repo.is_empty());
}

#[test]
fn exact_byte_budget_keeps_every_chunk_in_provider_order() {
    let chunks = vec![
        ContextChunk::new("low", "я").with_score(0.1),
        ContextChunk::new("rules", "a").with_metadata(json!({"provider": "project_instructions"})),
        ContextChunk::new("high", "").with_score(1.0),
    ];
    assert_eq!(apply_byte_budget(chunks.clone(), 3).unwrap(), chunks);
}

#[test]
fn byte_budget_prefers_higher_score_and_restores_original_order() {
    let chunks = vec![
        ContextChunk::new("low", "11111").with_score(0.1),
        ContextChunk::new("high_a", "22222").with_score(0.9),
        ContextChunk::new("high_b", "33333").with_score(0.8),
    ];

    let selected = apply_byte_budget(chunks, 10).unwrap();

    assert_eq!(
        selected
            .iter()
            .map(|chunk| chunk.source.as_str())
            .collect::<Vec<_>>(),
        vec!["high_a", "high_b"]
    );
}

#[test]
fn regression_context_budget_keeps_project_instructions_before_ranked_evidence() {
    let instructions = chunk(
        "repo_aware:project_instructions",
        None,
        "rules".into(),
        0.95,
        "project_instructions",
        "project instruction file",
    );
    let selected = apply_byte_budget(
        vec![
            ContextChunk::new("search", "noise").with_score(1.0),
            instructions,
        ],
        5,
    )
    .unwrap();
    assert_eq!(selected.len(), 1);
    assert_eq!(selected[0].content, "rules");
}

#[test]
fn context_budget_reports_project_instruction_overflow_instead_of_dropping_rules() {
    for limit in [0, 4] {
        let instructions = chunk(
            "repo_aware:project_instructions",
            None,
            "rules".into(),
            0.95,
            "project_instructions",
            "project instruction file",
        );
        let error = apply_byte_budget(vec![instructions], limit).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("cannot fit loaded project instructions")
        );
    }
}

#[test]
fn byte_budget_keeps_tie_score_order() {
    let chunks = vec![
        ContextChunk::new("first", "11111").with_score(0.5),
        ContextChunk::new("second", "22222").with_score(0.5),
        ContextChunk::new("third", "33333").with_score(0.5),
    ];

    let selected = apply_byte_budget(chunks, 10).unwrap();

    assert_eq!(
        selected
            .iter()
            .map(|chunk| chunk.source.as_str())
            .collect::<Vec<_>>(),
        vec!["first", "second"]
    );
}

#[test]
fn bounded_workspace_read_reads_only_limit() {
    let dir = tempfile::tempdir().expect("workspace");
    let path = dir.path().join("large.txt");
    std::fs::write(&path, "abcdef").expect("large file");

    let content = read_bounded_workspace_utf8_file(dir.path(), &path, 3)
        .expect("bounded read")
        .expect("content");

    assert_eq!(content, "abc");
}

#[cfg(unix)]
#[test]
fn bounded_workspace_read_rejects_symlink_escape() {
    let dir = tempfile::tempdir().expect("workspace");
    let outside = tempfile::tempdir().expect("outside");
    let outside_file = outside.path().join("secret.txt");
    std::fs::write(&outside_file, "secret").expect("outside file");
    let link = dir.path().join("AGENTS.md");
    std::os::unix::fs::symlink(&outside_file, &link).expect("symlink");

    let content = read_bounded_workspace_utf8_file(dir.path(), &link, 100).expect("bounded read");

    assert!(content.is_none());
}

#[test]
fn project_instruction_chunks_layer_root_to_cwd_and_use_override_first() {
    let dir = tempfile::tempdir().expect("workspace");
    let root = dir.path();
    let service = root.join("services");
    let cwd = service.join("payments");
    std::fs::create_dir_all(&cwd).expect("nested cwd");
    std::fs::write(root.join("AGENTS.md"), "root rules\n").expect("root agents");
    std::fs::write(service.join("AGENTS.md"), "service rules\n").expect("service agents");
    std::fs::write(cwd.join("AGENTS.md"), "base payment rules\n").expect("cwd agents");
    std::fs::write(cwd.join("AGENTS.override.md"), "override payment rules\n")
        .expect("cwd override");
    let config = RepoAwareContextConfig::default();

    let chunks = project_instruction_chunks_from_root(&cwd, root, &config).expect("chunks");

    assert_eq!(
        chunks
            .iter()
            .map(|chunk| chunk.path.as_deref())
            .collect::<Vec<_>>(),
        vec![
            Some(Path::new("AGENTS.md")),
            Some(Path::new("services/AGENTS.md")),
            Some(Path::new("services/payments/AGENTS.override.md")),
        ]
    );
    assert_eq!(
        chunks
            .iter()
            .map(|chunk| chunk.content.as_str())
            .collect::<Vec<_>>(),
        vec![
            "root rules\n",
            "service rules\n",
            "override payment rules\n",
        ]
    );
}

#[test]
fn project_instruction_chunks_skip_empty_override_for_fallback_file() {
    let dir = tempfile::tempdir().expect("workspace");
    std::fs::write(dir.path().join("AGENTS.override.md"), "").expect("empty override");
    std::fs::write(dir.path().join("AGENTS.md"), "fallback rules\n").expect("agents");
    let config = RepoAwareContextConfig::default();

    let chunks =
        project_instruction_chunks_from_root(dir.path(), dir.path(), &config).expect("chunks");

    assert_eq!(chunks.len(), 1);
    assert_eq!(chunks[0].path.as_deref(), Some(Path::new("AGENTS.md")));
    assert_eq!(chunks[0].content, "fallback rules\n");
}

#[test]
fn project_instruction_root_uses_git_root_when_available() {
    let dir = tempfile::tempdir().expect("workspace");
    let status = Command::new("git")
        .arg("init")
        .arg("-q")
        .current_dir(dir.path())
        .status();
    let Ok(status) = status else {
        return;
    };
    if !status.success() {
        return;
    }
    let cwd = dir.path().join("services/payments");
    std::fs::create_dir_all(&cwd).expect("nested cwd");

    let root = project_instruction_root(&cwd).expect("instruction root");

    assert_eq!(root, dir.path().canonicalize().expect("canonical root"));
}

#[test]
fn environment_chunks_report_current_platform_and_sh() {
    let input = ContextBuilderModuleInput {
        task: proteus_contracts::domain::AgentTask::new("task", PathBuf::from("/ws")),
        config: Value::Null,
        attribution: proteus_contracts::contracts::ExecutionAttribution::detached(
            proteus_contracts::domain::new_execution_id(),
        ),
    };

    let chunks = environment_chunks(&input);

    assert_eq!(chunks.len(), 1);
    assert_eq!(chunks[0].source, "repo_aware:environment");
    let content = &chunks[0].content;
    assert!(content.starts_with(ENVIRONMENT_CONTEXT_TAG), "{content}");
    assert!(content.contains("<cwd>/ws</cwd>"), "{content}");
    assert!(
        content.contains(&format!(
            "<operating_system>{}</operating_system>",
            std::env::consts::OS
        )),
        "{content}"
    );
    assert!(
        content.contains(&format!(
            "<architecture>{}</architecture>",
            std::env::consts::ARCH
        )),
        "{content}"
    );
    assert!(
        content.contains(&format!("<shell>{EXEC_SHELL}</shell>")),
        "{content}"
    );
}

#[test]
fn extract_search_queries_keeps_domain_lowercase_terms() {
    let queries = extract_search_queries("почему approval не работает где shell policy?");

    assert_eq!(queries, vec!["approval", "shell", "policy"]);
}

#[test]
fn extract_search_queries_skips_common_lowercase_stopwords() {
    let queries = extract_search_queries("what should this context workflow inspect");

    assert_eq!(queries, vec!["context", "workflow", "inspect"]);
}

#[test]
fn extract_search_queries_dedupes_case_insensitively() {
    let queries = extract_search_queries("Workflow workflow ToolSafety tool_safety");

    assert_eq!(queries, vec!["Workflow", "ToolSafety", "tool_safety"]);
}

#[test]
fn extract_search_queries_splits_filenames_before_selecting_terms() {
    assert_eq!(
        extract_search_queries("Cargo.toml lib.rs guide.md"),
        vec!["Cargo", "toml", "guide"]
    );
    assert_eq!(extract_search_queries(" lib.rs "), vec!["lib.rs"]);
}

#[test]
fn provider_enrichment_preserves_metadata_precedence_and_non_object_handling() {
    use proteus_contracts::{
        contracts::ExecutionAttribution,
        domain::{AgentTask, new_execution_id},
        process_module::{ContextBuilderModuleHost, ProcessModuleResult},
    };

    struct FixtureHost;
    impl ContextBuilderModuleHost for FixtureHost {
        fn context_provider_json(
            &self,
            provider_id: String,
            request: String,
        ) -> ProcessModuleResult<String> {
            let request: ProcessContextProviderInput = serde_json::from_str(&request).unwrap();
            assert_eq!(request.provider_id, provider_id);
            let object = json!({
                "provider": "provided",
                "reason": "from fixture",
                "query": "stale",
                "context_profile": "original",
                "nested": {"examples": ["Пример🙂", null, 17]}
            });
            let chunks = match provider_id.as_str() {
                "memory-fixture" => vec![
                    ContextChunk::new("memory:fact", "object").with_metadata(object),
                    ContextChunk::new("memory:preference", "array").with_metadata(json!([1, 2])),
                ],
                "search-fixture" => vec![
                    ContextChunk::new("rg", "matched line")
                        .with_path("src/fixture.rs".into())
                        .with_metadata(object),
                    ContextChunk::new("rg", "matched line")
                        .with_path("src/fixture.rs".into())
                        .with_metadata(json!({"should_be_deduped": true})),
                ],
                "external-fixture" => vec![
                    ContextChunk::new("other", "scalar").with_metadata(json!("text")),
                    ContextChunk::new("repo_aware:other", "external object").with_metadata(object),
                ],
                _ => panic!("unexpected fixture provider {provider_id}"),
            };
            Ok(serde_json::to_string(&chunks).unwrap())
        }
    }

    let config = json!({
        "providers": ["memory", "search", "external-fixture"],
        "memory_provider": "memory-fixture",
        "search_provider": "search-fixture",
    });
    let input = serde_json::to_string(&ContextBuilderModuleInput {
        task: AgentTask::new("needle", PathBuf::from("fixture")),
        attribution: ExecutionAttribution::detached(new_execution_id()),
        config,
    })
    .unwrap();

    let codex: ContextBundle = serde_json::from_str(
        &CodexContextBuilderModule
            .build_json(input.clone(), &mut FixtureHost)
            .unwrap(),
    )
    .unwrap();
    let repo: ContextBundle = serde_json::from_str(
        &RepoAwareContextBuilderModule
            .build_json(input, &mut FixtureHost)
            .unwrap(),
    )
    .unwrap();
    let expected = json!({
        "provider": "provided",
        "reason": "from fixture",
        "query": "stale",
        "context_profile": "codex_context",
        "nested": {"examples": ["Пример🙂", null, 17]}
    });
    assert_eq!(codex.chunks.len(), 5);
    assert_eq!(codex.chunks[0].metadata, expected);
    assert_eq!(
        codex.chunks[1].metadata,
        json!({"provider": "memory", "reason": "memory recall", "context_profile": "codex_context"})
    );
    assert_eq!(codex.chunks[2].metadata["query"], "needle");
    assert_eq!(codex.chunks[2].metadata["provider"], "provided");
    assert_eq!(codex.chunks[2].metadata["nested"], expected["nested"]);
    assert_eq!(
        codex.chunks[3].metadata,
        json!({"context_profile": "codex_context"})
    );
    assert_eq!(codex.chunks[4].source, "codex_context:other");
    assert_eq!(codex.chunks[4].metadata, expected);
    assert_eq!(repo.chunks.len(), 6);
    assert_eq!(repo.chunks[1].metadata["context_profile"], "original");
    assert_eq!(repo.chunks[3].metadata["query"], "needle");
    assert_eq!(repo.chunks[4].metadata, "text");
    assert_eq!(repo.chunks[5].metadata["context_profile"], "original");
}
