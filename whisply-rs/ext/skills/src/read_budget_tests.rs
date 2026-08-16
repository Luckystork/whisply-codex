use super::MAX_PACKAGE_RESOURCES;
use super::MAX_REFERENCE_DEPTH;
use super::MAX_TURN_READ_BYTES;
use super::MAX_TURN_RESOURCES;
use super::ReadKey;
use super::SkillReadBudget;
use super::referenced_resources;

const TURN: &str = "turn-1";

fn key(resource: &str) -> ReadKey {
    ReadKey::new("orchestrator", "writing-style", resource)
}

fn read(budget: &SkillReadBudget, resource: &str, contents: &str) -> Result<usize, String> {
    let key = key(resource);
    let admitted = budget.admit(TURN, &key, resource == "SKILL.md")?;
    budget.record(TURN, &key, admitted, contents);
    Ok(admitted.depth)
}

/// The walk, not the directory tree, is what depth measures. A package can
/// nest its files however it likes; what costs a turn is how many hops from
/// the skill the model took to get there.
#[test]
fn depth_follows_the_chain_the_model_actually_walked() {
    let budget = SkillReadBudget::default();

    assert_eq!(
        read(&budget, "SKILL.md", "start at [style](style.md)"),
        Ok(0)
    );
    assert_eq!(read(&budget, "style.md", "see [tone](tone.md)"), Ok(1));
    assert_eq!(read(&budget, "tone.md", "see [voice](voice.md)"), Ok(2));
    assert_eq!(
        read(&budget, "voice.md", "see [examples](examples.md)"),
        Ok(3)
    );
    assert_eq!(read(&budget, "examples.md", "see [more](more.md)"), Ok(4));

    let refusal = read(&budget, "more.md", "").expect_err("the fifth hop is over the limit");
    assert!(refusal.contains("more.md"), "{refusal}");
    assert!(
        refusal.contains(&MAX_REFERENCE_DEPTH.to_string()),
        "{refusal}"
    );
}

/// A resource nobody has pointed at is one hop from its package, because that
/// is where the model got the name: the catalog. Treating it as deep would
/// refuse an ordinary first read; treating it as free would let a package hide
/// a chain behind a name the model already knows.
#[test]
fn a_resource_no_page_has_named_is_one_hop_from_its_package() {
    let budget = SkillReadBudget::default();

    assert_eq!(read(&budget, "references/api.md", "see [b](b.md)"), Ok(1));
    assert_eq!(read(&budget, "b.md", ""), Ok(2));
}

/// Two routes to the same file do not make it more expensive; the model is
/// charged for the shortest way it could have reached it.
#[test]
fn a_resource_reachable_two_ways_keeps_its_shortest_distance() {
    let budget = SkillReadBudget::default();

    assert_eq!(
        read(
            &budget,
            "SKILL.md",
            "[deep](a/b/shared.md) and [near](near.md)"
        ),
        Ok(0)
    );
    assert_eq!(read(&budget, "near.md", "[shared](a/b/shared.md)"), Ok(1));
    assert_eq!(read(&budget, "a/b/shared.md", ""), Ok(1));
}

#[test]
fn one_turn_cannot_open_more_resources_than_the_fan_out_limit() {
    let budget = SkillReadBudget::default();
    for index in 0..MAX_PACKAGE_RESOURCES {
        assert!(read(&budget, &format!("reference-{index}.md"), "").is_ok());
    }

    let refusal = read(&budget, "one-more.md", "").expect_err("the package limit holds");
    assert!(refusal.contains("writing-style"), "{refusal}");
    assert!(
        refusal.contains(&MAX_PACKAGE_RESOURCES.to_string()),
        "{refusal}"
    );
}

/// The per-package limit is not the whole story: a turn that spreads its reads
/// across many packages spends the same context window.
#[test]
fn the_turn_limit_holds_across_packages_not_just_within_one() {
    let budget = SkillReadBudget::default();
    let mut opened = 0;
    let mut refusal = None;
    'packages: for package in 0..8 {
        for resource in 0..MAX_PACKAGE_RESOURCES {
            let key = ReadKey::new(
                "orchestrator",
                format!("package-{package}"),
                format!("reference-{resource}.md"),
            );
            match budget.admit(TURN, &key, false) {
                Ok(admitted) => {
                    budget.record(TURN, &key, admitted, "");
                    opened += 1;
                }
                Err(message) => {
                    refusal = Some(message);
                    break 'packages;
                }
            }
        }
    }

    assert_eq!(opened, MAX_TURN_RESOURCES);
    let refusal = refusal.expect("the turn limit holds across packages");
    assert!(
        refusal.contains(&MAX_TURN_RESOURCES.to_string()),
        "{refusal}"
    );
}

/// Paging through something the model is already holding is not new context,
/// so it must not spend the fan-out budget a second time. It does still cost
/// bytes, because every page is returned again.
#[test]
fn paging_a_resource_already_open_is_not_a_new_resource() {
    let budget = SkillReadBudget::default();
    for index in 0..MAX_PACKAGE_RESOURCES {
        assert!(read(&budget, &format!("reference-{index}.md"), "").is_ok());
    }

    assert!(
        read(&budget, "reference-0.md", "next page").is_ok(),
        "a second page of an open resource must not be refused"
    );
}

#[test]
fn a_turn_that_has_spent_its_bytes_is_refused_before_the_provider_is_asked() {
    let budget = SkillReadBudget::default();
    let page = "x".repeat(512 * 1024);
    for index in 0..4 {
        assert!(read(&budget, &format!("reference-{index}.md"), &page).is_ok());
    }

    let refusal = read(&budget, "reference-4.md", "").expect_err("the byte budget holds");
    assert!(
        refusal.contains(&MAX_TURN_READ_BYTES.to_string()),
        "{refusal}"
    );
}

/// The budget belongs to a turn. The next turn starts with a full one, or a
/// long conversation would slowly lose the ability to read its own skills.
#[test]
fn a_new_turn_starts_with_a_full_budget() {
    let budget = SkillReadBudget::default();
    for index in 0..MAX_PACKAGE_RESOURCES {
        let opened = key(&format!("reference-{index}.md"));
        let admitted = budget
            .admit(TURN, &opened, false)
            .expect("within the limit");
        budget.record(TURN, &opened, admitted, "");
    }

    let next = key("reference.md");
    assert!(budget.admit(TURN, &next, false).is_err());
    assert!(budget.admit("turn-2", &next, false).is_ok());
}

/// Recording a reference has to be cheap to be wrong in one direction only: a
/// name that is never read costs nothing, while a missed reference lets a
/// chain continue uncounted. Nothing here grants access — the resolver's own
/// containment check still decides that.
#[test]
fn references_are_found_in_the_shapes_a_skill_actually_writes_them() {
    let found = referenced_resources(
        "See [the style sheet](references/style.md) and `scripts/run.sh`.\n\
         Read ./tone.md first, then examples.json.\n\
         Ignore https://example.com/page.md and the word markdown.",
    );

    assert!(
        found.contains(&"references/style.md".to_string()),
        "{found:?}"
    );
    assert!(found.contains(&"scripts/run.sh".to_string()), "{found:?}");
    assert!(found.contains(&"tone.md".to_string()), "{found:?}");
    assert!(found.contains(&"examples.json".to_string()), "{found:?}");
    assert!(
        !found.iter().any(|candidate| candidate.contains("://")),
        "a link off the machine is not a resource of this package: {found:?}"
    );
    assert!(!found.contains(&"markdown".to_string()), "{found:?}");
}
