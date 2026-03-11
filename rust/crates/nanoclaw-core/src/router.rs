use regex::Regex;

use crate::timezone::format_local_time;
use crate::types::NewMessage;

pub fn escape_xml(input: &str) -> String {
    let mut escaped = String::with_capacity(input.len());
    for ch in input.chars() {
        match ch {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '"' => escaped.push_str("&quot;"),
            _ => escaped.push(ch),
        }
    }
    escaped
}

pub fn format_messages(messages: &[NewMessage], timezone: &str) -> String {
    let header = format!(r#"<context timezone="{}" />"#, escape_xml(timezone));
    let body = messages
        .iter()
        .map(|message| {
            let display_time = format_local_time(&message.timestamp, timezone);
            format!(
                r#"<message sender="{}" time="{}">{}</message>"#,
                escape_xml(&message.sender_name),
                escape_xml(&display_time),
                escape_xml(&message.content)
            )
        })
        .collect::<Vec<_>>()
        .join("\n");

    format!("{header}\n<messages>\n{body}\n</messages>")
}

pub fn strip_internal_tags(text: &str) -> String {
    let regex = Regex::new(r"(?s)<internal>.*?</internal>").expect("internal tag regex");
    regex.replace_all(text, "").trim().to_string()
}

pub fn format_outbound(raw_text: &str) -> String {
    strip_internal_tags(raw_text)
}

pub fn trigger_matches(text: &str, assistant_name: &str) -> bool {
    let trimmed = text.trim();
    let prefix = format!("@{assistant_name}");
    if !trimmed
        .get(..prefix.len())
        .map(|candidate| candidate.eq_ignore_ascii_case(&prefix))
        .unwrap_or(false)
    {
        return false;
    }

    match trimmed[prefix.len()..].chars().next() {
        None => true,
        Some(ch) => !ch.is_ascii_alphanumeric() && ch != '_',
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_msg(
        sender_name: &str,
        content: &str,
        timestamp: &str,
    ) -> NewMessage {
        NewMessage {
            id: "1".to_string(),
            chat_jid: "group@g.us".to_string(),
            sender: "123@s.whatsapp.net".to_string(),
            sender_name: sender_name.to_string(),
            content: content.to_string(),
            timestamp: timestamp.to_string(),
            is_from_me: None,
            is_bot_message: None,
        }
    }

    #[test]
    fn escapes_xml_content() {
        assert_eq!(escape_xml(r#"a & b < c > d "e""#), "a &amp; b &lt; c &gt; d &quot;e&quot;");
        assert_eq!(escape_xml(""), "");
    }

    #[test]
    fn formats_messages_as_xml() {
        let actual = format_messages(&[make_msg("Alice", "hello", "2024-01-01T00:00:00.000Z")], "UTC");
        assert!(actual.contains(r#"<context timezone="UTC" />"#));
        assert!(actual.contains(r#"<message sender="Alice" time="Jan 1, 2024, 12:00 AM">hello</message>"#));
    }

    #[test]
    fn escapes_sender_and_content_in_messages() {
        let actual = format_messages(
            &[make_msg("A & B <Co>", r#"<script>alert("xss")</script>"#, "2024-01-01T00:00:00.000Z")],
            "UTC",
        );
        assert!(actual.contains(r#"sender="A &amp; B &lt;Co&gt;""#));
        assert!(actual.contains("&lt;script&gt;alert(&quot;xss&quot;)&lt;/script&gt;"));
    }

    #[test]
    fn handles_empty_messages() {
        let actual = format_messages(&[], "UTC");
        assert_eq!(actual, "<context timezone=\"UTC\" />\n<messages>\n\n</messages>");
    }

    #[test]
    fn strips_internal_tags() {
        assert_eq!(
            strip_internal_tags("hello <internal>secret</internal> world"),
            "hello  world"
        );
        assert_eq!(
            strip_internal_tags("<internal>a</internal>hello<internal>b</internal>"),
            "hello"
        );
        assert_eq!(strip_internal_tags("<internal>only this</internal>"), "");
    }

    #[test]
    fn formats_outbound_text() {
        assert_eq!(format_outbound("hello world"), "hello world");
        assert_eq!(format_outbound("<internal>hidden</internal>"), "");
        assert_eq!(
            format_outbound("<internal>thinking</internal>The answer is 42"),
            "The answer is 42"
        );
    }

    #[test]
    fn matches_trigger_prefix() {
        assert!(trigger_matches("@Andy hello", "Andy"));
        assert!(trigger_matches("@andy hello", "Andy"));
        assert!(trigger_matches("@ANDY hello", "Andy"));
        assert!(trigger_matches("@Andy's thing", "Andy"));
        assert!(trigger_matches("@Andy", "Andy"));
        assert!(!trigger_matches("hello @Andy", "Andy"));
        assert!(!trigger_matches("@Andyextra hello", "Andy"));
    }
}
