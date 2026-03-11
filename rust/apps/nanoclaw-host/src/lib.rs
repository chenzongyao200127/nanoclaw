pub mod ipc;
pub mod queue;

use std::collections::{HashMap, HashSet};

use nanoclaw_core::types::{AvailableGroup, ChatInfo, RegisteredGroup};

pub fn get_available_groups(
    chats: &[ChatInfo],
    registered_groups: &HashMap<String, RegisteredGroup>,
) -> Vec<AvailableGroup> {
    let registered = registered_groups.keys().collect::<HashSet<_>>();
    let mut groups = chats
        .iter()
        .filter(|chat| chat.jid != "__group_sync__" && chat.is_group)
        .map(|chat| AvailableGroup {
            jid: chat.jid.clone(),
            name: chat.name.clone(),
            last_activity: chat.last_message_time.clone(),
            is_registered: registered.contains(&chat.jid),
        })
        .collect::<Vec<_>>();

    groups.sort_by(|left, right| right.last_activity.cmp(&left.last_activity));
    groups
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chat(jid: &str, name: &str, last_message_time: &str, is_group: bool) -> ChatInfo {
        ChatInfo {
            jid: jid.to_string(),
            name: name.to_string(),
            last_message_time: last_message_time.to_string(),
            channel: Some("whatsapp".to_string()),
            is_group,
        }
    }

    #[test]
    fn returns_only_groups() {
        let chats = vec![
            chat("group1@g.us", "Group 1", "2024-01-01T00:00:01.000Z", true),
            chat(
                "user@s.whatsapp.net",
                "User DM",
                "2024-01-01T00:00:02.000Z",
                false,
            ),
            chat("group2@g.us", "Group 2", "2024-01-01T00:00:03.000Z", true),
        ];

        let groups = get_available_groups(&chats, &HashMap::new());
        assert_eq!(groups.len(), 2);
        assert!(groups.iter().any(|group| group.jid == "group1@g.us"));
        assert!(groups.iter().any(|group| group.jid == "group2@g.us"));
        assert!(!groups.iter().any(|group| group.jid == "user@s.whatsapp.net"));
    }

    #[test]
    fn excludes_group_sync_sentinel() {
        let chats = vec![
            chat("__group_sync__", "__group_sync__", "2024-01-01T00:00:00.000Z", true),
            chat("group@g.us", "Group", "2024-01-01T00:00:01.000Z", true),
        ];

        let groups = get_available_groups(&chats, &HashMap::new());
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].jid, "group@g.us");
    }

    #[test]
    fn marks_registered_groups() {
        let chats = vec![
            chat("reg@g.us", "Registered", "2024-01-01T00:00:01.000Z", true),
            chat("unreg@g.us", "Unregistered", "2024-01-01T00:00:02.000Z", true),
        ];
        let registered_groups = HashMap::from([(
            "reg@g.us".to_string(),
            RegisteredGroup {
                name: "Registered".to_string(),
                folder: "registered".to_string(),
                trigger: "@Andy".to_string(),
                added_at: "2024-01-01T00:00:00.000Z".to_string(),
                container_config: None,
                requires_trigger: None,
                is_main: None,
            },
        )]);

        let groups = get_available_groups(&chats, &registered_groups);
        assert_eq!(
            groups.iter().find(|group| group.jid == "reg@g.us").map(|group| group.is_registered),
            Some(true)
        );
        assert_eq!(
            groups
                .iter()
                .find(|group| group.jid == "unreg@g.us")
                .map(|group| group.is_registered),
            Some(false)
        );
    }

    #[test]
    fn sorts_by_recent_activity() {
        let chats = vec![
            chat("old@g.us", "Old", "2024-01-01T00:00:01.000Z", true),
            chat("new@g.us", "New", "2024-01-01T00:00:05.000Z", true),
            chat("mid@g.us", "Mid", "2024-01-01T00:00:03.000Z", true),
        ];

        let groups = get_available_groups(&chats, &HashMap::new());
        assert_eq!(groups[0].jid, "new@g.us");
        assert_eq!(groups[1].jid, "mid@g.us");
        assert_eq!(groups[2].jid, "old@g.us");
    }

    #[test]
    fn returns_empty_when_no_groups_exist() {
        let groups = get_available_groups(&[], &HashMap::new());
        assert!(groups.is_empty());
    }
}
