use nanoclaw_core::types::ChatInfo;
use nanoclaw_db::NanoClawDb;

pub fn list_groups(db: &NanoClawDb, limit: usize) -> Result<Vec<(String, String)>, String> {
    let mut stmt = db.connection().prepare(
        r#"
        SELECT jid, name
        FROM chats
        WHERE jid LIKE '%@g.us' AND jid <> '__group_sync__' AND name <> jid
        ORDER BY last_message_time DESC
        LIMIT ?1
        "#,
    ).map_err(|err| err.to_string())?;
    let rows = stmt
        .query_map([limit as i64], |row| Ok((row.get(0)?, row.get(1)?)))
        .map_err(|err| err.to_string())?;
    rows.collect::<Result<Vec<_>, _>>().map_err(|err| err.to_string())
}

pub fn group_rows_from_chats(chats: &[ChatInfo], limit: usize) -> Vec<(String, String)> {
    let mut rows = chats
        .iter()
        .filter(|chat| chat.jid.ends_with("@g.us") && chat.jid != "__group_sync__" && chat.name != chat.jid)
        .map(|chat| (chat.jid.clone(), chat.name.clone(), chat.last_message_time.clone()))
        .collect::<Vec<_>>();
    rows.sort_by(|left, right| right.2.cmp(&left.2));
    rows.into_iter().take(limit).map(|(jid, name, _)| (jid, name)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use nanoclaw_core::types::ChatInfo;

    #[test]
    fn filters_and_sorts_group_rows() {
        let chats = vec![
            ChatInfo {
                jid: "group1@g.us".to_string(),
                name: "Group 1".to_string(),
                last_message_time: "2024-01-01T00:00:01.000Z".to_string(),
                channel: Some("whatsapp".to_string()),
                is_group: true,
            },
            ChatInfo {
                jid: "__group_sync__".to_string(),
                name: "__group_sync__".to_string(),
                last_message_time: "2024-01-01T00:00:03.000Z".to_string(),
                channel: None,
                is_group: true,
            },
            ChatInfo {
                jid: "group2@g.us".to_string(),
                name: "Group 2".to_string(),
                last_message_time: "2024-01-01T00:00:02.000Z".to_string(),
                channel: Some("whatsapp".to_string()),
                is_group: true,
            },
        ];

        let rows = group_rows_from_chats(&chats, 10);
        assert_eq!(
            rows,
            vec![
                ("group2@g.us".to_string(), "Group 2".to_string()),
                ("group1@g.us".to_string(), "Group 1".to_string())
            ]
        );
    }
}
