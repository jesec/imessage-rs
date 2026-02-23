/// Live integration tests against the real ~/Library/Messages/chat.db.
///
/// These tests require Full Disk Access and are ignored by default.
/// Run with: cargo test --test live_chatdb -- --ignored
use imessage_core::config::AppPaths;
use imessage_db::imessage::repository::MessageRepository;
use imessage_db::imessage::types::*;

fn open_live_db() -> MessageRepository {
    let db_path = AppPaths::imessage_db();
    MessageRepository::open(db_path)
        .expect("Failed to open chat.db — do you have Full Disk Access?")
}

#[test]
#[ignore = "requires Full Disk Access to ~/Library/Messages/chat.db"]
fn can_open_and_detect_schema() {
    let repo = open_live_db();
    let schema = repo.schema();

    // Our select list should have at least 70 columns (may grow for future macOS versions)
    let msg_cols = schema.message_select_columns();
    eprintln!("Detected {} message columns", msg_cols.len());
    assert!(msg_cols.len() >= 70, "Expected at least 70 message columns");
}

#[test]
#[ignore = "requires Full Disk Access"]
fn can_count_messages() {
    let repo = open_live_db();
    let count = repo
        .get_message_count(&MessageCountParams::default())
        .expect("Failed to count messages");
    eprintln!("Total messages: {count}");
    assert!(count > 0, "Expected at least some messages");
}

#[test]
#[ignore = "requires Full Disk Access"]
fn can_count_chats() {
    let repo = open_live_db();
    let count = repo.get_chat_count().expect("Failed to count chats");
    eprintln!("Total chats: {count}");
    assert!(count > 0, "Expected at least some chats");
}

#[test]
#[ignore = "requires Full Disk Access"]
fn can_count_handles() {
    let repo = open_live_db();
    let count = repo
        .get_handle_count(None)
        .expect("Failed to count handles");
    eprintln!("Total handles: {count}");
    assert!(count > 0, "Expected at least some handles");
}

#[test]
#[ignore = "requires Full Disk Access"]
fn can_count_attachments() {
    let repo = open_live_db();
    let count = repo
        .get_attachment_count()
        .expect("Failed to count attachments");
    eprintln!("Total attachments: {count}");
    // Attachments could be 0 in test environments, so just check it doesn't error
}

#[test]
#[ignore = "requires Full Disk Access"]
fn can_query_messages() {
    let repo = open_live_db();
    let params = MessageQueryParams {
        limit: 5,
        with_attachments: false,
        ..Default::default()
    };
    let (messages, total) = repo
        .get_messages(&params)
        .expect("Failed to query messages");
    eprintln!("Got {} messages (total: {total})", messages.len());
    assert!(!messages.is_empty(), "Expected at least some messages");

    // Verify the first message has basic fields populated
    let msg = &messages[0];
    assert!(!msg.guid.is_empty(), "Message should have a GUID");
    assert!(msg.date.is_some(), "Message should have a date");
    eprintln!("First message: GUID={}, date={:?}", msg.guid, msg.date);
}

#[test]
#[ignore = "requires Full Disk Access"]
fn can_query_chats_with_participants() {
    let repo = open_live_db();
    let params = ChatQueryParams {
        limit: Some(3),
        ..Default::default()
    };
    let (chats, total) = repo.get_chats(&params).expect("Failed to query chats");
    eprintln!("Got {} chats (total: {total})", chats.len());
    assert!(!chats.is_empty(), "Expected at least some chats");

    let chat = &chats[0];
    assert!(!chat.guid.is_empty(), "Chat should have a GUID");
    eprintln!(
        "First chat: GUID={}, participants={}",
        chat.guid,
        chat.participants.len()
    );
    // Most chats should have at least one participant
    assert!(
        !chat.participants.is_empty(),
        "Chat should have participants"
    );
}

#[test]
#[ignore = "requires Full Disk Access"]
fn can_query_handles() {
    let repo = open_live_db();
    let params = HandleQueryParams {
        limit: 5,
        ..Default::default()
    };
    let (handles, total) = repo.get_handles(&params).expect("Failed to query handles");
    eprintln!("Got {} handles (total: {total})", handles.len());
    assert!(!handles.is_empty(), "Expected at least some handles");

    let handle = &handles[0];
    assert!(!handle.id.is_empty(), "Handle should have an id");
    eprintln!("First handle: id={}, service={}", handle.id, handle.service);
}

#[test]
#[ignore = "requires Full Disk Access"]
fn can_get_imessage_account() {
    let repo = open_live_db();
    let account = repo
        .get_imessage_account()
        .expect("Failed to get iMessage account");
    eprintln!("iMessage account: {:?}", account);
    // Account might be None if no iMessage chats exist
}

#[test]
#[ignore = "requires Full Disk Access"]
fn message_has_handle_from_join() {
    let repo = open_live_db();
    let params = MessageQueryParams {
        limit: 10,
        with_attachments: false,
        ..Default::default()
    };
    let (messages, _) = repo
        .get_messages(&params)
        .expect("Failed to query messages");

    // Find a message that has a handle_id > 0 (not from me)
    for msg in &messages {
        if msg.handle_id > 0 {
            assert!(
                msg.handle.is_some(),
                "Message with handle_id > 0 should have a handle from JOIN"
            );
            let handle = msg.handle.as_ref().unwrap();
            assert!(!handle.id.is_empty(), "Handle should have an id");
            eprintln!("Message {} has handle: {}", msg.guid, handle.id);
            return;
        }
    }
    eprintln!("No messages with handle_id > 0 found in first 10 results");
}

#[test]
#[ignore = "requires Full Disk Access"]
fn messages_with_attachments() {
    let repo = open_live_db();
    let params = MessageQueryParams {
        limit: 50,
        with_attachments: true,
        ..Default::default()
    };
    let (messages, _) = repo
        .get_messages(&params)
        .expect("Failed to query messages");

    let with_att: Vec<_> = messages
        .iter()
        .filter(|m| !m.attachments.is_empty())
        .collect();

    if !with_att.is_empty() {
        let msg = with_att[0];
        let att = &msg.attachments[0];
        eprintln!(
            "Message {} has {} attachment(s). First: guid={}, mime={:?}, bytes={}",
            msg.guid,
            msg.attachments.len(),
            att.guid,
            att.mime_type,
            att.total_bytes
        );
    } else {
        eprintln!("No messages with attachments found in first 50 results");
    }
}
