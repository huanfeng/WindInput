use super::*;
use crate::comment_lib::CommentState;

#[test]
fn parses_comment_state_verbs() {
    for v in [
        "comment_state",
        "comment_state:detail",
        "comment_state:default",
    ] {
        assert!(BoundAction::parse(v).is_enabled(), "{v} 两张表都应认");
        assert!(SessionAction::parse(v).is_enabled(), "{v} 两张表都应认");
        assert_eq!(SessionAction::parse(v).to_string(), v);
    }
    assert_eq!(
        BoundAction::parse("comment_state:detail"),
        BoundAction::CommentState(CommentStateAction::Set(CommentState::Detail))
    );
    assert_eq!(
        SessionAction::parse("comment_state"),
        SessionAction::CommentState(CommentStateAction::Toggle)
    );
    // session 档只属于会话表。
    assert_eq!(
        BoundAction::parse("comment_state:session"),
        BoundAction::None
    );
    assert!(!BoundAction::parse("comment_state:session").is_enabled());
    assert_eq!(
        SessionAction::parse("comment_state:session"),
        SessionAction::CommentState(CommentStateAction::Session)
    );
    assert_eq!(
        SessionAction::parse("comment_state:session").to_string(),
        "comment_state:session"
    );
    // 写错的载荷不静默降级。
    assert_eq!(BoundAction::parse("comment_state:yes"), BoundAction::None);
    assert_eq!(
        SessionAction::parse("comment_state:yes"),
        SessionAction::None
    );
}

#[test]
fn only_session_payload_requires_candidates() {
    assert!(SessionAction::parse("comment_state:session").requires_candidates());
    assert!(!SessionAction::parse("comment_state").requires_candidates());
    assert!(!SessionAction::parse("comment_state:detail").requires_candidates());
}
