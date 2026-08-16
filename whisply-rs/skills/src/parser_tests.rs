use pretty_assertions::assert_eq;

use super::ParsedSkillFrontmatter;
use super::parse_skill_frontmatter_metadata;

#[test]
fn parses_repairs_and_sanitizes_frontmatter() {
    let parsed = parse_skill_frontmatter_metadata(
        "---\nname:  deploy  service\ndescription: Build for AWS: ECS\nmetadata:\n  short-description:  Deploy   safely\n---\n",
        || "fallback".to_string(),
    )
    .expect("valid frontmatter");

    assert_eq!(
        parsed,
        ParsedSkillFrontmatter {
            name: "deploy service".to_string(),
            description: "Build for AWS: ECS".to_string(),
            short_description: Some("Deploy safely".to_string()),
            version: None,
        }
    );
}

#[test]
fn uses_default_name_and_requires_description() {
    let parsed = parse_skill_frontmatter_metadata("---\ndescription: Demo skill\n---\n", || {
        "demo".to_string()
    })
    .expect("valid frontmatter");
    assert_eq!(
        parsed,
        ParsedSkillFrontmatter {
            name: "demo".to_string(),
            description: "Demo skill".to_string(),
            short_description: None,
            version: None,
        }
    );

    let error =
        parse_skill_frontmatter_metadata("---\nname: demo\n---\n", || "fallback".to_string())
            .expect_err("description should be required");
    assert_eq!(error.to_string(), "missing field `description`");
}

/// Packages have always been allowed to declare a version and it has always
/// been discarded, so every surface reported "no version" for packages that
/// plainly stated one.
#[test]
fn reads_a_declared_version() {
    let parsed = parse_skill_frontmatter_metadata(
        "---\nname: demo\ndescription: Demo skill\nversion: 1.4.2-beta\n---\n",
        || "fallback".to_string(),
    )
    .expect("valid frontmatter");
    assert_eq!(parsed.version.as_deref(), Some("1.4.2-beta"));
}

/// YAML types a bare `1.0` as a float and a bare date as a date. Reading only
/// strings would report no version for packages that are perfectly explicit.
#[test]
fn reads_versions_yaml_does_not_hand_back_as_strings() {
    for (frontmatter, expected) in [
        ("version: 1.0", "1.0"),
        ("version: 3", "3"),
        ("version: \"2026-01-05\"", "2026-01-05"),
    ] {
        let parsed = parse_skill_frontmatter_metadata(
            &format!("---\nname: demo\ndescription: Demo skill\n{frontmatter}\n---\n"),
            || "fallback".to_string(),
        )
        .expect("valid frontmatter");
        assert_eq!(parsed.version.as_deref(), Some(expected), "{frontmatter}");
    }
}

/// A package that declared nothing must stay distinguishable from one that
/// declared an empty value, because a client renders the two differently.
#[test]
fn an_absent_or_empty_version_is_reported_as_absent() {
    for frontmatter in ["", "version: \"\"", "version:", "version: \"   \""] {
        let parsed = parse_skill_frontmatter_metadata(
            &format!("---\nname: demo\ndescription: Demo skill\n{frontmatter}\n---\n"),
            || "fallback".to_string(),
        )
        .expect("valid frontmatter");
        assert_eq!(parsed.version, None, "{frontmatter:?}");
    }
}

/// Version is presentation metadata. A skill whose version is unusable must
/// still load, or a cosmetic field becomes a way to break a working package.
#[test]
fn an_unusable_version_does_not_make_the_skill_unloadable() {
    let long = "9".repeat(500);
    let parsed = parse_skill_frontmatter_metadata(
        &format!("---\nname: demo\ndescription: Demo skill\nversion: \"{long}\"\n---\n"),
        || "fallback".to_string(),
    )
    .expect("an over-long version must not fail the load");
    assert_eq!(parsed.version, None);

    let structured = parse_skill_frontmatter_metadata(
        "---\nname: demo\ndescription: Demo skill\nversion:\n  major: 1\n---\n",
        || "fallback".to_string(),
    )
    .expect("a structured version must not fail the load");
    assert_eq!(structured.version, None);
    assert_eq!(structured.description, "Demo skill");
}
