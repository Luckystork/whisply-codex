use super::*;
use pretty_assertions::assert_eq;
use std::collections::HashMap;
use std::collections::HashSet;
use whisply_utils_absolute_path::AbsolutePathBuf;
use whisply_utils_absolute_path::test_support::PathBufExt;
use whisply_utils_absolute_path::test_support::test_path_buf;

#[derive(Default)]
struct TestLookup {
    skills: Vec<SkillMetadata>,
    disabled_paths: HashSet<AbsolutePathBuf>,
    skill_discovery_path_by_path: HashMap<AbsolutePathBuf, AbsolutePathBuf>,
}

impl ExplicitSkillLookup for TestLookup {
    fn skills(&self) -> &[SkillMetadata] {
        &self.skills
    }

    fn disabled_paths(&self) -> &HashSet<AbsolutePathBuf> {
        &self.disabled_paths
    }

    fn skill_discovery_path_for_path(&self, path: &AbsolutePathBuf) -> Option<&AbsolutePathBuf> {
        self.skill_discovery_path_by_path.get(path)
    }
}

fn make_skill(name: &str, path: &str) -> SkillMetadata {
    SkillMetadata {
        name: name.to_string(),
        description: format!("{name} skill"),
        short_description: None,
        version: None,
        interface: None,
        dependencies: None,
        policy: None,
        path_to_skills_md: test_path_buf(path).abs(),
        scope: whisply_protocol::protocol::SkillScope::User,
        plugin_id: None,
        remote_plugin_id: None,
    }
}

fn linked_skill_mention(name: &str, unix_path: &str) -> String {
    format!("[${name}]({})", test_path_buf(unix_path).display())
}

fn collect_mentions(
    inputs: &[UserInput],
    skills: &[SkillMetadata],
    disabled_paths: &HashSet<AbsolutePathBuf>,
    connector_slug_counts: &HashMap<String, usize>,
) -> Vec<SkillMetadata> {
    let loaded_skills = TestLookup {
        skills: skills.to_vec(),
        disabled_paths: disabled_paths.clone(),
        ..Default::default()
    };
    collect_explicit_skill_mentions(inputs, &loaded_skills, connector_slug_counts)
}

fn collect_reporting_unused(
    inputs: &[UserInput],
    skills: &[SkillMetadata],
    connector_slug_counts: &HashMap<String, usize>,
) -> ExplicitSkillMentions {
    let loaded_skills = TestLookup {
        skills: skills.to_vec(),
        ..Default::default()
    };
    collect_explicit_skill_mentions_reporting_unused(inputs, &loaded_skills, connector_slug_counts)
}

fn text(text: &str) -> Vec<UserInput> {
    vec![UserInput::Text {
        text: text.to_string(),
        text_elements: Vec::new(),
    }]
}

fn skill_in_repo(name: &str, path: &str) -> SkillMetadata {
    SkillMetadata {
        scope: whisply_protocol::protocol::SkillScope::Repo,
        ..make_skill(name, path)
    }
}

/// The whole point of the change: the turn ran without the skill, and the
/// person is told so rather than left to infer it from a worse answer.
#[test]
fn an_ambiguous_name_is_reported_rather_than_dropped_in_silence() {
    let skills = vec![
        skill_in_repo("demo-skill", "/tmp/repo"),
        make_skill("demo-skill", "/tmp/home"),
    ];

    let mentions = collect_reporting_unused(&text("use $demo-skill"), &skills, &HashMap::new());

    assert_eq!(mentions.selected, Vec::new());
    assert_eq!(mentions.unused.len(), 1);
    assert_eq!(mentions.unused[0].name, "demo-skill");
}

/// Naming both packages is what makes the message actionable; "ambiguous" on
/// its own leaves the person with nothing to do about it.
#[test]
fn the_report_names_both_packages_and_where_each_one_is() {
    let skills = vec![
        skill_in_repo("demo-skill", "/tmp/repo"),
        make_skill("demo-skill", "/tmp/home"),
    ];

    let mentions = collect_reporting_unused(&text("use $demo-skill"), &skills, &HashMap::new());
    let message = mentions.unused[0].describe();

    assert!(
        message.contains("2 skills are named \"demo-skill\""),
        "{message}"
    );
    assert!(message.contains("the repository one"), "{message}");
    assert!(message.contains("the home one"), "{message}");
    assert!(
        message.contains(&test_path_buf("/tmp/repo").display().to_string()),
        "{message}"
    );
    assert!(message.contains("did not run"), "{message}");
}

/// A mention that resolved by path chose deliberately. Warning about the name
/// it also matched would report a problem the person already solved.
#[test]
fn a_name_the_message_also_resolved_by_path_is_not_reported() {
    let skills = vec![
        skill_in_repo("demo-skill", "/tmp/repo"),
        make_skill("demo-skill", "/tmp/home"),
    ];
    let inputs = text(&format!(
        "use $demo-skill and {}",
        linked_skill_mention("demo-skill", "/tmp/home")
    ));

    let mentions = collect_reporting_unused(&inputs, &skills, &HashMap::new());

    assert_eq!(mentions.selected.len(), 1);
    assert_eq!(mentions.unused, Vec::new());
}

/// One `$name` is one question, however many packages could have answered it
/// and however many times it was written.
#[test]
fn a_repeated_name_is_reported_once() {
    let skills = vec![
        skill_in_repo("demo-skill", "/tmp/repo"),
        make_skill("demo-skill", "/tmp/home"),
        make_skill("demo-skill", "/tmp/other"),
    ];

    let mentions = collect_reporting_unused(
        &text("use $demo-skill, then $demo-skill again"),
        &skills,
        &HashMap::new(),
    );

    assert_eq!(mentions.unused.len(), 1);
    let message = mentions.unused[0].describe();
    assert!(message.contains("3 skills are named"), "{message}");
}

/// The connector never answers a bare name either, so this mention did nothing
/// at all, which is exactly the case worth saying out loud.
#[test]
fn a_name_a_connector_also_answers_to_says_which_two_things_it_could_mean() {
    let skills = vec![make_skill("notes", "/tmp/notes")];
    let connector_counts = HashMap::from([("notes".to_string(), 1)]);

    let mentions = collect_reporting_unused(&text("use $notes"), &skills, &connector_counts);

    assert_eq!(mentions.selected, Vec::new());
    let message = mentions.unused[0].describe();
    assert!(message.contains("a connector are both named"), "{message}");
    assert!(message.contains("did not run either"), "{message}");
}

/// A name that resolves is the ordinary case and must stay quiet; a warning on
/// every mention would train people to ignore the one that matters.
#[test]
fn a_name_that_can_only_mean_one_package_is_not_reported() {
    let skills = vec![make_skill("demo-skill", "/tmp/home")];

    let mentions = collect_reporting_unused(&text("use $demo-skill"), &skills, &HashMap::new());

    assert_eq!(mentions.selected.len(), 1);
    assert_eq!(mentions.unused, Vec::new());
}

/// A word that is nobody's skill is not a failed skill mention.
#[test]
fn a_name_no_package_answers_to_is_not_reported() {
    let skills = vec![make_skill("demo-skill", "/tmp/home")];

    let mentions =
        collect_reporting_unused(&text("costs $400 for $nothing"), &skills, &HashMap::new());

    assert_eq!(mentions.unused, Vec::new());
}

fn skill_outcome_with_discovery_path(skill: SkillMetadata, discovery_path: &str) -> TestLookup {
    TestLookup {
        skill_discovery_path_by_path: HashMap::from([(
            skill.path_to_skills_md.clone(),
            test_path_buf(discovery_path).abs(),
        )]),
        skills: vec![skill],
        ..Default::default()
    }
}

#[test]
fn collect_explicit_skill_mentions_text_respects_skill_order() {
    let alpha = make_skill("alpha-skill", "/tmp/alpha");
    let beta = make_skill("beta-skill", "/tmp/beta");
    let skills = vec![beta.clone(), alpha.clone()];
    let inputs = vec![UserInput::Text {
        text: "first $alpha-skill then $beta-skill".to_string(),
        text_elements: Vec::new(),
    }];
    let connector_counts = HashMap::new();

    let selected = collect_mentions(&inputs, &skills, &HashSet::new(), &connector_counts);

    // Text scanning should not change the previous selection ordering semantics.
    assert_eq!(selected, vec![beta, alpha]);
}

#[test]
fn collect_explicit_skill_mentions_prioritizes_structured_inputs() {
    let alpha = make_skill("alpha-skill", "/tmp/alpha");
    let beta = make_skill("beta-skill", "/tmp/beta");
    let skills = vec![alpha.clone(), beta.clone()];
    let inputs = vec![
        UserInput::Text {
            text: "please run $alpha-skill".to_string(),
            text_elements: Vec::new(),
        },
        UserInput::Skill {
            name: "beta-skill".to_string(),
            path: test_path_buf("/tmp/beta"),
        },
    ];
    let connector_counts = HashMap::new();

    let selected = collect_mentions(&inputs, &skills, &HashSet::new(), &connector_counts);

    assert_eq!(selected, vec![beta, alpha]);
}

#[test]
fn collect_explicit_skill_mentions_accepts_structured_discovery_path() {
    let skill = make_skill("linked-skill", "/tmp/shared/linked-skill/SKILL.md");
    let loaded_skills = skill_outcome_with_discovery_path(
        skill.clone(),
        "/tmp/project/.agents/skills/linked-skill/SKILL.md",
    );
    let inputs = vec![UserInput::Skill {
        name: "linked-skill".to_string(),
        path: test_path_buf("/tmp/project/.agents/skills/linked-skill/SKILL.md"),
    }];

    let selected = collect_explicit_skill_mentions(&inputs, &loaded_skills, &HashMap::new());

    assert_eq!(selected, vec![skill]);
}

#[test]
fn collect_explicit_skill_mentions_accepts_linked_discovery_path() {
    let skill = make_skill("linked-skill", "/tmp/shared/linked-skill/SKILL.md");
    let loaded_skills = skill_outcome_with_discovery_path(
        skill.clone(),
        "/tmp/project/.agents/skills/linked-skill/SKILL.md",
    );
    let inputs = vec![UserInput::Text {
        text: linked_skill_mention(
            "linked-skill",
            "/tmp/project/.agents/skills/linked-skill/SKILL.md",
        ),
        text_elements: Vec::new(),
    }];

    let selected = collect_explicit_skill_mentions(&inputs, &loaded_skills, &HashMap::new());

    assert_eq!(selected, vec![skill]);
}

#[test]
fn collect_explicit_skill_mentions_rejects_disabled_discovery_path() {
    let skill = make_skill("linked-skill", "/tmp/shared/linked-skill/SKILL.md");
    let mut loaded_skills = skill_outcome_with_discovery_path(
        skill.clone(),
        "/tmp/project/.agents/skills/linked-skill/SKILL.md",
    );
    loaded_skills.disabled_paths.insert(skill.path_to_skills_md);
    let inputs = vec![UserInput::Skill {
        name: "linked-skill".to_string(),
        path: test_path_buf("/tmp/project/.agents/skills/linked-skill/SKILL.md"),
    }];

    let selected = collect_explicit_skill_mentions(&inputs, &loaded_skills, &HashMap::new());

    assert_eq!(selected, Vec::new());
}

#[test]
fn collect_explicit_skill_mentions_skips_invalid_structured_and_blocks_plain_fallback() {
    let alpha = make_skill("alpha-skill", "/tmp/alpha");
    let skills = vec![alpha];
    let inputs = vec![
        UserInput::Text {
            text: "please run $alpha-skill".to_string(),
            text_elements: Vec::new(),
        },
        UserInput::Skill {
            name: "alpha-skill".to_string(),
            path: test_path_buf("/tmp/missing"),
        },
    ];
    let connector_counts = HashMap::new();

    let selected = collect_mentions(&inputs, &skills, &HashSet::new(), &connector_counts);

    assert_eq!(selected, Vec::new());
}

#[test]
fn collect_explicit_skill_mentions_skips_disabled_structured_and_blocks_plain_fallback() {
    let alpha = make_skill("alpha-skill", "/tmp/alpha");
    let skills = vec![alpha];
    let inputs = vec![
        UserInput::Text {
            text: "please run $alpha-skill".to_string(),
            text_elements: Vec::new(),
        },
        UserInput::Skill {
            name: "alpha-skill".to_string(),
            path: test_path_buf("/tmp/alpha"),
        },
    ];
    let disabled = HashSet::from([test_path_buf("/tmp/alpha").abs()]);
    let connector_counts = HashMap::new();

    let selected = collect_mentions(&inputs, &skills, &disabled, &connector_counts);

    assert_eq!(selected, Vec::new());
}

#[test]
fn collect_explicit_skill_mentions_dedupes_by_path() {
    let alpha = make_skill("alpha-skill", "/tmp/alpha");
    let skills = vec![alpha.clone()];
    let mention = linked_skill_mention("alpha-skill", "/tmp/alpha");
    let inputs = vec![UserInput::Text {
        text: format!("use {mention} and {mention}"),
        text_elements: Vec::new(),
    }];
    let connector_counts = HashMap::new();

    let selected = collect_mentions(&inputs, &skills, &HashSet::new(), &connector_counts);

    assert_eq!(selected, vec![alpha]);
}

#[test]
fn collect_explicit_skill_mentions_skips_ambiguous_name() {
    let alpha = make_skill("demo-skill", "/tmp/alpha");
    let beta = make_skill("demo-skill", "/tmp/beta");
    let skills = vec![alpha, beta];
    let inputs = vec![UserInput::Text {
        text: "use $demo-skill and again $demo-skill".to_string(),
        text_elements: Vec::new(),
    }];
    let connector_counts = HashMap::new();

    let selected = collect_mentions(&inputs, &skills, &HashSet::new(), &connector_counts);

    assert_eq!(selected, Vec::new());
}

#[test]
fn collect_explicit_skill_mentions_prefers_linked_path_over_name() {
    let alpha = make_skill("demo-skill", "/tmp/alpha");
    let beta = make_skill("demo-skill", "/tmp/beta");
    let skills = vec![alpha, beta.clone()];
    let inputs = vec![UserInput::Text {
        text: format!(
            "use $demo-skill and {}",
            linked_skill_mention("demo-skill", "/tmp/beta")
        ),
        text_elements: Vec::new(),
    }];
    let connector_counts = HashMap::new();

    let selected = collect_mentions(&inputs, &skills, &HashSet::new(), &connector_counts);

    assert_eq!(selected, vec![beta]);
}

#[test]
fn collect_explicit_skill_mentions_skips_plain_name_when_connector_matches() {
    let alpha = make_skill("alpha-skill", "/tmp/alpha");
    let skills = vec![alpha];
    let inputs = vec![UserInput::Text {
        text: "use $alpha-skill".to_string(),
        text_elements: Vec::new(),
    }];
    let connector_counts = HashMap::from([("alpha-skill".to_string(), 1)]);

    let selected = collect_mentions(&inputs, &skills, &HashSet::new(), &connector_counts);

    assert_eq!(selected, Vec::new());
}

#[test]
fn collect_explicit_skill_mentions_allows_explicit_path_with_connector_conflict() {
    let alpha = make_skill("alpha-skill", "/tmp/alpha");
    let skills = vec![alpha.clone()];
    let inputs = vec![UserInput::Text {
        text: format!("use {}", linked_skill_mention("alpha-skill", "/tmp/alpha")),
        text_elements: Vec::new(),
    }];
    let connector_counts = HashMap::from([("alpha-skill".to_string(), 1)]);

    let selected = collect_mentions(&inputs, &skills, &HashSet::new(), &connector_counts);

    assert_eq!(selected, vec![alpha]);
}

#[test]
fn collect_explicit_skill_mentions_skips_when_linked_path_disabled() {
    let alpha = make_skill("demo-skill", "/tmp/alpha");
    let beta = make_skill("demo-skill", "/tmp/beta");
    let skills = vec![alpha, beta];
    let inputs = vec![UserInput::Text {
        text: format!("use {}", linked_skill_mention("demo-skill", "/tmp/alpha")),
        text_elements: Vec::new(),
    }];
    let disabled = HashSet::from([test_path_buf("/tmp/alpha").abs()]);
    let connector_counts = HashMap::new();

    let selected = collect_mentions(&inputs, &skills, &disabled, &connector_counts);

    assert_eq!(selected, Vec::new());
}

#[test]
fn collect_explicit_skill_mentions_prefers_resource_path() {
    let alpha = make_skill("demo-skill", "/tmp/alpha");
    let beta = make_skill("demo-skill", "/tmp/beta");
    let skills = vec![alpha, beta.clone()];
    let inputs = vec![UserInput::Text {
        text: format!("use {}", linked_skill_mention("demo-skill", "/tmp/beta")),
        text_elements: Vec::new(),
    }];
    let connector_counts = HashMap::new();

    let selected = collect_mentions(&inputs, &skills, &HashSet::new(), &connector_counts);

    assert_eq!(selected, vec![beta]);
}

#[test]
fn collect_explicit_skill_mentions_skips_missing_path_with_no_fallback() {
    let alpha = make_skill("demo-skill", "/tmp/alpha");
    let beta = make_skill("demo-skill", "/tmp/beta");
    let skills = vec![alpha, beta];
    let inputs = vec![UserInput::Text {
        text: format!("use {}", linked_skill_mention("demo-skill", "/tmp/missing")),
        text_elements: Vec::new(),
    }];
    let connector_counts = HashMap::new();

    let selected = collect_mentions(&inputs, &skills, &HashSet::new(), &connector_counts);

    assert_eq!(selected, Vec::new());
}

#[test]
fn collect_explicit_skill_mentions_skips_missing_path_without_fallback() {
    let alpha = make_skill("demo-skill", "/tmp/alpha");
    let skills = vec![alpha];
    let inputs = vec![UserInput::Text {
        text: format!("use {}", linked_skill_mention("demo-skill", "/tmp/missing")),
        text_elements: Vec::new(),
    }];
    let connector_counts = HashMap::new();

    let selected = collect_mentions(&inputs, &skills, &HashSet::new(), &connector_counts);

    assert_eq!(selected, Vec::new());
}
