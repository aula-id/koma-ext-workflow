//! Unit tests for the pure inbox-command builders (ARCHITECTURE.md 6.4). These pin the
//! exact JSON SHAPE each builder emits (op + field names); the cross-crate roundtrip
//! against the real parser lives in `office-daemon/src/inbox_test.rs`.

#[cfg(test)]
mod tests {
    use crate::inboxmsg::{authorize, breakdown, brief, comment, interrupt, resume, status};
    use serde_json::json;

    #[test]
    fn brief_with_project_emits_message_and_project() {
        assert_eq!(
            brief(Some("shop"), "add a cart"),
            json!({ "op": "brief", "message": "add a cart", "project": "shop" })
        );
    }

    #[test]
    fn brief_without_project_omits_the_key() {
        let v = brief(None, "hello");
        assert_eq!(v, json!({ "op": "brief", "message": "hello" }));
        assert!(v.get("project").is_none(), "absent project must not be emitted");
    }

    #[test]
    fn status_scopes_or_omits_project() {
        assert_eq!(status(Some("shop")), json!({ "op": "status", "project": "shop" }));
        let all = status(None);
        assert_eq!(all, json!({ "op": "status" }));
        assert!(all.get("project").is_none());
    }

    #[test]
    fn authorize_emits_project_and_delivery_path() {
        assert_eq!(
            authorize("shop", "/tmp/out"),
            json!({ "op": "authorize", "project": "shop", "delivery_path": "/tmp/out" })
        );
    }

    #[test]
    fn comment_emits_task_and_text() {
        assert_eq!(
            comment("shop/e1/s1/t1", "looks good"),
            json!({ "op": "comment", "task": "shop/e1/s1/t1", "text": "looks good" })
        );
    }

    #[test]
    fn interrupt_maps_hard_flag_to_mode_string() {
        assert_eq!(
            interrupt("shop", true),
            json!({ "op": "interrupt", "project": "shop", "mode": "hard" })
        );
        assert_eq!(
            interrupt("shop", false),
            json!({ "op": "interrupt", "project": "shop", "mode": "soft" })
        );
    }

    #[test]
    fn resume_emits_project() {
        assert_eq!(resume("shop"), json!({ "op": "resume", "project": "shop" }));
    }

    #[test]
    fn breakdown_emits_project() {
        assert_eq!(breakdown("shop"), json!({ "op": "breakdown", "project": "shop" }));
    }
}
