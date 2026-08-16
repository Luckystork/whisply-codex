use super::*;
use pretty_assertions::assert_eq;

fn all_sources_enabled() -> MemoryControls {
    MemoryControls {
        enabled: true,
        learn_automatically: true,
        reference_chat_history: true,
        reference_saved_sessions: true,
        reference_connected_apps: true,
    }
}

fn memory(memory_id: &str, attribution: MemoryAttribution) -> AttributedMemory {
    AttributedMemory {
        memory_id: memory_id.to_string(),
        attribution,
        text: format!("remembered detail from {}", attribution.attribution_id()),
    }
}

fn one_of_each() -> Vec<AttributedMemory> {
    vec![
        memory("m1", MemoryAttribution::UserStated),
        memory("m2", MemoryAttribution::AccountProfile),
        memory("m3", MemoryAttribution::ChatHistory),
        memory("m4", MemoryAttribution::SavedSession),
        memory("m5", MemoryAttribution::ConnectedApp),
    ]
}

#[test]
fn disabled_memory_projects_no_personalization_layer() {
    let controls = MemoryControls::default();

    assert_eq!(
        personalization_contribution(&controls, &one_of_each()),
        Ok(None)
    );
}

#[test]
fn each_source_control_gates_only_its_own_attribution() {
    let controls = MemoryControls {
        enabled: true,
        learn_automatically: false,
        reference_chat_history: false,
        reference_saved_sessions: true,
        reference_connected_apps: false,
    };

    let contribution = personalization_contribution(&controls, &one_of_each())
        .expect("projection")
        .expect("contribution");

    assert_eq!(
        contribution.text,
        "- (user_stated) remembered detail from user_stated\n\
         - (account_profile) remembered detail from account_profile\n\
         - (saved_session) remembered detail from saved_session"
    );
}

#[test]
fn every_projected_item_carries_its_attribution() {
    let contribution = personalization_contribution(&all_sources_enabled(), &one_of_each())
        .expect("projection")
        .expect("contribution");

    for line in contribution.text.lines() {
        assert!(
            line.starts_with("- ("),
            "unattributed personalization line: {line}"
        );
    }
    assert_eq!(contribution.text.lines().count(), 5);
}

#[test]
fn the_capture_control_never_becomes_an_injection_control() {
    let capturing = all_sources_enabled();
    let not_capturing = MemoryControls {
        learn_automatically: false,
        ..capturing
    };

    assert_eq!(
        personalization_contribution(&capturing, &one_of_each()),
        personalization_contribution(&not_capturing, &one_of_each())
    );
}

#[test]
fn the_personalization_layer_can_never_declare_tool_authority() {
    let contribution = personalization_contribution(&all_sources_enabled(), &one_of_each())
        .expect("projection")
        .expect("contribution");

    assert_eq!(contribution.declared_tool_ids, Vec::<String>::new());
    assert_eq!(contribution.layer, PromptLayerKind::PersonalizationMemory);
    assert_eq!(contribution.source, LayerSource::WhisplyProduct);
}

#[test]
fn a_repeated_memory_identifier_is_refused() {
    let mut items = one_of_each();
    items.push(memory("m1", MemoryAttribution::UserStated));

    assert_eq!(
        personalization_contribution(&all_sources_enabled(), &items),
        Err(PersonalizationError::DuplicateMemoryId {
            memory_id: "m1".to_string(),
        })
    );
}

#[test]
fn an_unusable_item_is_refused_before_projection() {
    let items = vec![AttributedMemory {
        memory_id: "m1".to_string(),
        attribution: MemoryAttribution::UserStated,
        text: "   ".to_string(),
    }];

    assert_eq!(
        personalization_contribution(&all_sources_enabled(), &items),
        Err(PersonalizationError::UnusableItem)
    );
}

#[test]
fn admitted_memory_stays_within_its_per_turn_ceilings() {
    let too_many: Vec<AttributedMemory> = (0..=MAX_PERSONALIZATION_ITEMS)
        .map(|index| memory(&format!("m{index}"), MemoryAttribution::UserStated))
        .collect();

    assert_eq!(
        personalization_contribution(&all_sources_enabled(), &too_many),
        Err(PersonalizationError::TooManyItems {
            admitted: MAX_PERSONALIZATION_ITEMS + 1,
        })
    );

    let oversized = vec![AttributedMemory {
        memory_id: "m1".to_string(),
        attribution: MemoryAttribution::UserStated,
        text: "x".repeat(MAX_PERSONALIZATION_BYTES + 1),
    }];

    assert!(matches!(
        personalization_contribution(&all_sources_enabled(), &oversized),
        Err(PersonalizationError::ProjectionTooLarge { .. })
    ));
}
